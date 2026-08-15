use super::{NarrationFailure, NarrationValue};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

const NARRATION_CACHE_CAPACITY: usize = 64;

/// Successful narration is disposable presentation state. The key includes the
/// graph generation and exact packet hash; no project knowledge is cached here.
#[derive(Default)]
pub struct StoryNarrationCache {
    inner: Mutex<NarrationCacheInner>,
}

#[derive(Default)]
struct NarrationCacheInner {
    completed: HashMap<String, Arc<NarrationValue>>,
    lru: VecDeque<String>,
    flights: HashMap<String, Arc<NarrationFlight>>,
}

pub(super) struct NarrationFlight {
    result: Mutex<Option<Result<Arc<NarrationValue>, NarrationFailure>>>,
    notify: tokio::sync::Notify,
}

impl NarrationFlight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        }
    }

    pub(super) async fn wait(&self) -> Result<Arc<NarrationValue>, NarrationFailure> {
        loop {
            let notified = self.notify.notified();
            if let Some(result) = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }
}

pub(super) enum CacheStart {
    Hit(Arc<NarrationValue>),
    Leader(Arc<NarrationFlight>),
    Follower(Arc<NarrationFlight>),
}

impl StoryNarrationCache {
    pub(super) fn begin(&self, key: &str) -> CacheStart {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(value) = inner.completed.get(key).cloned() {
            touch_lru(&mut inner.lru, key);
            return CacheStart::Hit(value);
        }
        if let Some(flight) = inner.flights.get(key) {
            return CacheStart::Follower(flight.clone());
        }
        let flight = Arc::new(NarrationFlight::new());
        inner.flights.insert(key.to_string(), flight.clone());
        CacheStart::Leader(flight)
    }

    pub(super) fn finish(
        &self,
        key: &str,
        flight: &Arc<NarrationFlight>,
        result: Result<Arc<NarrationValue>, NarrationFailure>,
    ) {
        *flight
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result.clone());
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.flights.remove(key);
        if let Ok(value) = result {
            inner.completed.insert(key.to_string(), value);
            touch_lru(&mut inner.lru, key);
            while inner.completed.len() > NARRATION_CACHE_CAPACITY {
                if let Some(oldest) = inner.lru.pop_front() {
                    inner.completed.remove(&oldest);
                }
            }
        }
        drop(inner);
        flight.notify.notify_waiters();
    }
}

fn touch_lru(lru: &mut VecDeque<String>, key: &str) {
    if let Some(index) = lru.iter().position(|candidate| candidate == key) {
        lru.remove(index);
    }
    lru.push_back(key.to_string());
}

#[cfg(test)]
mod cache_tests {
    use super::super::NarrationFailureReason;
    use super::*;

    fn value() -> Arc<NarrationValue> {
        Arc::new(NarrationValue { narrative: vec![] })
    }

    #[tokio::test]
    async fn cache_coalesces_success_and_does_not_cache_failure() {
        let cache = StoryNarrationCache::default();
        let CacheStart::Leader(success_leader) = cache.begin("success") else {
            panic!("first request must lead");
        };
        let CacheStart::Follower(success_follower) = cache.begin("success") else {
            panic!("second request must follow");
        };
        cache.finish("success", &success_leader, Ok(value()));
        assert!(success_follower.wait().await.is_ok());
        assert!(matches!(cache.begin("success"), CacheStart::Hit(_)));

        let CacheStart::Leader(failure_leader) = cache.begin("failure") else {
            panic!("first failure request must lead");
        };
        cache.finish(
            "failure",
            &failure_leader,
            Err(NarrationFailure::invalid(
                NarrationFailureReason::InvalidJson,
                "invalid_json",
            )),
        );
        assert!(matches!(cache.begin("failure"), CacheStart::Leader(_)));
    }

    #[test]
    fn cache_is_lru_bounded() {
        let cache = StoryNarrationCache::default();
        for index in 0..=NARRATION_CACHE_CAPACITY {
            let key = format!("key-{index}");
            let CacheStart::Leader(flight) = cache.begin(&key) else {
                panic!("new key must lead");
            };
            cache.finish(&key, &flight, Ok(value()));
        }
        assert!(matches!(cache.begin("key-0"), CacheStart::Leader(_)));
        assert!(matches!(
            cache.begin(&format!("key-{NARRATION_CACHE_CAPACITY}")),
            CacheStart::Hit(_)
        ));
    }
}

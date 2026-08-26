use super::super::model::NarrationOutcome;
use super::{NarrationFailure, NarrationValue};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Bounds completed presentation-only prose in memory; active flights are tracked separately.
const NARRATION_CACHE_CAPACITY: usize = 64;

/// Successful narration is disposable presentation state. The key includes the
/// graph generation and exact packet hash; no project knowledge is cached here.
#[derive(Default)]
pub struct StoryNarrationCache {
    inner: Arc<Mutex<NarrationCacheInner>>,
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
    Leader(FlightLease),
    Follower(Arc<NarrationFlight>),
}

/// A leader's claim on one narration flight.
///
/// Resolving the flight is tied to this value's LIFETIME rather than to the
/// leader reaching the end of its code path. An HTTP client that disconnects
/// mid-narration cancels the handler future, so the leader is simply dropped
/// between starting the provider call and recording its result. Without the
/// drop below, that flight stays in `flights` forever holding `None`, and every
/// later request for the same subject coalesces onto a flight nobody is driving
/// — waiting on a `Notify` that can never fire. It presents as a permanent,
/// subject-specific hang that survives until the daemon restarts.
pub(super) struct FlightLease {
    inner: Arc<Mutex<NarrationCacheInner>>,
    key: String,
    flight: Arc<NarrationFlight>,
    resolved: bool,
}

impl FlightLease {
    pub(super) fn finish(mut self, result: Result<Arc<NarrationValue>, NarrationFailure>) {
        self.publish(result);
        self.resolved = true;
    }

    fn publish(&self, result: Result<Arc<NarrationValue>, NarrationFailure>) {
        *self
            .flight
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result.clone());
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.flights.remove(&self.key);
        if let Ok(value) = result {
            inner.completed.insert(self.key.clone(), value);
            touch_lru(&mut inner.lru, &self.key);
            while inner.completed.len() > NARRATION_CACHE_CAPACITY {
                if let Some(oldest) = inner.lru.pop_front() {
                    inner.completed.remove(&oldest);
                }
            }
        }
        drop(inner);
        self.flight.notify.notify_waiters();
    }
}

impl Drop for FlightLease {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        // Nothing is driving this flight any more. Wake the followers so they
        // fall back to the symbolic article, and leave no entry behind: the
        // next request becomes a fresh leader and genuinely retries.
        self.publish(Err(NarrationFailure {
            outcome: NarrationOutcome::ProviderError,
            reason: None,
            category: "abandoned",
        }));
    }
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
        drop(inner);
        CacheStart::Leader(FlightLease {
            inner: Arc::clone(&self.inner),
            key: key.to_string(),
            flight,
            resolved: false,
        })
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
        success_leader.finish(Ok(value()));
        assert!(success_follower.wait().await.is_ok());
        assert!(matches!(cache.begin("success"), CacheStart::Hit(_)));

        let CacheStart::Leader(failure_leader) = cache.begin("failure") else {
            panic!("first failure request must lead");
        };
        failure_leader.finish(Err(NarrationFailure::invalid(
            NarrationFailureReason::InvalidJson,
            "invalid_json",
        )));
        assert!(matches!(cache.begin("failure"), CacheStart::Leader(_)));
    }

    #[test]
    fn cache_is_lru_bounded() {
        let cache = StoryNarrationCache::default();
        for index in 0..=NARRATION_CACHE_CAPACITY {
            let key = format!("key-{index}");
            let CacheStart::Leader(lease) = cache.begin(&key) else {
                panic!("new key must lead");
            };
            lease.finish(Ok(value()));
        }
        assert!(matches!(cache.begin("key-0"), CacheStart::Leader(_)));
        assert!(matches!(
            cache.begin(&format!("key-{NARRATION_CACHE_CAPACITY}")),
            CacheStart::Hit(_)
        ));
    }

    #[tokio::test]
    async fn an_abandoned_leader_does_not_strand_its_followers() {
        // The leader is dropped without finishing — exactly what happens when
        // an HTTP client disconnects mid-narration and its handler future is
        // cancelled. Before the lease, the flight stayed in the map forever and
        // every later request for the same key waited on it indefinitely.
        let cache = StoryNarrationCache::default();
        let CacheStart::Leader(leader) = cache.begin("abandoned") else {
            panic!("first request must lead");
        };
        let CacheStart::Follower(follower) = cache.begin("abandoned") else {
            panic!("second request must follow");
        };

        drop(leader);

        // The follower resolves rather than hanging...
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), follower.wait())
            .await
            .expect("a follower must not wait on a flight nobody is driving");
        assert!(result.is_err(), "an abandoned flight yields no narration");

        // ...and the abandonment is not cached, so the next caller retries.
        assert!(matches!(cache.begin("abandoned"), CacheStart::Leader(_)));
    }
}

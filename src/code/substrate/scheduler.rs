//! Debounced save-triggered substrate rebuilds.
//!
//! The Knowledge-LSP posts a nudge for every qualifying `textDocument/didSave`;
//! this scheduler coalesces bursts into one full `run_index` run on a dedicated
//! worker thread. Delivery needs no extra wiring: publishing a new generation
//! changes the manifest identity, `AppState::substrate()` reloads it lazily,
//! and each LSP session's tick republishes re-anchored diagnostics.
//!
//! Single-flight is structural (only the worker thread calls the runner), and
//! `index.lock` in `run_index` remains the cross-process guard against hook or
//! manual runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::producer::{detected_save_extensions, run_index};

/// Trailing debounce: a rebuild starts this long after the last qualifying save.
pub const DEBOUNCE_QUIET: Duration = Duration::from_secs(15);
/// Backstop: under continuous saves, a rebuild starts no later than this long
/// after the first save it will service.
pub const BACKSTOP_MAX_WAIT: Duration = Duration::from_secs(300);
/// Minimum spacing between rebuild attempts after a failed run.
pub const FAILURE_COOLDOWN: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
pub struct SchedulerConfig {
    pub quiet: Duration,
    pub max_wait: Duration,
    pub failure_cooldown: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            quiet: DEBOUNCE_QUIET,
            max_wait: BACKSTOP_MAX_WAIT,
            failure_cooldown: FAILURE_COOLDOWN,
        }
    }
}

/// One full substrate rebuild. Production wiring is `producer::run_index`;
/// tests inject a stub.
pub type IndexRunner = Box<dyn FnMut() -> anyhow::Result<()> + Send>;
/// True when a saved repo-relative path could shift indexed positions.
pub type SaveFilter = Box<dyn Fn(&str) -> bool + Send>;

enum Event {
    Save(String),
    Shutdown,
}

/// The armed save burst awaiting a rebuild.
struct Window {
    first: Instant,
    last: Instant,
}

pub struct ReindexScheduler {
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ReindexScheduler {
    pub fn spawn(config: SchedulerConfig, qualifies: SaveFilter, runner: IndexRunner) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("MooseDevReindexScheduler".to_string())
            .spawn(move || debounce_loop(&rx, config, qualifies, runner, &worker_stop))
            .expect("spawn reindex scheduler thread");
        Self {
            tx,
            stop,
            worker: Some(worker),
        }
    }

    /// Cheap clonable nudge entry point; send failures (scheduler gone) are
    /// deliberately ignored — a lost nudge only delays freshness.
    pub fn nudger(&self) -> Box<dyn Fn(&str) + Send + Sync> {
        let tx = self.tx.clone();
        Box::new(move |rel_path: &str| {
            let _ = tx.send(Event::Save(rel_path.to_string()));
        })
    }

    /// Stop scheduling new runs and detach the worker. Shutdown never joins:
    /// deciding whether a worker is idle from a sampled flag creates an
    /// unavoidable false-idle window immediately before the runner starts.
    /// The stop flag prevents an uncommitted run from starting; a run that has
    /// already committed finishes detached. Atomic publication keeps readers
    /// isolated, and the next successful run cleans any interrupted generation.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Event::Shutdown);
        drop(self.worker.take());
    }
}

fn debounce_loop(
    rx: &std::sync::mpsc::Receiver<Event>,
    config: SchedulerConfig,
    qualifies: SaveFilter,
    mut runner: IndexRunner,
    stop: &AtomicBool,
) {
    let mut pending: Option<Window> = None;
    let mut earliest_next_run: Option<Instant> = None;
    loop {
        match &mut pending {
            None => match rx.recv() {
                Ok(Event::Save(path)) => {
                    if qualifies(&path) {
                        let now = Instant::now();
                        pending = Some(Window {
                            first: now,
                            last: now,
                        });
                    }
                }
                Ok(Event::Shutdown) | Err(_) => return,
            },
            Some(window) => {
                let mut fire_at = (window.last + config.quiet).min(window.first + config.max_wait);
                if let Some(earliest) = earliest_next_run {
                    fire_at = fire_at.max(earliest);
                }
                let now = Instant::now();
                if now < fire_at {
                    match rx.recv_timeout(fire_at - now) {
                        Ok(Event::Save(path)) => {
                            if qualifies(&path) {
                                // `first` stays put: that is what makes the
                                // backstop a hard cap under continuous saves.
                                window.last = Instant::now();
                            }
                            continue;
                        }
                        Ok(Event::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
                // This is the run's commit point. Shutdown sets `stop` before
                // detaching the worker, so a not-yet-committed run is canceled.
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let result = runner();
                pending = None;
                match result {
                    Ok(()) => earliest_next_run = None,
                    Err(error) => {
                        earliest_next_run = Some(Instant::now() + config.failure_cooldown);
                        tracing::warn!(
                            "save-triggered substrate rebuild failed (next attempt in {}s): {error:#}",
                            config.failure_cooldown.as_secs()
                        );
                    }
                }
                // Saves that arrived during the run are queued in the channel;
                // the next recv() arms a fresh window immediately.
            }
        }
    }
}

/// Production wiring for the serve path. `None` when the env kill-switch is
/// set or no producer detects the repository (non-code repos, synthetic test
/// roots) — the nudger then stays uninstalled and saves are no-ops.
pub fn spawn_save_reindex(repo_root: PathBuf, data_dir: PathBuf) -> Option<ReindexScheduler> {
    if reindex_on_save_disabled() {
        tracing::info!("save-triggered reindex disabled via MOOSEDEV_NO_REINDEX_ON_SAVE");
        return None;
    }
    let extensions = detected_save_extensions(&repo_root);
    if extensions.is_empty() {
        tracing::info!(
            "save-triggered reindex not armed: no SCIP producer detects {}",
            repo_root.display()
        );
        return None;
    }
    let qualifies: SaveFilter = Box::new(move |rel_path: &str| {
        Path::new(rel_path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| extensions.contains(&ext))
    });
    let runner: IndexRunner = Box::new(move || {
        let report = run_index(&repo_root, &data_dir)?;
        tracing::info!(
            commit = %report.commit,
            duration_s = report.duration.as_secs_f64(),
            "save-triggered substrate rebuild published"
        );
        Ok(())
    });
    Some(ReindexScheduler::spawn(
        SchedulerConfig::default(),
        qualifies,
        runner,
    ))
}

fn reindex_on_save_disabled() -> bool {
    std::env::var_os("MOOSEDEV_NO_REINDEX_ON_SAVE")
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| !value.is_empty() && value != "0")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn config(quiet_ms: u64, max_wait_ms: u64, cooldown_ms: u64) -> SchedulerConfig {
        SchedulerConfig {
            quiet: Duration::from_millis(quiet_ms),
            max_wait: Duration::from_millis(max_wait_ms),
            failure_cooldown: Duration::from_millis(cooldown_ms),
        }
    }

    fn recording_runner(runs: Arc<Mutex<Vec<Instant>>>) -> IndexRunner {
        Box::new(move || {
            runs.lock().unwrap().push(Instant::now());
            Ok(())
        })
    }

    fn accept_rs() -> SaveFilter {
        Box::new(|path| path.ends_with(".rs"))
    }

    fn wait_for_runs(runs: &Arc<Mutex<Vec<Instant>>>, count: usize, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if runs.lock().unwrap().len() >= count {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        runs.lock().unwrap().len() >= count
    }

    #[test]
    fn trailing_debounce_coalesces_burst_into_one_run() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let scheduler = ReindexScheduler::spawn(
            config(50, 5_000, 5_000),
            accept_rs(),
            recording_runner(runs.clone()),
        );
        let nudge = scheduler.nudger();
        let last_nudge = {
            nudge("src/a.rs");
            std::thread::sleep(Duration::from_millis(20));
            nudge("src/b.rs");
            Instant::now()
        };
        assert!(wait_for_runs(&runs, 1, Duration::from_secs(2)));
        // One run for the burst, starting only after the quiet window.
        std::thread::sleep(Duration::from_millis(120));
        let recorded = runs.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0] >= last_nudge + Duration::from_millis(50));
        drop(recorded);
        scheduler.shutdown();
    }

    #[test]
    fn non_qualifying_saves_never_arm() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let scheduler = ReindexScheduler::spawn(
            config(20, 100, 100),
            accept_rs(),
            recording_runner(runs.clone()),
        );
        let nudge = scheduler.nudger();
        nudge("README.md");
        nudge("docs/notes.txt");
        std::thread::sleep(Duration::from_millis(200));
        assert!(runs.lock().unwrap().is_empty());
        scheduler.shutdown();
    }

    #[test]
    fn backstop_fires_under_continuous_saves() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let scheduler = ReindexScheduler::spawn(
            config(100, 250, 5_000),
            accept_rs(),
            recording_runner(runs.clone()),
        );
        let nudge = scheduler.nudger();
        let first = Instant::now();
        // Saves every 25ms: the 100ms quiet window never elapses.
        while Instant::now() < first + Duration::from_millis(600) {
            nudge("src/a.rs");
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(wait_for_runs(&runs, 1, Duration::from_secs(2)));
        let started = runs.lock().unwrap()[0];
        assert!(started >= first + Duration::from_millis(250));
        assert!(started <= first + Duration::from_millis(600));
        scheduler.shutdown();
    }

    #[test]
    fn single_flight_and_rearm_after_mid_run_saves() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let in_run = Arc::new(AtomicBool::new(false));
        let overlapped = Arc::new(AtomicBool::new(false));
        let runner: IndexRunner = {
            let runs = runs.clone();
            let in_run = in_run.clone();
            let overlapped = overlapped.clone();
            Box::new(move || {
                if in_run.swap(true, Ordering::SeqCst) {
                    overlapped.store(true, Ordering::SeqCst);
                }
                runs.lock().unwrap().push(Instant::now());
                std::thread::sleep(Duration::from_millis(150));
                in_run.store(false, Ordering::SeqCst);
                Ok(())
            })
        };
        let scheduler = ReindexScheduler::spawn(config(30, 5_000, 5_000), accept_rs(), runner);
        let nudge = scheduler.nudger();
        nudge("src/a.rs");
        assert!(wait_for_runs(&runs, 1, Duration::from_secs(2)));
        // Saves landing while the first run executes must coalesce into
        // exactly one follow-up run.
        nudge("src/b.rs");
        nudge("src/c.rs");
        assert!(wait_for_runs(&runs, 2, Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(runs.lock().unwrap().len(), 2);
        assert!(!overlapped.load(Ordering::SeqCst));
        scheduler.shutdown();
    }

    #[test]
    fn failure_cooldown_prevents_hot_retry() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let runner: IndexRunner = {
            let runs = runs.clone();
            Box::new(move || {
                runs.lock().unwrap().push(Instant::now());
                anyhow::bail!("producer unavailable")
            })
        };
        let scheduler = ReindexScheduler::spawn(config(20, 5_000, 300), accept_rs(), runner);
        let nudge = scheduler.nudger();
        nudge("src/a.rs");
        assert!(wait_for_runs(&runs, 1, Duration::from_secs(2)));
        let first_failure = runs.lock().unwrap()[0];
        // Keep saving: the cooldown, not the quiet window, must gate retries.
        for _ in 0..20 {
            nudge("src/a.rs");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(wait_for_runs(&runs, 2, Duration::from_secs(2)));
        let second_attempt = runs.lock().unwrap()[1];
        assert!(second_attempt >= first_failure + Duration::from_millis(300));
        scheduler.shutdown();
    }

    #[test]
    fn shutdown_does_not_join_worker_during_run_transition() {
        // Hold the worker after it accepts a save but before it can commit to
        // the runner. This deterministically exercises the false-idle window
        // that made a sampled-state join block (or deadlock) during shutdown.
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let qualifies: SaveFilter = Box::new(move |path| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            path.ends_with(".rs")
        });
        let (ran_tx, ran_rx) = std::sync::mpsc::channel();
        let runner: IndexRunner = Box::new(move || {
            ran_tx.send(()).unwrap();
            Ok(())
        });
        let scheduler = ReindexScheduler::spawn(config(0, 100, 100), qualifies, runner);
        let nudge = scheduler.nudger();
        nudge("src/a.rs");
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let started = Instant::now();
        scheduler.shutdown();
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "shutdown blocked for {:?}",
            started.elapsed()
        );

        release_tx.send(()).unwrap();
        assert_eq!(
            ran_rx.recv_timeout(Duration::from_secs(1)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            "shutdown must cancel a run that has not reached its commit point"
        );
    }

    #[test]
    fn shutdown_returns_immediately_when_idle_or_running() {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let idle = ReindexScheduler::spawn(
            config(20, 100, 100),
            accept_rs(),
            recording_runner(runs.clone()),
        );
        let started = Instant::now();
        idle.shutdown();
        assert!(started.elapsed() < Duration::from_millis(100));

        let slow_runs = Arc::new(Mutex::new(Vec::new()));
        let runner: IndexRunner = {
            let slow_runs = slow_runs.clone();
            Box::new(move || {
                slow_runs.lock().unwrap().push(Instant::now());
                std::thread::sleep(Duration::from_millis(500));
                Ok(())
            })
        };
        let busy = ReindexScheduler::spawn(config(10, 100, 100), accept_rs(), runner);
        let nudge = busy.nudger();
        nudge("src/a.rs");
        assert!(wait_for_runs(&slow_runs, 1, Duration::from_secs(2)));
        let started = Instant::now();
        busy.shutdown();
        assert!(started.elapsed() < Duration::from_millis(200));
    }
}

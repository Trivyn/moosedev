//! Crash evidence for `moosedev code`. The harness died mid-request in badciv
//! task c83c10f8 (2026-09-27, during a planning request) and left nothing on
//! disk: `code` skips the binary's tracing setup, and a panic printed only to
//! the terminal the TUI was drawing on. Everything here appends to
//! `.moosedev/harness/crash.log`: panics with a backtrace, fatal errors, the
//! signals that end a session and, on the next start, a session that ended
//! with none of those (SIGKILL, abort, power loss), detected by the
//! per-process `session-<pid>.json` marker a clean exit removes.
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
};

const LOG: &str = "crash.log";
/// Per-process session markers are `session-<pid>.json`.
const MARKER_PREFIX: &str = "session-";
/// The one marker per project that versions before per-process markers wrote.
const LEGACY_MARKER: &str = "session.json";

/// What an entry names besides its event. Written by the main flow, read by
/// the panic hook; every critical section only clones or assigns, so a panic
/// can never occur while it is held and the hook cannot deadlock on it.
struct State {
    root: Option<PathBuf>,
    command: String,
    task: Option<String>,
    /// When the running interactive session started, while its marker exists.
    session: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    root: None,
    command: String::new(),
    task: None,
    session: None,
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A `session-<pid>.json` marker: which process owns an interactive session and
/// what it was doing, so the next start can name what a silent death took.
#[derive(Serialize, Deserialize)]
struct Marker {
    pid: u32,
    task: Option<String>,
    started: String,
    command: String,
}

/// Direct this process's crash evidence to `root` and chain a panic hook in
/// front of the current one. The TUI's terminal-restoring hook is installed
/// later and chains this one, so a panic restores the terminal, is logged,
/// then prints as usual.
pub fn install(root: &Path, command: String) {
    {
        let mut state = state();
        state.root = Some(root.to_path_buf());
        state.command = command;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log_panic(info);
        previous(info);
    }));
}

fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(non-string panic payload)");
    let location = info
        .location()
        .map_or_else(|| "unknown".into(), ToString::to_string);
    let thread = std::thread::current();
    let detail = format!(
        "message: {message}\nlocation: {location}\nbacktrace:\n{}",
        std::backtrace::Backtrace::force_capture()
    );
    log_entry(
        &format!(
            "panicked on thread '{}'",
            thread.name().unwrap_or("unnamed")
        ),
        &detail,
    );
}

/// Append an event (a fatal error, the signal that ended a session) to the
/// installed crash log. A no-op before [`install`] or in a project that has no
/// `.moosedev` directory yet; never fails, since it runs on the way out.
pub fn log(event: &str) {
    log_entry(event, "");
}

fn log_entry(headline: &str, detail: &str) {
    let (root, command, task) = {
        let state = state();
        (
            state.root.clone(),
            state.command.clone(),
            state.task.clone(),
        )
    };
    let Some(root) = root else { return };
    let mut entry = format!(
        "[{}] pid {} {headline}\ncommand: {command}\ntask: {}\n",
        now(),
        std::process::id(),
        task.as_deref().unwrap_or("none"),
    );
    if !detail.is_empty() {
        entry.push_str(detail.trim_end());
        entry.push('\n');
    }
    append(&root, &entry);
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Name the task this process is working on, for the panic report and the
/// session marker. Called wherever a runner is created or loaded.
pub fn record_task(id: &str) {
    // Unit tests create runners concurrently in one process; recording them
    // would race the tests below that assert the recorded task.
    #[cfg(not(test))]
    set_task(id);
    #[cfg(test)]
    let _ = id;
}

fn set_task(id: &str) {
    let (root, session) = {
        let mut state = state();
        state.task = Some(id.to_string());
        (state.root.clone(), state.session.clone())
    };
    if let (Some(root), Some(started)) = (root, session) {
        write_marker(&root, &started);
    }
}

/// `.moosedev/harness`, created if missing. Nothing is created in a project
/// without `.moosedev`: that directory is what marks it initialized. Neither
/// level may be a symlink, as for task and conversation storage.
fn directory(root: &Path) -> Option<PathBuf> {
    let data = root.join(".moosedev");
    let real = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir() && !meta.is_symlink())
    };
    if !real(&data) {
        return None;
    }
    let harness = data.join("harness");
    if !harness.exists() {
        let _ = std::fs::create_dir(&harness);
    }
    real(&harness).then_some(harness)
}

fn append(root: &Path, entry: &str) {
    let Some(directory) = directory(root) else {
        return;
    };
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    if let Ok(mut file) = options.open(directory.join(LOG)) {
        let _ = file.write_all(entry.as_bytes());
    }
}

/// Open a new file readable only by its owner, refusing one that already
/// exists and, on unix, a symlink in its place.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// `session-<pid>.json`: one marker per process, so concurrent interfaces in
/// one project never overwrite each other's claim.
fn marker_name(pid: u32) -> String {
    format!("{MARKER_PREFIX}{pid}.json")
}

fn write_marker(root: &Path, started: &str) {
    let Some(directory) = directory(root) else {
        return;
    };
    let (command, task) = {
        let state = state();
        (state.command.clone(), state.task.clone())
    };
    let pid = std::process::id();
    let marker = Marker {
        pid,
        task,
        started: started.to_string(),
        command,
    };
    let Ok(bytes) = serde_json::to_vec_pretty(&marker) else {
        return;
    };
    // Replace by rename so a death mid-write cannot leave a torn marker. A
    // temporary left by a death mid-write (ours, or a dead process that had
    // this pid) is removed first; removal unlinks a symlink without following
    // it, and the exclusive create refuses anything placed there since.
    let name = marker_name(pid);
    let temporary = directory.join(format!(".{name}.tmp"));
    let _ = std::fs::remove_file(&temporary);
    let written = create_private(&temporary).and_then(|mut file| file.write_all(&bytes));
    if written.is_err() || std::fs::rename(&temporary, directory.join(name)).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

/// An interactive session's `session-<pid>.json`, removed when this is
/// dropped: on a normal return, an error return through `cli::main`, a
/// handled signal, or a panic that unwinds (which the hook has already logged).
pub struct Session {
    root: PathBuf,
}

/// Mark the start of an interactive session in the installed project.
pub fn begin_session() -> Option<Session> {
    let started = now();
    let root = {
        let mut state = state();
        let root = state.root.clone()?;
        state.session = Some(started.clone());
        root
    };
    write_marker(&root, &started);
    Some(Session { root })
}

impl Drop for Session {
    fn drop(&mut self) {
        state().session = None;
        // Only this process's marker; other sessions keep their claims.
        if let Some(directory) = directory(&self.root) {
            let _ = std::fs::remove_file(directory.join(marker_name(std::process::id())));
        }
    }
}

/// Report, once each, every interactive session in the installed project that
/// ended without a clean exit: its marker remains and its process is gone.
/// Each report is appended to the crash log and its marker removed.
pub fn previous_sessions() -> Vec<String> {
    let Some(root) = state().root.clone() else {
        return Vec::new();
    };
    previous_sessions_at(&root)
        .into_iter()
        .map(|report| format!("{report}; see .moosedev/harness/{LOG}"))
        .collect()
}

fn previous_sessions_at(root: &Path) -> Vec<String> {
    let Some(directory) = directory(root) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    // Per-process markers, and the single `session.json` an earlier version
    // wrote. Only regular files: a symlink is neither read nor followed.
    let mut markers: Vec<(String, Option<u32>)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name == LEGACY_MARKER {
                return Some((name, None));
            }
            let pid = name
                .strip_prefix(MARKER_PREFIX)?
                .strip_suffix(".json")?
                .parse()
                .ok()?;
            Some((name, Some(pid)))
        })
        .collect();
    markers.sort();
    let mut reports = Vec::new();
    for (name, named_pid) in markers {
        let path = directory.join(&name);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let report = match serde_json::from_slice::<Marker>(&bytes) {
            Ok(marker) if gone(marker.pid) => format!(
                "previous session {} ended without a clean exit {} (started {})",
                marker.pid,
                marker.task.map_or_else(
                    || "with no task open".into(),
                    |task| format!("while working on task {task}")
                ),
                marker.started,
            ),
            Ok(_) => continue,
            Err(_) => match named_pid {
                Some(pid) if !gone(pid) => continue,
                Some(pid) => format!(
                    "previous session {pid} ended without a clean exit and left an unreadable {name}"
                ),
                None => format!(
                    "previous session ended without a clean exit and left an unreadable {name}"
                ),
            },
        };
        append(root, &format!("[{}] {report}\n", now()));
        let _ = std::fs::remove_file(path);
        reports.push(report);
    }
    reports
}

/// Whether a marker's process is over: not this process, and not running.
fn gone(pid: u32) -> bool {
    pid != std::process::id() && !alive(pid)
}

/// Whether a process exists. PID reuse can make a dead session look alive;
/// that costs one missed report, never a false one.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 delivers nothing; it only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn alive(_: u32) -> bool {
    true
}

/// Resolves with the name of the first SIGHUP or SIGTERM. Registering replaces
/// their default action (immediate termination, which skips the terminal
/// restore and the marker removal), so the interface can log and exit cleanly.
#[cfg(unix)]
pub fn termination() -> std::io::Result<impl std::future::Future<Output = &'static str>> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            _ = hangup.recv() => "SIGHUP",
            _ = terminate.recv() => "SIGTERM",
        }
    })
}

#[cfg(not(unix))]
pub fn termination() -> std::io::Result<impl std::future::Future<Output = &'static str>> {
    Ok(std::future::pending())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hook, the installed root and the recorded task are process-global.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn project() -> PathBuf {
        let root = std::env::temp_dir().join(format!("moosedev-crash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".moosedev")).unwrap();
        root
    }

    fn crash_log(root: &Path) -> String {
        std::fs::read_to_string(root.join(".moosedev/harness").join(LOG)).unwrap_or_default()
    }

    #[test]
    fn a_panic_is_logged_with_its_location_and_task() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let root = project();
        let previous = std::panic::take_hook();
        install(&root, "moosedev code test".into());
        set_task("c83c10f8-0000-4000-8000-000000000000");
        let line = line!() + 1;
        let result = std::panic::catch_unwind(|| panic!("model request lost"));
        let _ = std::panic::take_hook();
        std::panic::set_hook(previous);
        {
            let mut state = state();
            state.root = None;
            state.task = None;
        }
        assert!(result.is_err());
        let log = crash_log(&root);
        assert!(log.contains("panicked on thread"), "{log}");
        assert!(log.contains("message: model request lost"), "{log}");
        assert!(
            log.contains(&format!("location: src/harness/crash.rs:{line}")),
            "{log}"
        );
        assert!(
            log.contains("task: c83c10f8-0000-4000-8000-000000000000"),
            "{log}"
        );
        assert!(log.contains("command: moosedev code test"), "{log}");
        assert!(log.contains("backtrace:"), "{log}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fatal_errors_append_and_uninitialized_projects_stay_untouched() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let root = project();
        state().root = Some(root.clone());
        log("exited with an error: first");
        log("exited with an error: second");
        let log_text = crash_log(&root);
        assert!(log_text.contains("first") && log_text.contains("second"));
        assert!(log_text.find("first") < log_text.find("second"));

        let bare = std::env::temp_dir().join(format!("moosedev-crash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&bare).unwrap();
        state().root = Some(bare.clone());
        log("exited with an error: nowhere to go");
        assert!(
            !bare.join(".moosedev").exists(),
            "logging must not initialize a project"
        );
        state().root = None;
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(bare).unwrap();
    }

    /// A pid whose process has exited and been reaped.
    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn marker(pid: u32, task: &str) -> Vec<u8> {
        serde_json::to_vec(&Marker {
            pid,
            task: Some(task.into()),
            started: "2026-09-27T00:30:00.000Z".into(),
            command: "moosedev code".into(),
        })
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_dead_session_is_reported_once_and_live_ones_are_not() {
        let root = project();
        let harness = root.join(".moosedev/harness");
        std::fs::create_dir(&harness).unwrap();
        let dead = dead_pid();
        let live = std::os::unix::process::parent_id();
        let ours = std::process::id();
        std::fs::write(harness.join(marker_name(dead)), marker(dead, "c83c10f8")).unwrap();
        std::fs::write(harness.join(marker_name(live)), marker(live, "5e11")).unwrap();
        std::fs::write(harness.join(marker_name(ours)), marker(ours, "0a45")).unwrap();

        let reports = previous_sessions_at(&root);
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert!(reports[0].contains(&format!("previous session {dead} ended without a clean exit while working on task c83c10f8 (started 2026-09-27T00:30:00.000Z)")), "{reports:?}");
        assert!(crash_log(&root).contains(&reports[0]));
        assert!(
            !harness.join(marker_name(dead)).exists(),
            "the dead marker is consumed"
        );
        assert!(
            harness.join(marker_name(live)).exists(),
            "a live session is left alone"
        );
        assert!(
            harness.join(marker_name(ours)).exists(),
            "this process's own marker is not reported"
        );
        assert!(previous_sessions_at(&root).is_empty(), "reported once");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn every_dead_session_and_a_legacy_marker_are_reported() {
        let root = project();
        let harness = root.join(".moosedev/harness");
        std::fs::create_dir(&harness).unwrap();
        let (first, second) = (dead_pid(), dead_pid());
        std::fs::write(harness.join(marker_name(first)), marker(first, "aaaa")).unwrap();
        std::fs::write(harness.join(marker_name(second)), b"{torn").unwrap();
        let legacy = dead_pid();
        std::fs::write(harness.join(LEGACY_MARKER), marker(legacy, "1e9a")).unwrap();

        let reports = previous_sessions_at(&root);
        assert_eq!(reports.len(), 3, "{reports:?}");
        let log = crash_log(&root);
        for report in &reports {
            assert!(log.contains(report), "{log}");
        }
        assert!(reports.iter().any(|r| r.contains(&format!(
            "previous session {first} ended without a clean exit while working on task aaaa"
        ))));
        assert!(reports.iter().any(|r| r.contains(&format!(
            "previous session {second} ended without a clean exit and left an unreadable {}",
            marker_name(second)
        ))));
        assert!(reports.iter().any(|r| r.contains(&format!(
            "previous session {legacy} ended without a clean exit while working on task 1e9a"
        ))));
        assert!(!harness.join(LEGACY_MARKER).exists());
        assert!(previous_sessions_at(&root).is_empty(), "reported once");

        // A legacy marker whose process still runs (an older harness) stays.
        let live = std::os::unix::process::parent_id();
        std::fs::write(harness.join(LEGACY_MARKER), marker(live, "1e9a")).unwrap();
        assert!(previous_sessions_at(&root).is_empty());
        assert!(harness.join(LEGACY_MARKER).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_session_marker_follows_the_task_and_a_clean_exit_removes_only_it() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let root = project();
        state().root = Some(root.clone());
        let harness = root.join(".moosedev/harness");
        let marker_path = harness.join(marker_name(std::process::id()));
        let session = begin_session().unwrap();
        let read =
            || serde_json::from_slice::<Marker>(&std::fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(read().pid, std::process::id());
        set_task("7a1d");
        assert_eq!(read().task.as_deref(), Some("7a1d"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&marker_path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Another interface in the same project keeps its claim.
        let other = harness.join(marker_name(u32::MAX));
        std::fs::write(&other, marker(u32::MAX, "5e11")).unwrap();
        drop(session);
        assert!(!marker_path.exists());
        assert!(other.exists(), "a clean exit removes only its own marker");
        {
            let mut state = state();
            state.root = None;
            state.task = None;
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_temporary_is_never_followed() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let root = project();
        let harness = root.join(".moosedev/harness");
        std::fs::create_dir(&harness).unwrap();
        let victim = root.join("victim");
        std::fs::write(&victim, "untouched").unwrap();
        let temporary = harness.join(format!(".{}.tmp", marker_name(std::process::id())));
        std::os::unix::fs::symlink(&victim, &temporary).unwrap();
        assert!(
            create_private(&temporary).is_err(),
            "an existing symlink is refused"
        );

        state().root = Some(root.clone());
        write_marker(&root, "2026-09-27T00:30:00.000Z");
        state().root = None;
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(!temporary.exists(), "the temporary is renamed into place");
        let written = harness.join(marker_name(std::process::id()));
        assert!(!std::fs::symlink_metadata(&written).unwrap().is_symlink());
        let marker: Marker = serde_json::from_slice(&std::fs::read(&written).unwrap()).unwrap();
        assert_eq!(marker.pid, std::process::id());
        std::fs::remove_dir_all(root).unwrap();
    }
}

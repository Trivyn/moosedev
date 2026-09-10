//! Request accounting is an append-only sidecar, never project evidence. The
//! shared snapshot makes cancellation receipts visible even when a model future
//! is dropped before the runner can save its ordinary task journal.
use crate::harness::progress::{Progress, ProgressSender};
use crate::llm::{RequestStatus, RequestUsage, UsageObserver};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot {
    schema: u32,
    legacy_gap: bool,
    requests: Vec<RequestUsage>,
    persistence_errors: Vec<String>,
}

#[derive(Debug)]
struct State {
    snapshot: Snapshot,
    file: Option<File>,
}

#[derive(Debug, Clone)]
pub struct UsageLedger(Arc<Mutex<State>>);

impl Default for UsageLedger {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            snapshot: Snapshot {
                schema: 1,
                legacy_gap: true,
                requests: Vec::new(),
                persistence_errors: Vec::new(),
            },
            file: None,
        })))
    }
}

impl Serialize for UsageLedger {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UsageLedger {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(Arc::new(Mutex::new(State {
            snapshot: Snapshot::deserialize(deserializer)?,
            file: None,
        }))))
    }
}

impl UsageLedger {
    pub(super) fn new(journal: &Path) -> Self {
        let ledger = Self::default();
        ledger.0.lock().unwrap().snapshot.legacy_gap = false;
        ledger.attach(journal);
        ledger
    }

    /// Replay under the runner's existing workspace lease. A request whose final
    /// receipt was lost remains Started/unknown; never infer zero consumption.
    pub(super) fn attach(&self, journal: &Path) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Err(error) = state.attach(journal) {
            state.gap(format!("Token accounting sidecar unavailable: {error}"));
        }
    }

    pub(super) fn observer(&self, progress: Option<ProgressSender>) -> UsageObserver {
        let ledger = self.clone();
        Arc::new(move |receipt| {
            let mut state = ledger.0.lock().unwrap_or_else(|error| error.into_inner());
            state.merge(receipt.clone());
            let result = (|| -> anyhow::Result<()> {
                let file = state
                    .file
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("sidecar is unavailable"))?;
                serde_json::to_writer(&mut *file, &receipt)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                Ok(())
            })();
            if let Err(error) = result {
                state.file = None;
                // Accounting must not turn a valid action into a repair/retry.
                let message = format!("Token accounting persistence gap: {error}");
                state.gap(message.clone());
                if let Some(progress) = &progress {
                    let _ = progress.send(Progress::Status(message));
                }
            }
        })
    }
}

impl State {
    fn gap(&mut self, message: String) {
        if !self.snapshot.persistence_errors.contains(&message) {
            self.snapshot.persistence_errors.push(message);
        }
    }

    fn merge(&mut self, receipt: RequestUsage) {
        if let Some(prior) = self
            .snapshot
            .requests
            .iter_mut()
            .find(|value| value.id == receipt.id)
        {
            // Journal may already contain a terminal receipt newer than the
            // first (Started) sidecar line. Replaying must not regress it.
            if receipt.status != RequestStatus::Started || prior.status == RequestStatus::Started {
                *prior = receipt;
            }
        } else {
            self.snapshot.requests.push(receipt);
        }
    }

    fn attach(&mut self, journal: &Path) -> anyhow::Result<()> {
        let path = journal.with_extension("usage.jsonl");
        let mut options = OpenOptions::new();
        options.read(true).append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // NONBLOCK prevents an attacker-controlled FIFO from hanging open.
            options
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .mode(0o600);
        }
        let mut file = options.open(&path)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(metadata.is_file(), "usage sidecar must be a regular file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            anyhow::ensure!(
                metadata.nlink() == 1
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.mode() & 0o777 == 0o600,
                "usage sidecar must be owned, single-link, and mode 0600"
            );
        }
        for (line, bytes) in BufReader::new(&file).split(b'\n').enumerate() {
            match bytes.and_then(|bytes| {
                serde_json::from_slice::<RequestUsage>(&bytes).map_err(std::io::Error::other)
            }) {
                Ok(receipt) => self.merge(receipt),
                Err(_) => self.gap(format!(
                    "Unreadable token accounting receipt at line {}",
                    line + 1
                )),
            }
        }
        // An interrupted append may leave a partial final line. Keep it as audit
        // evidence, and delimit future valid receipts instead of joining them.
        if metadata.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        file.sync_all()?;
        File::open(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("sidecar has no parent"))?,
        )?
        .sync_all()?;
        self.file = Some(file);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("moosedev-usage-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn receipt(id: &str, status: &str) -> RequestUsage {
        serde_json::from_value(json!({
            "id":id,"model":"fixture","endpoint":"http://127.0.0.1/v1/chat/completions",
            "streaming":true,"context":{"purpose":"harness_action","decision_id":"decision","candidate":2},
            "started_at":"2026-09-09T00:00:00Z","finished_at":null,"elapsed_ms":null,
            "status":status,"http_status":200,"raw_usage":null,"tokens":{},"response_complete":false
        })).unwrap()
    }

    fn value(ledger: &UsageLedger) -> Value {
        serde_json::to_value(ledger).unwrap()
    }

    #[test]
    fn sidecar_recovers_requests_after_stale_journal_without_double_counting() {
        let dir = Directory::new();
        let journal = dir.path().join("task.json");
        let ledger = UsageLedger::new(&journal);
        let stale = serde_json::to_vec(&ledger).unwrap();
        let observer = ledger.observer(None);
        observer(receipt("one", "started"));
        observer(receipt("one", "completed"));
        observer(receipt("two", "started"));
        observer(receipt("two", "cancelled"));
        observer(receipt("three", "started"));
        let resumed: UsageLedger = serde_json::from_slice(&stale).unwrap();
        resumed.attach(&journal);
        assert_eq!(value(&resumed)["requests"].as_array().unwrap().len(), 3);
        assert_eq!(value(&resumed)["requests"][0]["status"], "completed");
        assert_eq!(value(&resumed)["requests"][1]["status"], "cancelled");
        assert_eq!(value(&resumed)["requests"][2]["status"], "started");
        assert!(value(&resumed)["requests"][2]["tokens"]["prompt_tokens"].is_null());
        let latest: UsageLedger = serde_json::from_value(value(&resumed)).unwrap();
        latest.attach(&journal);
        assert_eq!(value(&latest), value(&resumed));
        assert_eq!(
            std::fs::read_to_string(journal.with_extension("usage.jsonl"))
                .unwrap()
                .lines()
                .count(),
            5
        );
    }

    #[test]
    fn clones_share_cancellation_and_legacy_is_not_complete_zero() {
        let dir = Directory::new();
        let ledger = UsageLedger::new(&dir.path().join("task.json"));
        let snapshot = ledger.clone();
        ledger.observer(None)(receipt("cancelled", "cancelled"));
        assert_eq!(value(&snapshot)["requests"][0]["status"], "cancelled");
        assert_eq!(value(&snapshot)["legacy_gap"], false);
        assert_eq!(value(&UsageLedger::default())["legacy_gap"], true);
    }

    #[test]
    fn interrupted_sidecar_keeps_valid_receipts_and_records_a_gap() {
        let dir = Directory::new();
        let journal = dir.path().join("task.json");
        let ledger = UsageLedger::new(&journal);
        ledger.observer(None)(receipt("one", "started"));
        let mut file = OpenOptions::new()
            .append(true)
            .open(journal.with_extension("usage.jsonl"))
            .unwrap();
        file.write_all(b"{\"id\":").unwrap();
        let resumed: UsageLedger = serde_json::from_value(value(&ledger)).unwrap();
        resumed.attach(&journal);
        resumed.observer(None)(receipt("two", "completed"));
        let reloaded: UsageLedger = serde_json::from_value(value(&resumed)).unwrap();
        reloaded.attach(&journal);
        assert_eq!(value(&reloaded)["requests"].as_array().unwrap().len(), 2);
        assert_eq!(
            value(&reloaded)["persistence_errors"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_sidecars_become_explicit_gaps_without_touching_targets() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = Directory::new();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"untouched").unwrap();
        for kind in ["symlink", "hardlink", "permissions", "fifo"] {
            let journal = dir.path().join(format!("{kind}.json"));
            let sidecar = journal.with_extension("usage.jsonl");
            match kind {
                "symlink" => symlink(&victim, &sidecar).unwrap(),
                "hardlink" => std::fs::hard_link(&victim, &sidecar).unwrap(),
                "permissions" => {
                    std::fs::write(&sidecar, b"").unwrap();
                    std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                _ => {
                    use std::os::unix::ffi::OsStrExt;
                    let path = std::ffi::CString::new(sidecar.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                }
            }
            let ledger = UsageLedger::new(&journal);
            ledger.observer(None)(receipt(kind, "completed"));
            assert!(!value(&ledger)["persistence_errors"]
                .as_array()
                .unwrap()
                .is_empty());
            assert_eq!(value(&ledger)["requests"].as_array().unwrap().len(), 1);
        }
        assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
    }
}

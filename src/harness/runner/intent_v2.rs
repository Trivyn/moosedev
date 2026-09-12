//! The approved change scope and the diff geometry that feeds association
//! derivation. Daemon contracts are validated here at task creation.
use super::*;
use crate::harness::protocol::{ChangedFile, HarnessSourcePosition, HarnessSourceRange};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedChangeScope {
    pub version: u8,
    pub knowledge_revision: String,
    pub files: BTreeMap<String, Option<String>>,
    pub obligation_iris: Vec<String>,
    #[serde(default)]
    pub definition_scopes: Vec<ApprovedDefinitionScope>,
    pub checks: Vec<String>,
    pub approval_cycle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedDefinitionScope {
    pub file: String,
    pub symbol: String,
    pub source_digest: String,
}

pub(super) fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(hash(&serde_json::to_string(value)?))
}

impl Runner {
    pub(super) fn validate_daemon_contracts(context: &ContextResponse) -> Result<()> {
        anyhow::ensure!(
            context.capture_contracts.contains(&2),
            "project daemon does not advertise capture contract v2; upgrade the daemon before creating a new harness task"
        );
        anyhow::ensure!(
            context.intent_contracts.contains(&2),
            "project daemon does not advertise intent contract v2; upgrade the daemon before creating a new harness task"
        );
        Ok(())
    }
}

pub(super) fn changed_files(edits: &[PendingEdit]) -> Result<Vec<ChangedFile>> {
    let mut chains: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for edit in edits {
        chains
            .entry(edit.file.clone())
            .and_modify(|(_, after)| {
                *after = edit.after.clone();
            })
            .or_insert_with(|| (edit.before.clone(), edit.after.clone()));
    }
    Ok(chains
        .into_iter()
        .map(|(file, (before, after))| ChangedFile {
            file,
            before_digest: fingerprint(&before),
            after_digest: fingerprint(&after),
            changed_ranges: vec![changed_range(before.as_deref(), after.as_deref())],
        })
        .collect())
}

fn changed_range(before: Option<&str>, after: Option<&str>) -> HarnessSourceRange {
    let source = after.or(before).unwrap_or_default();
    let other = if after.is_some() {
        before.unwrap_or_default()
    } else {
        ""
    };
    changed_range_between(source, other)
}

fn changed_range_between(source: &str, other: &str) -> HarnessSourceRange {
    let mut prefix = source
        .bytes()
        .zip(other.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !source.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = source[prefix..]
        .bytes()
        .rev()
        .zip(
            other.as_bytes()[prefix.min(other.len())..]
                .iter()
                .rev()
                .copied(),
        )
        .take_while(|(a, b)| a == b)
        .count();
    while suffix > 0 && !source.is_char_boundary(source.len() - suffix) {
        suffix -= 1;
    }
    let position = |offset: usize| {
        let head = &source[..offset.min(source.len())];
        let line = head.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let col = head
            .rsplit_once('\n')
            .map_or(head.len(), |(_, tail)| tail.len()) as u32;
        HarnessSourcePosition { line, col }
    };
    HarnessSourceRange {
        start: position(prefix),
        end: position(source.len().saturating_sub(suffix)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_edits_are_rebased_to_one_original_to_final_range() {
        let edits = vec![
            PendingEdit {
                file: "code.py".into(),
                before: Some("abc\n".into()),
                after: Some("abXc\n".into()),
                reason: String::new(),
                revision: "r1".into(),
            },
            PendingEdit {
                file: "code.py".into(),
                before: Some("abXc\n".into()),
                after: Some("QabXc\n".into()),
                reason: String::new(),
                revision: "r1".into(),
            },
        ];
        let changed = changed_files(&edits).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].before_digest, fingerprint(&Some("abc\n".into())));
        assert_eq!(
            changed[0].after_digest,
            fingerprint(&Some("QabXc\n".into()))
        );
        assert_eq!(changed[0].changed_ranges.len(), 1);
        assert_eq!(changed[0].changed_ranges[0].start.line, 0);
        assert_eq!(changed[0].changed_ranges[0].start.col, 0);
        assert_eq!(changed[0].changed_ranges[0].end.line, 0);
        assert_eq!(changed[0].changed_ranges[0].end.col, 4);
    }
}

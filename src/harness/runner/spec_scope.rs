//! Spec scope: which part of the covered scope each spec record governs.
//!
//! A specification that governs a whole project usually also names its parts
//! (crates, packages, modules), and most of its records govern one part.
//! Anchoring them all to the covered scope made every one of them reach every
//! file (badciv: 33 records on every root file). Two sensor calls separate
//! them: the first lists the parts the specification names, the second gives
//! every record exactly one part or `whole`. Leaving a record out of every
//! part let the model skip the decision (4 of 35 assigned in replay); asking
//! for an answer per record assigned all 35, identically on three runs.
//! Everything the sensor says is checked here before the daemon plans it, and
//! you approve the result in the spec preview.
use super::model::InvalidModelOutput;
use super::*;
use crate::harness::protocol::{SpecComponentGroup, SpecPartDraft, SpecRecordDraft};
use serde::Deserialize;
use serde_json::json;

pub(super) const SPEC_PARTS_PURPOSE: &str = "harness_spec_parts";
pub(super) const SPEC_SCOPE_PURPOSE: &str = "harness_spec_scope";
/// Records classified per scope call.
const SCOPE_BATCH: usize = 40;
const MAX_PARTS: usize = 16;
const WHOLE: &str = "whole";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartsProposal {
    action: String,
    parts: Vec<ProposedPart>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedPart {
    name: String,
    path: String,
    stated_by: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeProposal {
    action: String,
    assignments: Vec<Assignment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Assignment {
    record: usize,
    part: String,
}

/// The scope a spec's records were given.
pub(super) enum SpecScope {
    /// Parts to send with the preparation (possibly none).
    Parts(Vec<SpecPartDraft>),
    /// The sensor's answers failed validation three times; every record
    /// governs the approval's own component. Carries the last diagnostic.
    Failed(String),
}

impl Runner {
    /// The parts of the covered scope and the records each governs.
    pub(super) async fn scope_spec(
        &mut self,
        path: &str,
        covers: &[String],
        records: &[SpecRecordDraft],
    ) -> Result<SpecScope> {
        if let Some(progress) = &self.progress {
            let _ = progress.send(crate::harness::progress::Progress::Status(format!(
                "Scoping {path}: finding the parts it names…"
            )));
        }
        let main = main_component_name(path, covers);
        let parts = loop {
            match self.propose_parts(path, covers, records, &main).await {
                Ok(parts) => {
                    self.candidate_accepted();
                    break parts;
                }
                Err(error) => {
                    if !self.repair_candidate(&error)? {
                        return self.scoping_failed(path, error);
                    }
                }
            }
        };
        if parts.is_empty() {
            self.intent_event("spec_parts", &format!("{path}: no parts named"));
            return Ok(SpecScope::Parts(Vec::new()));
        }
        let mut assigned: Vec<Option<usize>> = vec![None; records.len()];
        for start in (0..records.len()).step_by(SCOPE_BATCH) {
            let end = (start + SCOPE_BATCH).min(records.len());
            if let Some(progress) = &self.progress {
                let _ = progress.send(crate::harness::progress::Progress::Status(format!(
                    "Scoping {path}: records {}-{} of {}…",
                    start + 1,
                    end,
                    records.len()
                )));
            }
            let batch = loop {
                match self
                    .assign_parts(path, covers, &parts, records, start, end)
                    .await
                {
                    Ok(batch) => {
                        self.candidate_accepted();
                        break batch;
                    }
                    Err(error) => {
                        if !self.repair_candidate(&error)? {
                            return self.scoping_failed(path, error);
                        }
                    }
                }
            };
            assigned[start..end].copy_from_slice(&batch);
        }
        let drafts: Vec<SpecPartDraft> = parts
            .iter()
            .enumerate()
            .map(|(index, part)| SpecPartDraft {
                name: part.name.trim().to_string(),
                path: part.path.clone(),
                stated_by: part.stated_by,
                records: assigned
                    .iter()
                    .enumerate()
                    .filter(|(_, part)| **part == Some(index))
                    .map(|(record, _)| record)
                    .collect(),
            })
            .collect();
        let whole = assigned.iter().filter(|part| part.is_none()).count();
        self.intent_event(
            "spec_parts",
            &format!(
                "{path}: {}; {whole} record(s) govern {main}",
                drafts
                    .iter()
                    .map(|part| format!(
                        "{} {} ({} records)",
                        part.name,
                        part.path,
                        part.records.len()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        Ok(SpecScope::Parts(drafts))
    }

    /// Exhausted repairs never block an approval: every record governs the
    /// approval's own component, as before scoping existed (Constraint
    /// cd9f1a96). Any other failure is returned.
    fn scoping_failed(&mut self, path: &str, error: anyhow::Error) -> Result<SpecScope> {
        if !error.is::<InvalidModelOutput>() {
            return Err(error);
        }
        self.task.recovery = None;
        let diagnostic = bounded(&format!("{error:#}"), 300);
        self.intent_event("spec_parts_failed", &format!("{path}: {diagnostic}"));
        self.event(format!(
            "Scoping {path} failed validation three times; every record will govern the specification's own component. {diagnostic}"
        ));
        Ok(SpecScope::Failed(diagnostic))
    }

    async fn propose_parts(
        &mut self,
        path: &str,
        covers: &[String],
        records: &[SpecRecordDraft],
        main: &str,
    ) -> Result<Vec<ProposedPart>> {
        let prompt = parts_prompt(path, covers, records);
        let proposal: PartsProposal = self
            .model_json(&prompt, SPEC_PARTS_PURPOSE, parts_schema())
            .await?;
        let root = self.workspace.root().to_path_buf();
        check_parts(proposal, covers, records, main, |candidate| {
            root.join(candidate.trim_end_matches('/')).exists()
        })
        .map_err(|error| error.context(InvalidModelOutput))
    }

    async fn assign_parts(
        &mut self,
        path: &str,
        covers: &[String],
        parts: &[ProposedPart],
        records: &[SpecRecordDraft],
        start: usize,
        end: usize,
    ) -> Result<Vec<Option<usize>>> {
        let names: Vec<&str> = parts.iter().map(|part| part.name.trim()).collect();
        let prompt = scope_prompt(path, covers, parts, records, start, end);
        let proposal: ScopeProposal = self
            .model_json(
                &prompt,
                SPEC_SCOPE_PURPOSE,
                scope_schema(&names, start, end),
            )
            .await?;
        check_assignments(proposal, &names, start, end)
            .map_err(|error| error.context(InvalidModelOutput))
    }
}

/// The name the daemon gives the approval's own component: the last segment
/// of the first covered path, or the spec's file stem for `.`.
fn main_component_name(path: &str, covers: &[String]) -> String {
    match covers.first().map(String::as_str) {
        None | Some(".") => std::path::Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("project")
            .to_string(),
        Some(first) => first
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(first)
            .to_string(),
    }
}

fn covered_scope(covers: &[String]) -> String {
    if covers.iter().any(|cover| cover == ".") {
        "the whole project (covered path: .)".to_string()
    } else {
        format!("the covered paths {}", covers.join(", "))
    }
}

fn record_line(index: usize, record: &SpecRecordDraft) -> String {
    let description = record.description.replace('\n', " ");
    format!(
        "[{index}] {} · {} — {}",
        record.kind,
        record.title,
        bounded(&description, 200)
    )
}

fn parts_prompt(path: &str, covers: &[String], records: &[SpecRecordDraft]) -> String {
    let lines: Vec<String> = records
        .iter()
        .enumerate()
        .map(|(index, record)| record_line(index, record))
        .collect();
    format!(
        "You are the scope sensor for MOOSEDev's symbolic project memory. The records below were extracted from the specification {path}, which governs {}. Some specifications describe a system made of separate parts (components, modules, packages or crates), each with its own responsibility.\n\nList the parts this specification names as separate parts of the system with their own responsibility. For each part give: name, exactly as the specification writes it; path, the repository directory (ending in /) or file that holds it, relative to the project root, or where it will be created; and stated_by, the number of the record that names the part and states its responsibility. Name only parts the specification names; never invent one, and never add a directory the specification does not state. If it names no separate parts, return an empty parts list.\n\nRecords:\n{}",
        covered_scope(covers),
        lines.join("\n")
    )
}

fn scope_prompt(
    path: &str,
    covers: &[String],
    parts: &[ProposedPart],
    records: &[SpecRecordDraft],
    start: usize,
    end: usize,
) -> String {
    let parts: Vec<String> = parts
        .iter()
        .map(|part| {
            let stated = records
                .get(part.stated_by)
                .map(|record| record.description.replace('\n', " "))
                .unwrap_or_default();
            format!("- {}: {}", part.name.trim(), bounded(&stated, 300))
        })
        .collect();
    let lines: Vec<String> = (start..end)
        .map(|index| record_line(index, &records[index]))
        .collect();
    format!(
        "You are the scope sensor for MOOSEDev's symbolic project memory. The specification {path} governs {} and describes it as these parts, each with the responsibility the specification gives it:\n{}\n\nFor every record below, name the part that would build what the record requires, judged by those responsibilities. Answer {WHOLE} only when the record constrains all of it (its language, storage, overall structure, or how the parts relate) or would be built in more than one part. Give exactly one assignment per record, in record order.\n\nRecords:\n{}",
        covered_scope(covers),
        parts.join("\n"),
        lines.join("\n")
    )
}

fn parts_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action", "parts"],
        "properties": {
            "action": {"type": "string", "const": "propose_spec_parts"},
            "parts": {
                "type": "array",
                "maxItems": MAX_PARTS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["name", "path", "stated_by"],
                    "properties": {
                        "name": {"type": "string", "minLength": 1, "maxLength": 80},
                        "path": {"type": "string", "minLength": 1},
                        "stated_by": {"type": "integer", "minimum": 0}
                    }
                }
            }
        }
    })
}

fn scope_schema(names: &[&str], start: usize, end: usize) -> Value {
    let mut allowed: Vec<&str> = names.to_vec();
    allowed.push(WHOLE);
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action", "assignments"],
        "properties": {
            "action": {"type": "string", "const": "assign_spec_parts"},
            "assignments": {
                "type": "array",
                "minItems": end - start,
                "maxItems": end - start,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["record", "part"],
                    "properties": {
                        "record": {"type": "integer", "minimum": start, "maximum": end - 1},
                        "part": {"type": "string", "enum": allowed}
                    }
                }
            }
        }
    })
}

/// The parts the sensor listed, each checked: a unique name that is not the
/// approval's own component, a stating record that names it, and a path inside
/// the covered paths. A path that does not exist yet must be the part's name
/// directly under a covered path; an invented parent directory is refused.
fn check_parts(
    proposal: PartsProposal,
    covers: &[String],
    records: &[SpecRecordDraft],
    main: &str,
    exists: impl Fn(&str) -> bool,
) -> Result<Vec<ProposedPart>> {
    anyhow::ensure!(
        proposal.action == "propose_spec_parts",
        "spec parts returned the wrong action"
    );
    anyhow::ensure!(
        proposal.parts.len() <= MAX_PARTS,
        "list at most {MAX_PARTS} parts"
    );
    let mut names = vec![main.to_lowercase()];
    let mut parts = Vec::new();
    for mut part in proposal.parts {
        let name = part.name.trim().to_string();
        anyhow::ensure!(
            !name.is_empty() && name.len() <= 80 && !name.chars().any(char::is_control),
            "a part name must be 1..=80 bytes without control characters"
        );
        anyhow::ensure!(
            !names.contains(&name.to_lowercase()),
            "part {name:?} repeats another part or names the specification's own scope"
        );
        anyhow::ensure!(
            !name.eq_ignore_ascii_case(WHOLE),
            "{WHOLE:?} is reserved for records that govern the whole scope; name the part as the specification does"
        );
        names.push(name.to_lowercase());
        let stated = records.get(part.stated_by).ok_or_else(|| {
            anyhow::anyhow!(
                "part {name:?} is stated by record {}, which does not exist",
                part.stated_by
            )
        })?;
        let needle = name.to_lowercase();
        anyhow::ensure!(
            stated.title.to_lowercase().contains(&needle)
                || stated.description.to_lowercase().contains(&needle),
            "part {name:?} is stated by record {}, which does not name it; give the record that names the part",
            part.stated_by
        );
        let raw = part.path.trim().trim_start_matches("./").to_string();
        let body = raw.trim_end_matches('/');
        anyhow::ensure!(
            !body.is_empty()
                && !raw.contains('\\')
                && body
                    .split('/')
                    .all(|segment| !segment.is_empty() && segment != "." && segment != ".."),
            "part {name:?} path must be repo-relative without traversal: {}",
            part.path
        );
        let present = exists(body);
        let path = if raw.ends_with('/')
            || (!present && !body.rsplit('/').next().unwrap_or(body).contains('.'))
        {
            format!("{body}/")
        } else {
            body.to_string()
        };
        anyhow::ensure!(
            !covers.contains(&path) && !covers.contains(&body.to_string()),
            "part {name:?} must cover less than the specification does"
        );
        let inside = covers.iter().find(|cover| {
            cover.as_str() == "." || (cover.ends_with('/') && path.starts_with(cover.as_str()))
        });
        let Some(cover) = inside else {
            anyhow::bail!(
                "part {name:?} path {path} lies outside the covered paths {}",
                covers.join(", ")
            );
        };
        if !present {
            let under = if cover == "." { "" } else { cover.as_str() };
            anyhow::ensure!(
                path == format!("{under}{name}/"),
                "part {name:?} path {path} does not exist; a part still to be created goes directly under the covered path, as {under}{name}/"
            );
        }
        part.name = name;
        part.path = path;
        parts.push(part);
    }
    Ok(parts)
}

/// One assignment per record of the batch, each to a listed part or `whole`.
fn check_assignments(
    proposal: ScopeProposal,
    names: &[&str],
    start: usize,
    end: usize,
) -> Result<Vec<Option<usize>>> {
    anyhow::ensure!(
        proposal.action == "assign_spec_parts",
        "spec scope returned the wrong action"
    );
    let mut assigned: Vec<Option<Option<usize>>> = vec![None; end - start];
    for assignment in proposal.assignments {
        anyhow::ensure!(
            (start..end).contains(&assignment.record),
            "record {} is not one of records {start}-{}",
            assignment.record,
            end - 1
        );
        let part = if assignment.part == WHOLE {
            None
        } else {
            Some(
                names
                    .iter()
                    .position(|name| *name == assignment.part)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "record {} names part {:?}, which is not listed; answer one of {} or {WHOLE}",
                            assignment.record,
                            assignment.part,
                            names.join(", ")
                        )
                    })?,
            )
        };
        let slot = &mut assigned[assignment.record - start];
        anyhow::ensure!(
            slot.is_none(),
            "record {} is assigned more than once",
            assignment.record
        );
        *slot = Some(part);
    }
    let missing: Vec<String> = assigned
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.is_none())
        .map(|(offset, _)| (start + offset).to_string())
        .collect();
    anyhow::ensure!(
        missing.is_empty(),
        "every record needs one assignment; missing records {}",
        missing.join(", ")
    );
    Ok(assigned.into_iter().map(|slot| slot.flatten()).collect())
}

/// The parts of an unchanged approval, rebuilt from the components its
/// records concern: the group covering a requested path is the approval's
/// own; every other group inside the covered paths is a part. `None` when
/// there is no such group, so the sensor is asked.
pub(super) fn parts_from_components(
    groups: &[SpecComponentGroup],
    covers: &[String],
    records: &[SpecRecordDraft],
) -> Option<Vec<SpecPartDraft>> {
    let same = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
    let is_main = |group: &SpecComponentGroup| {
        group
            .covers
            .iter()
            .any(|path| covers.iter().any(|cover| same(path, cover)))
    };
    let inside = |path: &str| {
        covers.iter().any(|cover| {
            let directory = format!("{}/", cover.trim_end_matches('/'));
            cover == "." || (path.starts_with(&directory) && !same(path, cover))
        })
    };
    // A record goes to one part at most: the first group that claims it.
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let parts: Vec<SpecPartDraft> = groups
        .iter()
        .filter(|group| !is_main(group))
        .filter_map(|group| {
            let path = group.covers.iter().find(|path| inside(path))?.clone();
            let needle = group.name.to_lowercase();
            let stated_by = group
                .records
                .iter()
                .copied()
                .find(|&index| {
                    records.get(index).is_some_and(|record| {
                        record.title.to_lowercase().contains(&needle)
                            || record.description.to_lowercase().contains(&needle)
                    })
                })
                .or_else(|| group.records.first().copied())?;
            let records: Vec<usize> = group
                .records
                .iter()
                .copied()
                .filter(|index| claimed.insert(*index))
                .collect();
            Some(SpecPartDraft {
                name: group.name.clone(),
                path,
                stated_by,
                records,
            })
        })
        .collect();
    (!parts.is_empty()).then_some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(title: &str, description: &str) -> SpecRecordDraft {
        SpecRecordDraft {
            kind: "Requirement".into(),
            title: title.into(),
            description: description.into(),
            evidence: vec!["spec.md:1".into()],
        }
    }

    fn part(name: &str, path: &str, stated_by: usize) -> ProposedPart {
        ProposedPart {
            name: name.into(),
            path: path.into(),
            stated_by,
        }
    }

    fn records() -> Vec<SpecRecordDraft> {
        vec![
            record("Implementation language", "The system is written in Rust."),
            record(
                "sim crate responsibility",
                "The sim crate runs the game rules.",
            ),
            record("Faction rules", "Each faction has one weakness."),
        ]
    }

    #[test]
    fn parts_must_be_named_by_their_record_and_lie_inside_the_covers() {
        let covers = vec![".".to_string()];
        let check = |parts: Vec<ProposedPart>| {
            check_parts(
                PartsProposal {
                    action: "propose_spec_parts".into(),
                    parts,
                },
                &covers,
                &records(),
                "spec",
                |path| path == "src",
            )
        };
        let parts = check(vec![part("sim", "sim", 1)]).unwrap();
        assert_eq!(parts[0].path, "sim/", "a directory still to be created");
        let error = check(vec![part("sim", "sim", 0)]).unwrap_err().to_string();
        assert!(error.contains("does not name it"), "{error}");
        let error = check(vec![part("sim", "crates/sim/", 1)])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("goes directly under the covered path"),
            "{error}"
        );
        let error = check(vec![part("spec", "spec/", 1)])
            .unwrap_err()
            .to_string();
        assert!(error.contains("repeats"), "{error}");
        let error = check(vec![part("sim", "../sim/", 1)])
            .unwrap_err()
            .to_string();
        assert!(error.contains("without traversal"), "{error}");
        let whole = check_parts(
            PartsProposal {
                action: "propose_spec_parts".into(),
                parts: vec![part("Whole", "whole/", 0)],
            },
            &covers,
            &[record("Whole engine", "The Whole engine runs.")],
            "spec",
            |_| false,
        )
        .unwrap_err()
        .to_string();
        assert!(whole.contains("reserved"), "{whole}");
        // An existing directory may be anywhere inside the covers.
        let covers = vec!["crates/".to_string()];
        let inside = check_parts(
            PartsProposal {
                action: "propose_spec_parts".into(),
                parts: vec![part("sim", "crates/engine/sim/", 1)],
            },
            &covers,
            &records(),
            "crates",
            |path| path == "crates/engine/sim",
        )
        .unwrap();
        assert_eq!(inside[0].path, "crates/engine/sim/");
        let outside = check_parts(
            PartsProposal {
                action: "propose_spec_parts".into(),
                parts: vec![part("sim", "sim/", 1)],
            },
            &covers,
            &records(),
            "crates",
            |_| true,
        )
        .unwrap_err()
        .to_string();
        assert!(outside.contains("outside the covered paths"), "{outside}");
    }

    #[test]
    fn every_record_of_a_batch_gets_exactly_one_listed_part_or_whole() {
        let names = ["sim", "tui"];
        let assignment = |record: usize, part: &str| Assignment {
            record,
            part: part.into(),
        };
        let check = |assignments: Vec<Assignment>| {
            check_assignments(
                ScopeProposal {
                    action: "assign_spec_parts".into(),
                    assignments,
                },
                &names,
                40,
                43,
            )
        };
        assert_eq!(
            check(vec![
                assignment(40, "whole"),
                assignment(41, "sim"),
                assignment(42, "tui")
            ])
            .unwrap(),
            vec![None, Some(0), Some(1)]
        );
        let missing = check(vec![assignment(40, "sim")]).unwrap_err().to_string();
        assert!(missing.contains("missing records 41, 42"), "{missing}");
        let unknown = check(vec![
            assignment(40, "map"),
            assignment(41, "sim"),
            assignment(42, "sim"),
        ])
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("not listed"), "{unknown}");
        let twice = check(vec![
            assignment(40, "sim"),
            assignment(40, "tui"),
            assignment(42, "sim"),
        ])
        .unwrap_err()
        .to_string();
        assert!(twice.contains("more than once"), "{twice}");
        let range = check(vec![assignment(3, "sim")]).unwrap_err().to_string();
        assert!(range.contains("not one of records 40-42"), "{range}");
    }

    #[test]
    fn an_unchanged_approval_keeps_its_parts_from_the_components_its_records_concern() {
        let groups = [
            SpecComponentGroup {
                name: "spec".into(),
                covers: vec![".".into()],
                records: vec![0],
            },
            SpecComponentGroup {
                name: "sim".into(),
                covers: vec!["sim/".into()],
                records: vec![1, 2],
            },
            // A component outside the covered paths is not a part.
            SpecComponentGroup {
                name: "elsewhere".into(),
                covers: vec!["../other/".into()],
                records: vec![2],
            },
        ];
        let covers = vec![".".to_string()];
        let parts = parts_from_components(&groups[..2], &covers, &records()).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(
            (
                parts[0].name.as_str(),
                parts[0].path.as_str(),
                parts[0].stated_by
            ),
            ("sim", "sim/", 1)
        );
        assert_eq!(parts[0].records, vec![1, 2]);
        assert!(parts_from_components(&groups[..1], &covers, &records()).is_none());
        let scoped = vec!["crates/".to_string()];
        assert!(parts_from_components(&groups[2..], &scoped, &records()).is_none());
        // A sibling directory sharing the prefix is not inside the covers.
        let sibling = [SpecComponentGroup {
            name: "sim".into(),
            covers: vec!["crates2/sim/".into()],
            records: vec![1],
        }];
        assert!(parts_from_components(&sibling, &scoped, &records()).is_none());
    }
}

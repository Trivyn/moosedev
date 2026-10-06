//! The harness does the looking, and the model names what is missing.
//!
//! Looking loops caused 67-86% of parks over the badciv, cafe and sim
//! journals, and 99% of the sim's searches were the objective's own words
//! (Lesson 0fd685fa). So on entering a planning cycle the harness searches the
//! objective's words itself, once, and shows the matching lines in the prompt
//! head. What the project holds is the harness's to look up; what it lacks
//! takes understanding, so at a looking park the model is asked one narrow
//! question, what it needs that is not shown, and the answer goes to the
//! human with the park.
use super::task::bounded;
use super::{Mode, Phase, Runner};
use anyhow::Result;
use serde_json::json;

/// Opens the gathered block in the prompt head.
pub(super) const GATHER_HEADER: &str = "Gathered for this objective (the harness searched its words before planning; searching them again shows the same):\n";
const MAX_TERMS: usize = 64;
/// The most lines shown from one file, and in all, ranked by how many of the
/// objective's words each holds.
const LINES_PER_FILE: usize = 6;
const MAX_LINES: usize = 40;
const MAX_CANDIDATES: usize = 20_000;
const LINE_CHARS: usize = 160;
const BLOCK_BYTES: usize = 4_000;
const KNOWLEDGE_BYTES: usize = 1_200;
const ABSENT_BYTES: usize = 600;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_FILES_READ: usize = 3_000;
/// Words that say nothing about the project: articles, auxiliaries, common
/// verbs of instruction, and the harness's own action names.
const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "are",
    "not",
    "but",
    "you",
    "your",
    "have",
    "has",
    "was",
    "were",
    "will",
    "can",
    "should",
    "would",
    "could",
    "into",
    "onto",
    "then",
    "than",
    "when",
    "what",
    "which",
    "who",
    "how",
    "why",
    "where",
    "there",
    "here",
    "they",
    "them",
    "their",
    "its",
    "our",
    "all",
    "any",
    "each",
    "every",
    "one",
    "two",
    "also",
    "just",
    "only",
    "more",
    "most",
    "some",
    "such",
    "own",
    "same",
    "other",
    "out",
    "use",
    "using",
    "used",
    "make",
    "made",
    "does",
    "did",
    "done",
    "doing",
    "get",
    "got",
    "let",
    "may",
    "might",
    "must",
    "shall",
    "been",
    "being",
    "very",
    "too",
    "now",
    "new",
    "old",
    "like",
    "want",
    "need",
    "needs",
    "know",
    "see",
    "yet",
    "per",
    "via",
    "etc",
    "don",
    "doesn",
    "isn",
    "aren",
    "won",
    "please",
    "add",
    "write",
    "create",
    "implement",
    "change",
    "update",
    "fix",
    "read",
    "search",
    "inspect",
    "plan",
    "replace",
    "command",
    "question",
    "reply",
    "finish",
    "later",
    "rather",
    "instead",
    "about",
    "after",
    "before",
    "over",
    "under",
    "between",
    "both",
    "either",
    "neither",
    "nor",
    "yes",
];
/// Appended to the step prompt that produced a looping action.
const MISSING_QUESTION: &str = "\n\nBefore the human is asked for guidance: list the information this change needs that the project, the accepted knowledge and the results shown above do not contain. Answer with only this JSON object: {\"missing\": [...]}, one short item per missing piece; leave the list empty if nothing is missing.";
const MAX_MISSING: usize = 5;

/// `MOOSEDEV_HARNESS_OBJECTIVE_GATHER=off` leaves the looking to the model.
pub(super) fn gather_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_OBJECTIVE_GATHER").map_or(true, |value| value.trim() != "off")
}

/// `MOOSEDEV_HARNESS_ASK_MISSING=off` parks without asking the model what is
/// missing.
fn ask_missing_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_ASK_MISSING").map_or(true, |value| value.trim() != "off")
}

/// Whether a repository path is searched for the objective's words: not a
/// hidden path (tooling and agent configuration such as `.claude/`) and not a
/// generated lockfile.
fn searchable(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    !file.split('/').any(|part| part.starts_with('.'))
        && !name.ends_with(".lock")
        && !matches!(name, "package-lock.json" | "pnpm-lock.yaml" | "go.sum")
}

/// The objective's words worth searching, lowercased, in order of first
/// appearance: three or more characters, hyphenated and snake_case words kept
/// whole ("city-legal", "map_name"), anything else a separator
/// ("food/prod/sci" is three words), digits-only words and stopwords dropped.
pub(super) fn objective_terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')) {
        let word = raw.trim_matches(|c| c == '-' || c == '_').to_lowercase();
        if word.chars().count() < 3
            || word.chars().all(|c| c.is_ascii_digit())
            || STOPWORDS.contains(&word.as_str())
            || terms.contains(&word)
        {
            continue;
        }
        terms.push(word);
        if terms.len() == MAX_TERMS {
            break;
        }
    }
    terms
}

/// The block shown in the head: the objective's knowledge, the repository
/// lines its words matched (each line once), and the words found nowhere.
/// Bounded to [`BLOCK_BYTES`]; the absent words keep their room. `skipped`
/// files (too large, unreadable, or past the scan bound) are named, so an
/// absence is never claimed over files that were not searched.
fn render_block(knowledge: &str, lines: &[String], absent: &[String], skipped: usize) -> String {
    let mut block = GATHER_HEADER.to_string();
    if !knowledge.is_empty() {
        block.push_str(&format!(
            "Accepted project knowledge for the objective:\n{}\n",
            bounded(knowledge, KNOWLEDGE_BYTES)
        ));
    }
    let mut absent_line = String::new();
    if !absent.is_empty() {
        let mut listed = Vec::new();
        let mut bytes = 0;
        for word in absent {
            if bytes + word.len() + 4 > ABSENT_BYTES {
                break;
            }
            bytes += word.len() + 4;
            listed.push(format!("'{word}'"));
        }
        let more = absent.len() - listed.len();
        let place = if skipped == 0 {
            "the repository".to_string()
        } else {
            format!("the files searched ({skipped} not searched: too large, unreadable or past the scan bound)")
        };
        absent_line = format!(
            "Words found nowhere in {place} or this knowledge: {}{}.\n",
            listed.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        );
    }
    // Room for the lines, the omitted-lines notice held back.
    let room = BLOCK_BYTES.saturating_sub(block.len() + absent_line.len() + 48);
    let mut used = 0;
    let mut omitted = 0;
    for line in lines {
        if used + line.len() + 1 > room {
            omitted += 1;
            continue;
        }
        used += line.len() + 1;
        block.push_str(line);
        block.push('\n');
    }
    if omitted > 0 {
        block.push_str(&format!("({omitted} more matching lines not shown)\n"));
    }
    if lines.is_empty() && knowledge.is_empty() {
        block.push_str(
            "Nothing in the repository or project knowledge matched the objective's words.\n",
        );
    }
    block.push_str(&absent_line);
    block
}

/// What a model's answer to the missing question names. The step prompt it
/// is asked on carries the action schema in its head, so a model may answer
/// with an action: a `question` names what is missing in its own words
/// (simH3 prompt 2's fourth answer: "no yield table is defined anywhere");
/// any other action names nothing.
fn missing_items(answer: &serde_json::Value) -> Vec<String> {
    let named: Vec<String> = match answer["missing"].as_array() {
        Some(items) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::to_owned)
            .collect(),
        None if answer["action"] == "question" => answer["question"]
            .as_str()
            .map(|question| vec![question.to_owned()])
            .unwrap_or_default(),
        None => Vec::new(),
    };
    named
        .into_iter()
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .take(MAX_MISSING)
        .collect()
}

impl Runner {
    /// The gathered block for this planning cycle, when there is one.
    pub(super) fn gathered_block(&self) -> Option<&str> {
        if self.task.mode != Mode::Plan || !gather_enabled() {
            return None;
        }
        self.task
            .symbolic
            .as_ref()?
            .gathered
            .as_ref()
            .map(|gathered| gathered.block.as_str())
    }

    /// On entering a planning cycle (a new objective, or a human message that
    /// changed the guidance), before its first model step: search the
    /// objective's words once, in one pass over the repository and one
    /// knowledge search, and keep the block for every step of the cycle, so
    /// the prompt head stays byte-identical across it (Pattern 22f62f69).
    pub(super) async fn gather_for_objective(&mut self, files: &[String]) -> Result<()> {
        if self.task.mode != Mode::Plan || !gather_enabled() {
            return Ok(());
        }
        // Edits change what the repository holds: a planning cycle after
        // them gathers afresh even with the same objective and guidance.
        let text = format!("{} {}", self.task.objective, self.task.guidance)
            .trim()
            .to_owned();
        if text.is_empty() {
            return Ok(());
        }
        let key = format!("{text}\n{} edits", self.task.edits.len());
        let current = self
            .task
            .symbolic
            .as_ref()
            .and_then(|state| state.gathered.as_ref())
            .is_some_and(|gathered| gathered.key == key);
        if current {
            return Ok(());
        }
        let terms = objective_terms(&text);
        let mut found = vec![false; terms.len()];
        // Every line holding an objective word, scored by how many distinct
        // words it holds: a line naming several of them is about the
        // objective, one naming a single common word mostly is not.
        let mut candidates: Vec<(usize, usize, usize, String)> = Vec::new();
        let mut read = 0;
        let mut skipped = 0;
        for (position, file) in files.iter().enumerate() {
            if read == MAX_FILES_READ {
                skipped += files.len() - position;
                break;
            }
            if !searchable(file) {
                continue;
            }
            // Sized before it is read: a large file is skipped, not loaded.
            let small = std::fs::metadata(self.workspace.root().join(file))
                .is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES as u64);
            let source = match small.then(|| self.workspace.read(file)) {
                Some(Ok(Some(source))) => {
                    read += 1;
                    source
                }
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            let path = file.to_lowercase();
            let lower = source.to_lowercase();
            for (index, term) in terms.iter().enumerate() {
                if path.contains(term.as_str()) || lower.contains(term.as_str()) {
                    found[index] = true;
                }
            }
            // Lowercasing adds no line breaks, so the lines pair up.
            for (number, (line, lowered)) in source.lines().zip(lower.lines()).enumerate() {
                let score = terms
                    .iter()
                    .filter(|term| lowered.contains(term.as_str()))
                    .count();
                if score > 0 && candidates.len() < MAX_CANDIDATES {
                    candidates.push((
                        score,
                        position,
                        number,
                        format!(
                            "{file}:{}: {}",
                            number + 1,
                            bounded(line.trim(), LINE_CHARS)
                        ),
                    ));
                }
            }
        }
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut per_file: std::collections::BTreeMap<usize, usize> = Default::default();
        let mut lines: Vec<String> = Vec::new();
        for (_, position, _, shown) in candidates {
            let count = per_file.entry(position).or_default();
            if *count == LINES_PER_FILE || lines.len() == MAX_LINES {
                continue;
            }
            *count += 1;
            lines.push(shown);
        }
        // The objective's knowledge by relevance, recorded as no model search;
        // a daemon failure leaves the repository half.
        let knowledge = match self.query_knowledge(&text, Some(KNOWLEDGE_BYTES)).await {
            Ok(search) if !search.evidence_iris.is_empty() => search.context.trim().to_owned(),
            Ok(_) => String::new(),
            Err(error) => {
                self.intent_event(
                    "objective_gather_knowledge_failed",
                    &bounded(&format!("{error:#}"), 300),
                );
                String::new()
            }
        };
        let knowledge_lower = knowledge.to_lowercase();
        let absent: Vec<String> = terms
            .iter()
            .zip(&found)
            .filter(|(term, found)| !**found && !knowledge_lower.contains(term.as_str()))
            .map(|(term, _)| term.clone())
            .collect();
        let block = render_block(&knowledge, &lines, &absent, skipped);
        self.intent_event(
            "objective_gathered",
            &format!(
                "{} terms, {} lines, {} absent{}",
                terms.len(),
                lines.len(),
                absent.len(),
                if skipped > 0 {
                    format!("; {skipped} files not searched")
                } else {
                    String::new()
                }
            ),
        );
        self.event(format!(
            "Harness searched the objective's words before planning: {} matching line(s); found nowhere: {}.",
            lines.len(),
            if absent.is_empty() {
                "none".to_string()
            } else {
                absent.join(", ")
            }
        ));
        self.symbolic_state_mut().gathered = Some(super::symbolic::GatheredState { key, block });
        Ok(())
    }

    /// After a read, inspect or search park: ask the model, with the step
    /// prompt that produced the looping action, what it needs that is not
    /// shown, and put a non-empty answer at the head of the park message. One
    /// request per park; a failed request leaves the plain park.
    pub(super) async fn ask_what_is_missing(&mut self, step_prompt: String) -> Result<()> {
        // A park (recovery off), or a harness recovery from the loop (on).
        let parked = self.task.phase == Phase::AwaitingInput;
        let recovered = matches!(self.task.phase, Phase::Planning | Phase::Working)
            && self.task.best_effort.is_none();
        if !ask_missing_enabled() || !(parked || recovered) {
            return Ok(());
        }
        let schema = json!({"type":"object","additionalProperties":false,"required":["missing"],"properties":{"missing":{"type":"array","maxItems":MAX_MISSING,"items":{"type":"string","maxLength":300}}}});
        let prompt = format!("{step_prompt}{MISSING_QUESTION}");
        match self
            .model_json::<serde_json::Value>(&prompt, "harness_missing", schema)
            .await
        {
            Err(error) => {
                self.intent_event("missing_failed", &bounded(&format!("{error:#}"), 300));
            }
            Ok(answer) => {
                let items = missing_items(&answer);
                if let Some(action) = answer["action"].as_str().filter(|_| items.is_empty()) {
                    self.intent_event("missing_answered_with_action", action);
                }
                self.intent_event("missing_asked", &format!("{} item(s)", items.len()));
                if !items.is_empty() {
                    let list = items
                        .iter()
                        .map(|item| format!("- {item}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.event(format!("The model named what it is missing:\n{list}"));
                    if parked {
                        self.task.last_response = format!(
                            "The model needs information the project does not hold:\n{list}\nGuidance is needed: give it, or say to proceed with stated placeholders.\n({})",
                            self.task.last_response
                        );
                    } else {
                        // Information only the human holds is the human's
                        // stop (AD ad50c9cd): the recovery gives way to it.
                        self.event(
                            "Harness recovery set aside: the model named information only the human holds.",
                        );
                        self.task.last_response = format!(
                            "The model needs information the project does not hold:\n{list}\nGuidance is needed: give it, or say to proceed with stated placeholders."
                        );
                        self.task.phase = Phase::AwaitingInput;
                        self.task.turn_finished = true;
                        self.park_under_approved_plan();
                    }
                }
            }
        }
        self.persist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROMPT_2: &str = "Schema, v0:\n\ngame(id, map_name, width, height, wrap_x, wrap_y, turn)\ntile(game_id, x, y, terrain, climate, resource, has_river, owner, food, prod, sci)\n\nStore terrain, climate, resource, faction as the same strings the map file uses. Compute food/prod/sci at ingest from terrain + climate and write them onto the tile. I don't want every read path reimplementing the yield table.\n\nMountains and ocean are not city-legal. Land units cannot enter ocean.";

    #[test]
    fn the_objectives_words_keep_compounds_and_drop_stopwords() {
        let terms = objective_terms(PROMPT_2);
        for kept in [
            "map_name",
            "city-legal",
            "food",
            "prod",
            "sci",
            "yield",
            "terrain",
        ] {
            assert!(terms.contains(&kept.to_string()), "{kept}: {terms:?}");
        }
        for dropped in ["the", "and", "don", "write", "read", "v0", "id"] {
            assert!(
                !terms.contains(&dropped.to_string()),
                "{dropped}: {terms:?}"
            );
        }
        // First appearance order, no repeats.
        assert_eq!(terms[0], "schema");
        let unique: std::collections::BTreeSet<_> = terms.iter().collect();
        assert_eq!(unique.len(), terms.len());
    }

    #[test]
    fn a_question_answers_the_missing_question_and_another_action_names_nothing() {
        assert_eq!(
            missing_items(&json!({"missing": [" the yield table ", ""]})),
            ["the yield table"]
        );
        assert_eq!(
            missing_items(&json!({"action": "question", "question": "What yield values?"})),
            ["What yield values?"]
        );
        assert!(missing_items(&json!({"action": "search", "query": "yield"})).is_empty());
    }

    #[test]
    fn hidden_paths_and_lockfiles_are_not_searched() {
        assert!(searchable("badciv-map.md"));
        assert!(searchable("badciv-map/src/lib.rs"));
        assert!(!searchable(".gitignore"));
        assert!(!searchable(".claude/skills/a/SKILL.md"));
        assert!(!searchable("Cargo.lock"));
        assert!(!searchable("web/package-lock.json"));
    }

    #[test]
    fn the_block_shows_each_line_once_lists_absent_words_and_stays_bounded() {
        let lines: Vec<String> = (0..200)
            .map(|n| format!("spec.md:{n}: a line about yields number {n}"))
            .collect();
        let absent = vec!["reimplementing".to_string(), "map_name".to_string()];
        let block = render_block("", &lines, &absent, 0);
        assert!(block.starts_with(GATHER_HEADER));
        assert!(block.len() <= BLOCK_BYTES, "{}", block.len());
        // Files not searched are named, so absence is not claimed over them.
        let partial = render_block("", &[], &absent, 3);
        assert!(
            partial.contains("the files searched (3 not searched"),
            "{partial}"
        );
        assert!(block.ends_with(
            "Words found nowhere in the repository or this knowledge: 'reimplementing', 'map_name'.\n"
        ));
        assert!(block.contains("more matching lines not shown"));
        assert_eq!(block, render_block("", &lines, &absent, 0));
    }
}

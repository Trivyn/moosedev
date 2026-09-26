//! What each step is shown of the plan. The plan is harness state: the task
//! keeps all of it, and a model may write a plan as long as the work needs.
//! A step's prompt shows a bounded view chosen by what the step is about,
//! the way source and rules are bounded, instead of the planner being told to
//! write less (badciv e948c9c7).
use super::Runner;

/// Summary bytes a step prompt shows of the plan.
pub(super) const PLAN_VIEW_BYTES: usize = 4_000;

/// Escaped bytes of the blank line between two shown paragraphs.
const SEPARATOR: usize = 4;

/// `summary` within `budget` bytes as the prompt carries it (JSON-escaped).
/// A summary that fits is shown whole. Otherwise whole paragraphs are
/// admitted while they fit: the first (the plan's intent, cut to half the room
/// if it alone is too long), then those naming each `focus` file, most
/// pressing file first, then the rest. Admitted paragraphs keep the plan's
/// order. A closing line counts what was left out and names `route`, where the
/// whole plan can be read.
pub(super) fn plan_view(summary: &str, focus: &[String], budget: usize, route: &str) -> String {
    if escaped_len(summary) <= budget {
        return summary.to_owned();
    }
    let paragraphs = paragraphs(summary);
    let notice = |omitted: usize, omitted_bytes: usize| {
        format!(
            "[Plan shown in part: {omitted} of {} paragraphs ({omitted_bytes} of {} bytes) left out for this step; {route}]",
            paragraphs.len(),
            summary.len()
        )
    };
    // The closing line at its longest, so the paragraphs never crowd it out.
    let room = budget.saturating_sub(escaped_len(&notice(paragraphs.len(), summary.len())));
    // Admission order: the intent, then the paragraphs of each focus file in
    // focus order (the most pressing file first), then everything else.
    let mut order = Vec::new();
    for file in focus {
        for (index, paragraph) in paragraphs.iter().enumerate().skip(1) {
            if !order.contains(&index) && mentions_file(paragraph, file) {
                order.push(index);
            }
        }
    }
    for index in 1..paragraphs.len() {
        if !order.contains(&index) {
            order.push(index);
        }
    }
    const CUT: &str = " […]";
    if room <= escaped_len(CUT) + SEPARATOR {
        // No room beside the closing line: it alone stands for the plan.
        return notice(paragraphs.len(), summary.len());
    }
    let intent_whole = escaped_len(paragraphs[0]) + SEPARATOR <= room;
    let intent = if intent_whole {
        paragraphs[0].to_owned()
    } else {
        let share = (room / 2).saturating_sub(escaped_len(CUT) + SEPARATOR);
        format!("{}{CUT}", escaped_prefix(paragraphs[0], share))
    };
    let mut used = escaped_len(&intent) + SEPARATOR;
    let mut keep = vec![false; paragraphs.len()];
    keep[0] = intent_whole;
    for index in order {
        let cost = escaped_len(paragraphs[index]) + SEPARATOR;
        if used + cost <= room {
            keep[index] = true;
            used += cost;
        }
    }
    let mut out = format!("{intent}\n\n");
    for (paragraph, _) in paragraphs
        .iter()
        .zip(&keep)
        .skip(1)
        .filter(|(_, kept)| **kept)
    {
        out.push_str(paragraph);
        out.push_str("\n\n");
    }
    let omitted = keep.iter().filter(|kept| !**kept).count();
    let omitted_bytes: usize = paragraphs
        .iter()
        .zip(&keep)
        .filter(|(_, kept)| !**kept)
        .map(|(paragraph, _)| paragraph.len())
        .sum();
    out.push_str(&notice(omitted, omitted_bytes));
    out
}

/// Bytes `text` takes inside a JSON string.
fn escaped_len(text: &str) -> usize {
    serde_json::to_string(text).map_or(text.len() * 6, |json| json.len() - 2)
}

/// The longest prefix of `text` whose JSON-escaped form fits `room`.
fn escaped_prefix(text: &str, room: usize) -> &str {
    let mut used = 0;
    for (at, character) in text.char_indices() {
        used += escaped_len(character.encode_utf8(&mut [0; 4]));
        if used > room {
            return &text[..at];
        }
    }
    text
}

/// Whether `paragraph` names `file`: its path or any tail of it at a `/`
/// (`tests/x.rs` or `x.rs` for `crate/tests/x.rs`), standing alone, so never
/// the tail of another name (`data.rs` for `a.rs`) or of another path
/// (`other/x.rs` for `src/x.rs`).
fn mentions_file(paragraph: &str, file: &str) -> bool {
    let part = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    let stands_alone = |needle: &str| {
        paragraph.match_indices(needle).any(|(at, _)| {
            let before = paragraph[..at].chars().next_back();
            let mut after = paragraph[at + needle.len()..].chars();
            // A name ends at a space or punctuation; `.` or `-` ends it only
            // when no name character follows (`a.rs.` ends a sentence,
            // `a.rs.bak` is another file).
            let ends = match after.next() {
                None => true,
                Some(c) if c.is_alphanumeric() || matches!(c, '_' | '/') => false,
                Some('.' | '-') => after
                    .next()
                    .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '/'))),
                Some(_) => true,
            };
            before.is_none_or(|c| !part(c)) && ends
        })
    };
    std::iter::once(file)
        .chain(file.match_indices('/').map(|(at, _)| &file[at + 1..]))
        .any(|tail| !tail.is_empty() && stands_alone(tail))
}

/// Paragraphs at blank lines; a summary without them splits at lines.
fn paragraphs(summary: &str) -> Vec<&str> {
    let blocks: Vec<&str> = summary
        .split("\n\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .collect();
    if blocks.len() > 1 {
        return blocks;
    }
    summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

impl Runner {
    /// The current plan's summary as this step is shown it: focused on the
    /// file the step is about, the files the latest failure names and the
    /// plan files not yet edited.
    pub(super) fn plan_summary_view(&self) -> Option<String> {
        let plan = self.task.plan.as_ref()?;
        let mut focus: Vec<String> = self
            .source_ranking()
            .into_iter()
            .filter(|(_, reason)| matches!(*reason, "latest" | "failed_output"))
            .map(|(file, _)| file)
            .collect();
        for file in &plan.files {
            if !focus.contains(file) && !self.task.edits.iter().any(|edit| &edit.file == file) {
                focus.push(file.clone());
            }
        }
        Some(plan_view(
            &plan.summary,
            &focus,
            PLAN_VIEW_BYTES,
            &self.plan_route(),
        ))
    }

    /// Where the whole current plan can be read: the journaled action that
    /// proposed it.
    pub(super) fn plan_route(&self) -> String {
        // The latest proposal whose summary is the stored plan's: a proposal
        // returned by the coverage check is journaled but never stored.
        let summary = self.task.plan.as_ref().map(|plan| plan.summary.as_str());
        let proposed = |message: &str| {
            let action = message.strip_prefix("Model action: ")?;
            let value: serde_json::Value = serde_json::from_str(action).ok()?;
            value["summary"].as_str().map(str::to_owned)
        };
        match self.task.events.iter().rposition(|event| {
            event.message.starts_with(PLAN_ACTION) && proposed(&event.message).as_deref() == summary
        }) {
            Some(event) => {
                format!("the whole plan is journal event {event}; inspect({event}, 0) pages it")
            }
            None => "the whole plan is in the task journal".into(),
        }
    }
}

const PLAN_ACTION: &str = "Model action: {\"action\":\"plan\"";

#[cfg(test)]
mod tests {
    use super::super::task::{Event, Plan};
    use super::super::test_support::{context_router, serve, Project};
    use super::*;

    #[tokio::test]
    async fn the_route_names_the_stored_plan_not_a_later_returned_proposal() {
        let project = Project::new("plan-route");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, "Plan".into())
            .await
            .unwrap();
        let action = |summary: &str| Event {
            message: format!(
                "Model action: {}",
                serde_json::json!({"action":"plan","summary":summary,"files":["a.rs"],"checks":["true"]})
            ),
        };
        let stored = runner.task.events.len();
        runner.task.events.push(action("Stored plan."));
        // A replan the coverage check returned: journaled, never stored.
        runner.task.events.push(action("Returned proposal."));
        runner.task.plan = Some(Plan {
            summary: "Stored plan.".into(),
            files: vec!["a.rs".into()],
            checks: vec!["true".into()],
            addresses: vec![],
        });
        assert!(
            runner
                .plan_route()
                .contains(&format!("journal event {stored};")),
            "{}",
            runner.plan_route()
        );
        server.abort();
    }

    #[test]
    fn a_summary_that_fits_is_shown_whole() {
        assert_eq!(
            plan_view("Do the thing.", &[], 100, "route"),
            "Do the thing."
        );
    }

    #[test]
    fn a_long_plan_keeps_its_intent_and_the_focus_paragraphs_in_order() {
        let summary = [
            "Implement the map crate.",
            &format!("Writer: src/write.rs emits sections. {}", "w".repeat(300)),
            &format!("Parser: src/parse.rs reads headers. {}", "p".repeat(300)),
            &format!("Tests: tests/roundtrip.rs covers it. {}", "t".repeat(300)),
        ]
        .join("\n\n");
        let view = plan_view(&summary, &["src/parse.rs".into()], 600, "see event 9");
        assert!(view.starts_with("Implement the map crate.\n\n"), "{view}");
        assert!(view.contains("Parser: src/parse.rs"), "{view}");
        assert!(
            !view.contains("Writer:") && !view.contains("Tests:"),
            "{view}"
        );
        assert!(view.ends_with("see event 9]"), "{view}");
        assert!(view.contains("2 of 4 paragraphs"), "{view}");

        // A file named by its file name alone still focuses its paragraph.
        let view = plan_view(&summary, &["crate/tests/roundtrip.rs".into()], 600, "r");
        assert!(view.contains("Tests: tests/roundtrip.rs"), "{view}");
        // Order is the plan's, whatever the focus.
        let both = plan_view(
            &summary,
            &["src/parse.rs".into(), "src/write.rs".into()],
            1_000,
            "r",
        );
        assert!(
            both.find("Writer:").unwrap() < both.find("Parser:").unwrap(),
            "{both}"
        );
    }

    #[test]
    fn the_view_fits_its_budget_as_the_prompt_carries_it() {
        let tabs = format!("Intent.\n\n{}", "\t".repeat(4_000));
        let long_intent = format!("{}\n\nnext paragraph", "i".repeat(9_000));
        let many = (0..400)
            .map(|i| format!("paragraph {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        for summary in [tabs, long_intent, many] {
            // Down to a budget with no room beside the closing line.
            for budget in [4_000, 1_000, 600, 200] {
                let view = plan_view(
                    &summary,
                    &[],
                    budget,
                    "the whole plan is journal event 12345; inspect(12345, 0) pages it",
                );
                assert!(
                    escaped_len(&view) <= budget,
                    "{budget}: {}",
                    escaped_len(&view)
                );
            }
        }
    }

    #[test]
    fn a_file_name_matches_only_standing_alone() {
        assert!(mentions_file("edit parse.rs next", "src/parse.rs"));
        assert!(mentions_file("see src/parse.rs:12", "src/parse.rs"));
        assert!(!mentions_file("see src/data.rs", "src/a.rs"));
        assert!(!mentions_file("see other/parse.rs", "src/parse.rs"));
        assert!(mentions_file("Edit src/a.rs.", "src/a.rs"));
        assert!(!mentions_file("see src/a.rs.bak", "src/a.rs"));
        assert!(!mentions_file("see src/a.rs-old", "src/a.rs"));
        assert!(!mentions_file("see src/a.rs/child", "src/a.rs"));
        assert!(!mentions_file("see src/a.rs._bak", "src/a.rs"));
        assert!(!mentions_file("see src/a.rs./child", "src/a.rs"));
    }

    #[test]
    fn an_intent_longer_than_the_view_is_cut_with_a_notice() {
        let summary = "x".repeat(2_000);
        let view = plan_view(&summary, &[], 1_000, "r");
        assert!(view.starts_with(&"x".repeat(300)));
        assert!(view.contains(" […]"));
        assert!(view.ends_with("r]"));
        assert!(view.len() <= 1_000);
    }
}

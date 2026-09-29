//! What each step is shown of the plan. The plan is harness state: the task
//! keeps all of it, and a model may write a plan as long as the work needs.
//! A step's prompt shows a bounded view chosen by what the step is about,
//! the way source and rules are bounded, instead of the planner being told to
//! write less (badciv e948c9c7).
use super::{Mode, Runner};

/// Summary bytes a step prompt shows of the plan, focused on the step: in
/// Plan mode, and in Auto for a plan larger than [`whole_plan_budget`].
pub(super) const PLAN_VIEW_BYTES: usize = 4_000;

/// Whether an Auto step is shown the whole approved plan when it fits
/// [`whole_plan_budget`] (`MOOSEDEV_HARNESS_WHOLE_PLAN`, default on).
fn whole_plan_enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_WHOLE_PLAN").map_or(true, |value| value.trim() != "off")
}

/// Summary bytes (as the prompt carries them) up to which an Auto step is
/// shown the whole plan: an eighth of the prompt budget, never less than the
/// focused view. badciv run 14's builder paged its 10.6 KB plan from the
/// journal 43 times while the step showed 4 KB of it, focused on files that
/// changed from step to step, so the plan's bytes also missed the prefix
/// cache.
pub(super) fn whole_plan_budget(prompt_budget: usize) -> usize {
    PLAN_VIEW_BYTES.max(prompt_budget / 8)
}

/// Summary bytes a replanning prompt shows of the approved plan it amends.
pub(super) const AMEND_VIEW_BYTES: usize = 6_000;

/// Escaped bytes of the blank line between two shown paragraphs.
const SEPARATOR: usize = 4;

/// `summary` whole up to `budget` bytes; past that its first `budget` bytes,
/// cut on a character boundary, and a line counting what was left out and
/// naming `route`.
pub(super) fn bounded_whole(summary: &str, budget: usize, route: &str) -> String {
    if summary.len() <= budget {
        return summary.to_owned();
    }
    let mut end = budget;
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[Plan cut: {} of {} bytes left out; {route}]",
        &summary[..end],
        summary.len() - end,
        summary.len()
    )
}

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
    /// The current plan's summary as this step is shown it. In Auto it is
    /// the whole approved plan when it fits [`whole_plan_budget`]: the same
    /// bytes every step, so the builder need not page it from the journal.
    /// Otherwise, and in Plan mode, it is focused on the file the step is
    /// about, the files the latest failure names and the plan files not yet
    /// edited. Only the step prompt shows this view; the capture note
    /// excerpts plans on its own.
    pub(super) fn plan_summary_view(&self) -> Option<String> {
        self.step_plan_view(whole_plan_enabled())
    }

    /// [`Self::plan_summary_view`] with the whole-plan switch given.
    fn step_plan_view(&self, whole: bool) -> Option<String> {
        let plan = self.task.plan.as_ref()?;
        if whole && self.task.mode == Mode::Auto {
            let budget = self
                .prompt_budget()
                .map_or(PLAN_VIEW_BYTES, whole_plan_budget);
            if escaped_len(&plan.summary) <= budget {
                return Some(plan.summary.clone());
            }
        }
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

    /// Whether the planner is revising a plan the human approved: Plan mode
    /// with the stored plan still the latest approved one (a replan, human
    /// guidance or /plan after approval), not a new proposal under review.
    pub(super) fn amending_approved_plan(&self) -> bool {
        self.task.mode == Mode::Plan
            && self.task.plan.as_ref().is_some_and(|plan| {
                self.task
                    .approved_plans
                    .last()
                    .is_some_and(|approved| approved.summary == plan.summary)
            })
    }

    /// The approved plan's whole summary for the planner amending it, up to
    /// `AMEND_VIEW_BYTES`: the condensed step view left the planner paging
    /// its own plan from the journal (badciv P5, one replan parked that way).
    pub(super) fn approved_plan_view(&self) -> Option<String> {
        let plan = self.task.plan.as_ref()?;
        Some(bounded_whole(
            &plan.summary,
            AMEND_VIEW_BYTES,
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
            satisfied: vec![],
            open_rules: vec![],
            open_choices: vec![],
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

    fn plan(summary: String) -> Plan {
        Plan {
            summary,
            files: vec!["src/parse.rs".into(), "tests/fixture.rs".into()],
            checks: vec!["true".into()],
            addresses: vec![],
            satisfied: vec![],
            open_rules: vec![],
            open_choices: vec![],
        }
    }

    /// A plan of `paragraphs` paragraphs of about 1 KB each.
    fn long_summary(paragraphs: usize) -> String {
        (0..paragraphs)
            .map(|i| {
                format!(
                    "Part {i}: src/parse.rs does step {i}. {}",
                    "x".repeat(1_000)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The Plan line of a step prompt: the plan as the prompt carries it.
    fn plan_line(prompt: &str) -> &str {
        prompt
            .split_once("\nPlan: ")
            .unwrap()
            .1
            .split_once('\n')
            .unwrap()
            .0
    }

    /// 1A: in Auto the whole approved plan is shown while it fits an eighth
    /// of the prompt budget (the same bytes every step); above that, and in
    /// Plan mode, the focused view. `MOOSEDEV_HARNESS_WHOLE_PLAN=off`
    /// restores the focused view byte for byte. One test, since it sets the
    /// switch for the process.
    #[tokio::test]
    async fn in_auto_the_whole_plan_is_shown_while_it_fits() {
        use super::super::test_support::test_config;
        let project = Project::new("whole-plan");
        let (daemon, server) = serve(context_router(), &project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, "Build".into())
            .await
            .unwrap();
        runner.configure(test_config(), None);
        let context = runner.context.clone().unwrap();
        // The test window's budget is 84,992 bytes: an eighth is 10,624.
        let budget = runner.prompt_budget().unwrap();
        assert_eq!(whole_plan_budget(budget), 10_624);
        assert_eq!(whole_plan_budget(10_000), PLAN_VIEW_BYTES);
        let summary = long_summary(8);
        assert!(summary.len() > PLAN_VIEW_BYTES && summary.len() <= 10_624);
        runner.task.plan = Some(plan(summary.clone()));
        runner.task.mode = Mode::Auto;
        runner.task.approved_revision = Some("fixture".into());

        let whole = runner.plan_summary_view().unwrap();
        assert_eq!(whole, summary);
        let (on, _) = runner.prompt(&context, &[]).unwrap();
        assert!(plan_line(&on).contains(&serde_json::to_string(&summary).unwrap()));
        // The focused view is today's, from the same state.
        let focused = runner.step_plan_view(false).unwrap();
        assert!(focused.contains("[Plan shown in part:"), "{focused}");
        assert!(escaped_len(&focused) <= PLAN_VIEW_BYTES);

        // Switched off, the prompt is today's byte for byte: only the plan
        // view differs, and it is the focused one.
        std::env::set_var("MOOSEDEV_HARNESS_WHOLE_PLAN", "off");
        let off = runner.prompt(&context, &[]).map(|(prompt, _)| prompt);
        let off_view = runner.plan_summary_view();
        std::env::remove_var("MOOSEDEV_HARNESS_WHOLE_PLAN");
        let off = off.unwrap();
        assert_eq!(off_view.as_deref(), Some(focused.as_str()));
        let escaped = |text: &str| {
            let json = serde_json::to_string(text).unwrap();
            json[1..json.len() - 1].to_owned()
        };
        assert_eq!(on.replacen(&escaped(&summary), &escaped(&focused), 1), off);

        // A plan past the whole-plan budget is focused, as before.
        let oversized = long_summary(12);
        assert!(oversized.len() > 10_624);
        runner.task.plan = Some(plan(oversized));
        assert_eq!(runner.plan_summary_view(), runner.step_plan_view(false));
        assert!(runner
            .plan_summary_view()
            .unwrap()
            .contains("[Plan shown in part:"));

        // Plan mode is focused whatever the size.
        runner.task.plan = Some(plan(summary));
        runner.task.mode = Mode::Plan;
        assert_eq!(runner.plan_summary_view().unwrap(), focused);
        server.abort();
    }

    #[test]
    fn an_approved_plan_is_shown_whole_up_to_its_bound_and_cut_on_a_character() {
        assert_eq!(bounded_whole("Keep it.", 100, "route"), "Keep it.");
        let summary = format!("{}é tail", "a".repeat(9));
        let cut = bounded_whole(&summary, 10, "see event 4");
        assert!(cut.starts_with(&"a".repeat(9)), "{cut}");
        assert!(!cut.contains('é'), "cut before a split character: {cut}");
        assert!(
            cut.ends_with(&format!(
                "\n[Plan cut: {} of {} bytes left out; see event 4]",
                summary.len() - 9,
                summary.len()
            )),
            "{cut}"
        );
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

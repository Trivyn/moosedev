//! Thin interactive frontend. Human review commands never enter the model action surface.
use super::protocol::{
    AssociatePage, DerivedBasis, SpecDisposition, SpecPrepareResponse, SpecRetirementDisposition,
    TypedDisposition, TypingMode,
};
use super::runner::{Phase, ReviewItem, Runner, SpecUncited, Task};
use super::{
    clipboard, markdown,
    selection::{self, Pos, Selection},
    session::{Command, Controller, Conversation, Snapshot, Update},
    startup::{ProviderSettings, StartupOptions},
};
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use ratatui::{
    layout::{Position, Rect},
    text::{Line, Span, Text},
};
use std::{
    collections::{BTreeSet, VecDeque},
    io::{self, IsTerminal},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Once,
    },
    time::Duration,
};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

const MAX_RUN_STEPS: usize = 32;
const MOUSE_SCROLL_LINES: u16 = 1;

#[derive(Debug)]
pub enum Action {
    Step,
    Run,
    Approve,
    ApprovePolicy,
    ApprovePermission,
    DenyPermission,
    Permissions,
    RevokePermission(String),
    Accept,
    Reject,
    NoKnowledge,
    Plan,
    Cancel,
    Resume,
    Answer(String),
}

fn can_advance(phase: &Phase) -> bool {
    matches!(phase, Phase::Planning | Phase::Working | Phase::Verifying)
}

/// Shared dispatch for both frontends; only explicit human input reaches this function.
pub async fn execute(runner: &mut Runner, action: Action) -> Result<()> {
    match action {
        Action::Step => runner.advance().await,
        Action::Run => {
            for _ in 0..MAX_RUN_STEPS {
                if !can_advance(&runner.task.phase) {
                    break;
                }
                runner.advance().await?;
            }
            Ok(())
        }
        Action::Approve => runner.approve_plan().await,
        Action::ApprovePolicy => runner.approve_policy().await,
        Action::ApprovePermission => {
            // Headless approval keeps its one-call meaning: grant, then run
            // the approved command as the next step.
            runner.approve_permission().await?;
            runner.advance().await
        }
        Action::DenyPermission => runner.deny_permission(),
        Action::Permissions => Ok(()),
        Action::RevokePermission(id) => runner.revoke_permission(&id),
        Action::Accept => runner.review(true).await,
        Action::Reject => runner.review(false).await,
        Action::NoKnowledge => runner.confirm_no_knowledge().await,
        Action::Plan => runner.mode_plan().await,
        Action::Cancel => runner.cancel().await,
        Action::Resume => runner.resume().await,
        Action::Answer(text) => runner.answer(text).await,
    }
}

struct Screen {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}
static SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

fn restore_terminal() {
    if SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
        if KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}
impl Screen {
    fn enter() -> Result<Self> {
        anyhow::ensure!(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            "tui requires an interactive terminal"
        );
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal();
                previous(info);
            }));
        });
        enable_raw_mode()?;
        SCREEN_ACTIVE.store(true, Ordering::SeqCst);
        let result = (|| {
            execute!(io::stdout(), EnterAlternateScreen)?;
            if matches!(
                crossterm::terminal::supports_keyboard_enhancement(),
                Ok(true)
            ) {
                execute!(
                    io::stdout(),
                    PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
                )?;
                KEYBOARD_ENHANCEMENT_ACTIVE.store(true, Ordering::SeqCst);
            }
            execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
            Ok(Self {
                terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
            })
        })();
        if result.is_err() {
            restore_terminal();
        }
        result
    }
}

fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    // Disambiguation deliberately preserves legacy Enter. Reporting every key
    // is what lets the terminal encode Shift-Enter as CSI 13;2u; alternate keys
    // preserve the shifted text for ordinary characters in that mode.
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore_terminal();
    }
}

#[derive(Default, Debug)]
struct Composer {
    text: String,
    cursor: usize,
}
impl Composer {
    fn insert(&mut self, value: &str) {
        let value = visible(value);
        self.text.insert_str(self.cursor, &value);
        self.cursor += value.len();
    }
    fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(index, _)| index)
                .unwrap_or(0);
        }
    }
    fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }
    fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.text.drain(self.cursor..end);
    }
    fn delete(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text.drain(self.cursor..self.cursor + c.len_utf8());
        }
    }
    fn submit(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
    fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }
    fn end(&mut self) {
        self.cursor += self.text[self.cursor..]
            .find('\n')
            .unwrap_or(self.text.len() - self.cursor);
    }
    fn vertical(&mut self, down: bool) {
        let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let column = self.text[start..self.cursor].chars().count();
        let target = if down {
            let Some(end) = self.text[self.cursor..].find('\n') else {
                return;
            };
            let start = self.cursor + end + 1;
            let end = start
                + self.text[start..]
                    .find('\n')
                    .unwrap_or(self.text.len() - start);
            start..end
        } else {
            if start == 0 {
                return;
            }
            let end = start - 1;
            let start = self.text[..end].rfind('\n').map_or(0, |i| i + 1);
            start..end
        };
        self.cursor = target.start
            + self.text[target.clone()]
                .char_indices()
                .nth(column)
                .map_or(target.len(), |(i, _)| i);
    }
    fn position(&self, width: usize) -> (usize, usize) {
        let width = width.max(1);
        let mut row = 0;
        let mut col = 0;
        for c in self.text[..self.cursor].chars() {
            if c == '\n' {
                row += 1;
                col = 0;
                continue;
            }
            let size = if c == '\t' { 4 } else { c.width().unwrap_or(0) };
            if col + size > width {
                row += 1;
                col = 0;
            }
            col += size;
        }
        if col == width {
            row += 1;
            col = 0;
        }
        (row, col)
    }
}

#[derive(Debug, Clone, Copy)]
struct KnowledgeHeader {
    sequence: u64,
    start: usize,
    end: usize,
}

#[derive(Default)]
struct View {
    tab: usize,
    scroll: u16,
    scroll_max: u16,
    follow: bool,
    composer: Composer,
    tick: usize,
    activity_expanded: bool,
    notice: String,
    retained_inputs: VecDeque<String>,
    knowledge_collapsed: BTreeSet<u64>,
    knowledge_headers: Vec<KnowledgeHeader>,
    knowledge_selected: Option<u64>,
    knowledge_latest: Option<u64>,
    knowledge_reveal_selection: bool,
    content_area: Rect,
    line_count: usize,
    /// Content rows that continue the row before them (hard-wrap breaks).
    soft_breaks: BTreeSet<usize>,
    /// Where the left button went down; a selection begins once it moves.
    press: Option<Pos>,
    selection: Option<Selection>,
    copy_pending: bool,
    copy_request: Option<String>,
}

fn visible(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

fn gate(task: &Task, standing: &[String]) -> String {
    match task.phase {
        Phase::AwaitingPlan => task.plan.as_ref().map(|plan| {
            let mut text = format!("PLAN · human approval required\n{}\nFiles: {}\nChecks:\n{}", plan.summary, plan.files.join(", "), plan.checks.join("\n"));
            if !task.permission_grants.is_empty() || !standing.is_empty() {
                // Grants outlive a replan, so re-approval must not hide them,
                // and standing paths are in force whether or not any exist.
                text.push_str(&format!(
                    "\nActive sandbox grants: {} task · {} standing · /permissions lists them",
                    task.permission_grants.len(),
                    standing.len()
                ));
            }
            text.push_str(&format!("\n{SYMBOLIC_APPROVAL}\n/approve to execute · send feedback to revise"));
            text
        }).unwrap_or_default(),
        Phase::AwaitingSpecApproval => task
            .pending_spec
            .as_ref()
            .map(|pending| spec_approval_gate(&pending.preview, &pending.uncited))
            .unwrap_or_else(|| "SPEC APPROVAL · preview unavailable; run /approve-spec <path> again.".into()),
        Phase::AwaitingPolicy => format!("EDIT APPROVAL · {}\n{}\nView Diff and Review (Tab), then /approve or send feedback.",task.pending_edit.as_ref().map(|e|e.file.as_str()).unwrap_or("pending edit"),task.pending_edit.as_ref().map(|e|e.reason.as_str()).unwrap_or("")),
        Phase::AwaitingPermission => task
            .pending_permission
            .as_ref()
            .map(|pending| permission_gate(pending, standing))
            .unwrap_or_else(|| {
                "PERMISSION REQUEST · details unavailable; deny it and retry the task.".into()
            }),
        Phase::AwaitingReview => format!("KNOWLEDGE REVIEW · {} operation(s)\n{}\nView Review (Tab) · /accept [operation] · /reject [operation] · /no-knowledge",task.reviews.len(),task.capture_reason.as_deref().unwrap_or("Review the captured evidence before completion.")),
        Phase::AwaitingInput => if task.turn_finished {
            "Your turn. Ask a follow-up or describe the next change.".into()
        } else if task.last_response.is_empty() {
            "Your input is needed. Reply below.".into()
        } else {
            // A question, an exhausted repair, or an interrupted action: show
            // what is being waited on, not only that something is.
            format!("INPUT NEEDED\n{}\nReply with guidance (the task returns to Plan for re-approval), or /plan.", task.last_response)
        },
        Phase::Cancelled => "Interrupted; obligations are saved. /continue resumes, or send follow-up guidance.".into(),
        Phase::Complete => "Task complete. Describe the next request to continue this conversation.".into(),
        _ => String::new(),
    }
}

fn permission_gate(request: &super::runner::PendingPermission, standing: &[String]) -> String {
    let mut text = format!(
        "{}\nRequest: {}\nCommand:\n  {}\nReason:\n  {}\n",
        if request.is_refused() {
            "PERMISSION REQUEST · cannot be granted as asked"
        } else {
            "PERMISSION REQUEST · human approval required"
        },
        request.request_id,
        request.command.replace('\n', "\n  "),
        request.justification.replace('\n', "\n  ")
    );
    if request.read_paths.is_empty() {
        text.push_str("Read access: none\n");
    } else {
        text.push_str(&format!(
            "Read access:\n  {}\n",
            request.read_paths.join("\n  ")
        ));
    }
    if request.write_paths.is_empty() {
        text.push_str("Write access: none\n");
    } else {
        text.push_str(&format!(
            "Write access (create, modify, and delete):\n  {}\n",
            request.write_paths.join("\n  ")
        ));
    }
    text.push_str(&format!(
        "Network: {}\n",
        if request.network {
            "enabled"
        } else {
            "disabled"
        }
    ));
    if !standing.is_empty() {
        // What is already ambient, so the human judges the delta and not the
        // whole surface.
        text.push_str(&format!(
            "Already granted standing (every task): {}\n",
            standing.join(", ")
        ));
    }
    for line in request.findings.lines() {
        text.push_str(&format!("Note: {line}\n"));
    }
    // A refused request is still shown: the human sees what was asked and why
    // it was turned down, instead of the task dying silently.
    match &request.refusal {
        Some(refusal) => text.push_str(&format!(
            "Refused:\n  {}\n/deny dismisses it and tells the model to ask for something narrower",
            refusal.replace('\n', "\n  ")
        )),
        None => text.push_str(
            "/approve grants this access for the current task and runs the command · /deny refuses it",
        ),
    }
    text
}

fn spec_approval_gate(preview: &SpecPrepareResponse, uncited: &[SpecUncited]) -> String {
    let mut text = format!(
        "SPEC APPROVAL · human approval required\nSource: {}\nSHA-256: {}\nKnowledge revision: {}\n",
        preview.path, preview.source_sha256, preview.knowledge_revision
    );
    if preview.already_approved {
        text.push_str("\nUNCHANGED · this source and active record set are already approved\n");
    }
    match &preview.component {
        Some(plan) => text.push_str(&format!(
            "\nCOMPONENT · {} · {}\nCovers: {}\nIRI: {}\nEvery record below concerns this component; its Constraints govern the files it covers.\n",
            plan.name,
            if plan.new {
                "NEW".to_string()
            } else if plan.added.is_empty() {
                "existing".to_string()
            } else {
                format!("existing, adding {}", plan.added.join(" "))
            },
            plan.covers.join(" "),
            plan.iri
        )),
        // Floating records are findable only by lexical luck: say so before
        // the human accepts, and name the command that anchors them.
        None => text.push_str(&format!(
            "\nCOMPONENT · none · the records below will not be linked to any component or code, so their Constraints will not govern implementation. To anchor them, run /approve-spec {} <dir/ | file | .> instead (an unchanged batch is re-approved with the link).\n",
            preview.path
        )),
    }
    for entry in &preview.entries {
        let (effect, identity) = match &entry.disposition {
            SpecDisposition::New { iri } => ("NEW", iri.clone()),
            SpecDisposition::Reuse { iri } => ("REUSE", iri.clone()),
            SpecDisposition::Supersede { iri, previous_iri } => {
                ("SUPERSEDE", format!("{previous_iri} -> {iri}"))
            }
        };
        text.push_str(&format!(
            "\n{effect} · {} · {}\nExtracted claim: {}\nIRI: {identity}\n",
            entry.draft.kind, entry.draft.title, entry.draft.description
        ));
        text.push_str("Evidence:\n");
        for evidence in &entry.draft.evidence {
            text.push_str(&format!("  {evidence}\n"));
        }
        if let Some(existing) = &entry.existing {
            text.push_str(&format!(
                "Existing accepted record: {} · {}\nExisting claim and evidence:\n{}\n",
                existing.title, existing.iri, existing.description
            ));
        }
    }
    for retirement in &preview.retirements {
        let effect = match retirement.disposition {
            SpecRetirementDisposition::Retract => "RETRACT",
            SpecRetirementDisposition::RetainShared => "RETAIN SHARED",
        };
        text.push_str(&format!(
            "\n{effect} · {} · {}\nIRI: {}\nExisting claim and evidence:\n{}\n",
            retirement.kind, retirement.title, retirement.iri, retirement.description
        ));
    }
    if let Some(previous) = &preview.previous_approval_iri {
        let effect = if preview.already_approved {
            "CURRENT APPROVAL"
        } else {
            "SUPERSEDE PRIOR APPROVAL"
        };
        text.push_str(&format!("\n{effect}\nIRI: {previous}\n"));
    }
    // What the batch leaves out is as much a part of the judgment as what it
    // holds: a section no record cites will never govern anything.
    if !uncited.is_empty() {
        text.push_str(&format!(
            "\nUNCITED · {} range(s) of {} no record above cites; they will not become project knowledge:\n",
            uncited.len(),
            preview.path
        ));
        for range in uncited {
            text.push_str(&format!("  {}\n", range.describe()));
        }
    }
    text.push_str(&format!(
        "\nApproval marker: spec-approval: {}\n/approve-spec or ‘I approve the spec’ records this exact batch.\nA separate /approve is still required before code execution.",
        preview.path
    ));
    text
}

fn push_plain_lines(lines: &mut Vec<Line<'static>>, value: &str, style: Style) {
    for line in visible(value).split('\n') {
        lines.push(Line::from(Span::styled(line.to_owned(), style)));
    }
}

fn push_plain_section(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    label_style: Style,
    value: &str,
    body_style: Style,
) {
    lines.push(Line::from(Span::styled(label.to_owned(), label_style)));
    push_plain_lines(lines, value, body_style);
    lines.push(Line::default());
}

fn push_assistant(lines: &mut Vec<Line<'static>>, value: &str, streaming: bool) {
    lines.push(Line::from(Span::styled(
        "🫎 MOOSEDev",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    let mut response = markdown::render(value, Style::default().fg(Color::LightCyan));
    if streaming {
        if response.lines.is_empty() {
            response.lines.push(Line::default());
        }
        response.lines.last_mut().unwrap().spans.push(Span::styled(
            "▌",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.extend(response.lines);
    lines.push(Line::default());
}

fn conversation_body(snapshot: &Snapshot, view: &View) -> Text<'static> {
    let mut lines = Vec::new();
    for message in &snapshot.conversation.messages {
        if message.role == "activity" && view.tab == 0 && !view.activity_expanded {
            continue;
        }
        match message.role.as_str() {
            "user" => push_plain_section(
                &mut lines,
                "YOU",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                &message.text,
                Style::default(),
            ),
            "assistant" => push_assistant(&mut lines, &message.text, false),
            "activity" => push_plain_section(
                &mut lines,
                "ACTIVITY",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                &message.text,
                Style::default().fg(Color::DarkGray),
            ),
            _ => push_plain_section(
                &mut lines,
                "SESSION",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                &message.text,
                Style::default().fg(Color::DarkGray),
            ),
        }
    }
    let (live_assistant, live_command) = {
        let live = snapshot.live.lock().unwrap();
        (live.assistant.clone(), live.command.clone())
    };
    if !live_assistant.is_empty() {
        push_assistant(&mut lines, &live_assistant, true);
    }
    if !live_command.is_empty() {
        push_plain_section(
            &mut lines,
            "COMMAND OUTPUT",
            Style::default()
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD),
            &live_command,
            Style::default().fg(Color::DarkGray),
        );
    }
    let mut control = String::new();
    if let Some(task) = &snapshot.task {
        control.push_str(&gate(task, &snapshot.standing_read_paths));
        if !task.reviews.is_empty() && task.phase != Phase::AwaitingReview {
            control.push_str(&format!(
                "\n\n{} knowledge review(s) pending · /review or view Review",
                task.reviews.len()
            ));
        }
    }
    if !snapshot.conversation.queued.is_empty() {
        control.push_str(&format!(
            "\n\nQUEUED · {} message(s), delivered before the next action",
            snapshot.conversation.queued.len()
        ));
    }
    if !control.is_empty() {
        push_plain_lines(&mut lines, &control, Style::default().fg(Color::Yellow));
    }
    Text::from(lines)
}

fn body(snapshot: &Snapshot, view: &View) -> Text<'static> {
    if matches!(view.tab, 0 | 1) {
        return conversation_body(snapshot, view);
    }
    let mut text = String::new();
    match view.tab {
        0 | 1 => unreachable!(),
        2 => {
            if let Some(task) = &snapshot.task {
                if let Some(edit) = &task.pending_edit {
                    text.push_str(&edit_diff(edit, "PENDING APPROVAL"));
                    text.push_str("/approve authorizes this exact edit.\n\n");
                }
                for edit in task.edits.iter().rev() {
                    text.push_str(&edit_diff(edit, "APPLIED"));
                    text.push('\n');
                }
            }
            if text.is_empty() {
                text.push_str(
                    "No edits yet. Pending approvals and completed changes will appear here.",
                );
            }
        }
        3 => {
            if let Some(task) = &snapshot.task {
                text.push_str(&legacy_knowledge_text(task));
            } else {
                text.push_str("No active task. Graph context appears here after work begins.");
            }
        }
        4 => {
            if let Some(task) = &snapshot.task {
                for (index, review) in task.reviews.iter().enumerate() {
                    text.push_str(&format!("REVIEW {}\n{}\n", index + 1, review.reason));
                    if let Some(associations) = &review.intent_links {
                        text.push_str(&format!(
                            "\nCode associations · {}\n",
                            associations.operation_id
                        ));
                        match derived_page(task, &associations.operation_id) {
                            Some(page) => text.push_str(&link_review(page)),
                            None => {
                                for binding in &associations.bindings {
                                    text.push_str(&format!(
                                        "{}\n{} · {}\n\n",
                                        binding.record_iri, binding.file, binding.symbol
                                    ));
                                }
                                text.push_str("Derivation detail unavailable\n");
                            }
                        }
                        text.push_str("Review each record's relevance to its target; acceptance is not proof of correctness.\n");
                    }
                    text.push_str(&final_review(task, review));
                    for proposal in &review.request.proposals {
                        text.push_str(&format!(
                            "\n{} · {}\n{}\n\nEvidence\n{}\n",
                            proposal.kind,
                            proposal.title,
                            proposal.description,
                            proposal.evidence.join("\n")
                        ));
                        if !proposal.files.is_empty() {
                            text.push_str(&format!("Files: {}\n", proposal.files.join(", ")));
                        }
                        if !proposal.components.is_empty() {
                            text.push_str(&format!(
                                "Components: {}\n",
                                proposal.components.join(", ")
                            ));
                        }
                        // Derived from the approved plan's obligations. Shown
                        // because accepting the note accepts these edges too,
                        // and neither was rendered before — isMotivatedBy has
                        // been writable since the field existed and a reviewer
                        // could never see it.
                        if let Some(record) = &proposal.requirement {
                            text.push_str(&format!("Motivated by: {record}\n"));
                        }
                        if let Some(record) = &proposal.learned_from {
                            text.push_str(&format!("Learned from: {record}\n"));
                        }
                        if let Some(record) = &proposal.supersedes {
                            text.push_str(&format!("Replaces: {record}\n"));
                        }
                        if let Some(record) = &proposal.retracts {
                            text.push_str(&format!("Retracts: {record}\n"));
                        }
                    }
                    for record in &review.response.proposals {
                        if !record.unanchored.is_empty() {
                            text.push_str(&format!(
                                "Unresolved code links: {}\n",
                                record.unanchored.join(", ")
                            ));
                        }
                    }
                    text.push_str(&format!(
                        "\n/accept {} · /reject {}\n\n",
                        index + 1,
                        index + 1
                    ));
                }
                text.push_str(&symbolic_scope(task));
                text.push_str(&format!(
                    "Current assessment\n{}\n",
                    task.capture_reason
                        .as_deref()
                        .unwrap_or("No pending assessment")
                ));
                if task.capture_request.is_some() {
                    text.push_str("A capture request is pending; its exact evidence is preserved in Journal.\n");
                }
                text.push_str("\nVerification\n");
                for check in &task.check_results {
                    text.push_str(&format!(
                        "{} · {}\n{}\n",
                        if check.success { "PASS" } else { "FAIL" },
                        check.command,
                        check.output
                    ));
                }
                text.push_str("\n/accept or /reject reviews all displayed operations.\n/no-knowledge confirms a consolidated no-change assessment.");
            } else {
                text.push_str("Knowledge assessments appear here as work progresses.");
            }
        }
        _ => {
            text = journal_summary(snapshot);
        }
    }
    Text::raw(visible(&text))
}

fn sync_knowledge_state(view: &mut View, task: &Task) {
    let sequences: BTreeSet<_> = task
        .knowledge_turns
        .iter()
        .map(|turn| turn.sequence)
        .collect();
    view.knowledge_collapsed
        .retain(|sequence| sequences.contains(sequence));
    let latest = task.knowledge_turns.last().map(|turn| turn.sequence);
    if latest != view.knowledge_latest {
        if let Some(latest) = latest {
            view.knowledge_collapsed.extend(
                sequences
                    .iter()
                    .copied()
                    .filter(|sequence| *sequence != latest),
            );
            view.knowledge_collapsed.remove(&latest);
            view.knowledge_selected = Some(latest);
            view.knowledge_reveal_selection = true;
        }
        view.knowledge_latest = latest;
    }
    if view
        .knowledge_selected
        .is_some_and(|sequence| !sequences.contains(&sequence))
    {
        view.knowledge_selected = latest;
    }
}

fn push_wrapped_text(
    lines: &mut Vec<Line<'static>>,
    soft_breaks: &mut BTreeSet<usize>,
    text: Text<'static>,
    width: usize,
) -> (usize, usize) {
    let start = lines.len();
    let (wrapped, continues) = markdown::wrap_rows(text, width.max(1));
    soft_breaks.extend(
        continues
            .iter()
            .enumerate()
            .filter_map(|(row, continues)| continues.then_some(start + row)),
    );
    lines.extend(wrapped.lines);
    (start, lines.len())
}

fn record_color(kind: &str) -> Color {
    match kind {
        "Constraint" => Color::Red,
        "Requirement" => Color::Yellow,
        "ArchitecturalDecision" => Color::Magenta,
        "Lesson" => Color::Green,
        "Pattern" => Color::Blue,
        "AntiPattern" => Color::LightRed,
        _ => Color::Cyan,
    }
}

fn push_record_cards(
    lines: &mut Vec<Line<'static>>,
    soft_breaks: &mut BTreeSet<usize>,
    records: &[super::protocol::ContextRecord],
    width: usize,
) {
    for record in records {
        push_wrapped_text(
            lines,
            soft_breaks,
            Text::from(Line::from(vec![
                Span::styled(
                    format!("[{}]", visible(&record.kind)),
                    Style::default()
                        .fg(record_color(&record.kind))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", visible(&record.title)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ])),
            width,
        );
        if record.claim.trim().is_empty() {
            push_wrapped_text(
                lines,
                soft_breaks,
                Text::from(Line::from(Span::styled(
                    "Claim not supplied by this retrieval.",
                    Style::default().fg(Color::DarkGray),
                ))),
                width,
            );
        } else {
            let claim = markdown::render(
                &visible(record.claim.trim()),
                Style::default().fg(Color::White),
            );
            push_wrapped_text(lines, soft_breaks, claim, width);
        }
        for source in &record.provenance {
            push_wrapped_text(
                lines,
                soft_breaks,
                Text::from(Line::from(Span::styled(
                    format!("via · {}", visible(source)),
                    Style::default().fg(Color::DarkGray),
                ))),
                width,
            );
        }
        push_wrapped_text(
            lines,
            soft_breaks,
            Text::from(Line::from(Span::styled(
                visible(&record.iri),
                Style::default().fg(Color::DarkGray),
            ))),
            width,
        );
        lines.push(Line::default());
    }
}

fn knowledge_body(task: &Task, view: &mut View, width: usize) -> Text<'static> {
    sync_knowledge_state(view, task);
    view.knowledge_headers.clear();
    if task.knowledge_turns.is_empty() {
        let mut lines = Vec::new();
        push_wrapped_text(
            &mut lines,
            &mut view.soft_breaks,
            Text::raw(visible(&legacy_knowledge_text(task))),
            width,
        );
        return Text::from(lines);
    }

    let mut lines = Vec::new();
    for (index, turn) in task.knowledge_turns.iter().enumerate() {
        let collapsed = view.knowledge_collapsed.contains(&turn.sequence);
        let record_count = turn.records.len()
            + turn
                .searches
                .iter()
                .map(|search| search.records.len())
                .sum::<usize>();
        let selected = view.knowledge_selected == Some(turn.sequence);
        let header_style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        };
        let (start, end) = push_wrapped_text(
            &mut lines,
            &mut view.soft_breaks,
            Text::from(Line::from(vec![
                Span::styled(if collapsed { "▶ " } else { "▼ " }, header_style),
                Span::styled(
                    format!(
                        "Query {} · {} record{} — ",
                        index + 1,
                        record_count,
                        if record_count == 1 { "" } else { "s" }
                    ),
                    header_style,
                ),
                Span::styled(visible(&turn.query), header_style),
            ])),
            width,
        );
        view.knowledge_headers.push(KnowledgeHeader {
            sequence: turn.sequence,
            start,
            end,
        });
        if collapsed {
            lines.push(Line::default());
            continue;
        }

        let revision = &turn.revision[..turn.revision.len().min(12)];
        let mut detail = format!("revision {revision}");
        if !turn.files.is_empty() {
            detail.push_str(&format!(" · files {}", turn.files.join(", ")));
        }
        push_wrapped_text(
            &mut lines,
            &mut view.soft_breaks,
            Text::from(Line::from(Span::styled(
                visible(&detail),
                Style::default().fg(Color::DarkGray),
            ))),
            width,
        );
        lines.push(Line::default());
        if turn.records.is_empty() {
            push_wrapped_text(
                &mut lines,
                &mut view.soft_breaks,
                Text::from(Line::from(Span::styled(
                    "No accepted project knowledge matched this turn.",
                    Style::default().fg(Color::DarkGray),
                ))),
                width,
            );
            lines.push(Line::default());
        } else {
            push_record_cards(&mut lines, &mut view.soft_breaks, &turn.records, width);
        }

        for (search_index, search) in turn.searches.iter().enumerate() {
            push_wrapped_text(
                &mut lines,
                &mut view.soft_breaks,
                Text::from(Line::from(Span::styled(
                    format!(
                        "Model search {} · {} — {}",
                        search_index + 1,
                        search.records.len(),
                        visible(&search.query)
                    ),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))),
                width,
            );
            lines.push(Line::default());
            if search.records.is_empty() {
                push_wrapped_text(
                    &mut lines,
                    &mut view.soft_breaks,
                    Text::from(Line::from(Span::styled(
                        "No accepted project knowledge matched this search.",
                        Style::default().fg(Color::DarkGray),
                    ))),
                    width,
                );
                lines.push(Line::default());
            } else {
                push_record_cards(&mut lines, &mut view.soft_breaks, &search.records, width);
            }
        }
    }
    Text::from(lines)
}

fn legacy_knowledge_text(task: &Task) -> String {
    let mut text = String::new();
    text.push_str("CURRENT WORKING CONTEXT\n");
    if let Some(context) = &task.knowledge_context {
        text.push_str(&format!(
            "Topic: {}\nRevision: {}\n\nACCEPTED PROJECT KNOWLEDGE\n",
            context.topic, context.revision
        ));
        if context.context.trim().is_empty() {
            text.push_str("No accepted project knowledge matched.\n");
        } else {
            text.push_str(context.context.trim());
            text.push('\n');
        }
        text.push_str("\nPROJECT RULES\n");
        if context.governing_rules.is_empty() {
            text.push_str("No governing constraints for the current working set.\n");
        }
        for rule in &context.governing_rules {
            text.push_str(&format!(
                "{}\n{}\nvia: {}\n",
                rule.label, rule.iri, rule.via
            ));
            if !rule.claim.is_empty() {
                text.push_str(&rule.claim);
                text.push('\n');
            }
            text.push('\n');
        }
        text.push_str("FILE DOSSIERS\n");
        if context.files.is_empty() {
            text.push_str("No files are in the current working set.\n");
        }
        for file in &context.files {
            text.push_str(&format!("\n{}\n", file.file));
            if file.dossier.trim().is_empty() {
                text.push_str("No recorded knowledge is linked to this file.\n");
            } else {
                text.push_str(file.dossier.trim());
                text.push('\n');
            }
        }
    } else {
        text.push_str("No graph context has been retrieved for this task yet.\n");
    }

    text.push_str("\nEXPLICIT SEARCH HISTORY\n");
    if task.knowledge_searches.is_empty() {
        text.push_str("No explicit graph searches yet.\n");
    }
    for (index, search) in task.knowledge_searches.iter().enumerate() {
        text.push_str(&format!(
            "\nSEARCH {} · {} accepted record(s)\nQuery: {}\nRevision: {}\n",
            index + 1,
            search.evidence_iris.len(),
            search.query,
            search.revision
        ));
        if let Some(receipt) = &search.delivery_receipt {
            text.push_str(&format!(
                "Delivery: {} bytes{}\n",
                receipt.context_bytes,
                receipt
                    .max_bytes
                    .map(|max| format!(" of {max}"))
                    .unwrap_or_else(|| " (unbounded)".into())
            ));
            for record in &receipt.records {
                text.push_str(&format!(
                    "- {}: {} ({}) — {}\n",
                    record.tier.as_str(),
                    record.kind,
                    record.iri,
                    record.reason
                ));
            }
        }
        if search.context.trim().is_empty() {
            text.push_str("No accepted project knowledge matched.\n");
        } else {
            text.push_str(search.context.trim());
            text.push('\n');
        }
    }
    text
}

const SYMBOLIC_APPROVAL: &str = "On /approve the harness derives obligations from the plan files' governing records; no model call";

/// Record class from a `/kg/<Class>/<id>` IRI; the journal stores IRIs only.
fn record_kind(iri: &str) -> &str {
    iri.rsplit('/').nth(1).unwrap_or("record")
}

/// What plan approval derived and how much autonomy the task has spent, all
/// from the journal.
fn symbolic_scope(task: &Task) -> String {
    let Some(symbolic) = &task.symbolic else {
        return String::new();
    };
    let mut text = String::from("Derived scope\n");
    if symbolic.obligations.is_empty() {
        text.push_str("No approved plan scope derived yet.\n");
    }
    for (file, records) in &symbolic.obligations {
        if records.is_empty() {
            text.push_str(&format!("{file} · ungoverned\n"));
            continue;
        }
        text.push_str(&format!("{file}\n"));
        for iri in records {
            text.push_str(&format!("  {} · {iri}\n", record_kind(iri)));
        }
    }
    if let Some(scope) = &task.approved_change_scope {
        for definition in &scope.definition_scopes {
            text.push_str(&format!(
                "  def {} · {}\n",
                definition.file, definition.symbol
            ));
        }
    }
    text.push_str(&format!(
        "Revision {} · obligations {}\nScope escapes {} · no-op continuations {} · retypes {}\n",
        symbolic.knowledge_revision,
        &symbolic.obligations_digest[..symbolic.obligations_digest.len().min(12)],
        symbolic.scope_escapes,
        symbolic.noop_continuations,
        symbolic.retypes
    ));
    for check in &symbolic.check_history {
        text.push_str(&format!(
            "  {} · {}{}\n",
            if check.success { "PASS" } else { "FAIL" },
            check.command,
            if check.after_edit {
                " · after edit"
            } else {
                ""
            }
        ));
    }
    text.push('\n');
    text
}

/// The derived association page behind a link review, when the journal still
/// holds the batch that produced it.
fn derived_page<'a>(task: &'a Task, operation_id: &str) -> Option<&'a AssociatePage> {
    task.symbolic
        .as_ref()?
        .association
        .as_ref()
        .filter(|association| association.link_operation_id.as_deref() == Some(operation_id))
        .map(|association| &association.page)
}

fn link_review(page: &AssociatePage) -> String {
    let mut text = String::new();
    for binding in &page.bindings {
        text.push_str(&format!(
            "{} · {}\n{} · {} ({}) · line {}\n-{}-> {}\n\n",
            binding.record_kind,
            binding.record_iri,
            binding.file,
            binding.name.as_deref().unwrap_or(&binding.symbol),
            binding.kind.as_deref().unwrap_or("definition"),
            binding.definition_range.start.line + 1,
            binding.predicate,
            match binding.basis {
                DerivedBasis::Obligation => "plan obligation",
                DerivedBasis::FileDossier => "sibling definition's dossier",
            }
        ));
    }
    if !page.skipped.is_empty() {
        let mut counts = std::collections::BTreeMap::new();
        for skipped in &page.skipped {
            *counts
                .entry(format!("{:?}", skipped.reason))
                .or_insert(0usize) += 1;
        }
        text.push_str("Skipped: ");
        text.push_str(
            &counts
                .iter()
                .map(|(reason, count)| format!("{reason} {count}"))
                .collect::<Vec<_>>()
                .join(" · "),
        );
        text.push('\n');
    }
    if !page.ungoverned.is_empty() {
        text.push_str(&format!("Ungoverned: {}\n", page.ungoverned.join(", ")));
    }
    for unresolved in &page.unresolved {
        text.push_str(&format!(
            "Unresolved: {} · {}\n",
            unresolved.file, unresolved.reason
        ));
    }
    text
}

/// The final review card: the model's one note and how the daemon typed it.
fn final_review(task: &Task, review: &ReviewItem) -> String {
    let Some(note) = task
        .symbolic
        .as_ref()
        .and_then(|symbolic| symbolic.capture_note.as_ref())
        .filter(|note| note.capture_operation_id == review.request.operation_id)
    else {
        return String::new();
    };
    let mut text = format!("\nCapture note\n{}\n", note.note);
    let Some(typed) = &note.response else {
        return text;
    };
    text.push_str(&format!(
        "Typing: {}{}\n",
        match typed.typing_mode {
            TypingMode::SymbolicOnly => "symbolic only",
            TypingMode::Sensor => "symbolic with sensor",
        },
        typed
            .typing_note
            .as_deref()
            .map(|note| format!(" · {note}"))
            .unwrap_or_default()
    ));
    for proposal in &typed.proposals {
        text.push_str(&format!(
            "{:?} · {} · {} — {}\n",
            proposal.origin,
            proposal.proposal.kind,
            proposal.proposal.title,
            match &proposal.disposition {
                TypedDisposition::Restates { candidate_iri, .. } =>
                    format!("restates {candidate_iri}; no record proposed"),
                TypedDisposition::Refines {
                    candidate_iri,
                    confidence,
                    ..
                } => format!("refines {candidate_iri} ({confidence:.2})"),
                TypedDisposition::Distinct { .. } => "new record".into(),
            }
        ));
    }
    text
}

fn excerpt(value: &str) -> String {
    let mut chars = value.chars();
    let mut text: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        text.push_str("… [full content in journal]");
    }
    text
}

fn journal_summary(snapshot: &Snapshot) -> String {
    let conversation = &snapshot.conversation;
    let mut text = format!(
        "CONVERSATION {}\n{} messages · {} queued · {} tasks\nFile: {}\n",
        conversation.id,
        conversation.messages.len(),
        conversation.queued.len(),
        conversation.tasks.len(),
        conversation
            .root
            .join(format!(
                ".moosedev/harness/conversations/{}.json",
                conversation.id
            ))
            .display()
    );
    if let Some(task) = &snapshot.task {
        text.push_str(&format!("\nTASK {} · {:?} · {:?}\n{}\nFile: {}\n{} events · {} model requests · {} edits · {} pending reviews\n\nRecent events (bounded preview)\n", task.id, task.mode, task.phase, excerpt(&task.objective), task.root.join(format!(".moosedev/harness/tasks/{}.json", task.id)).display(), task.events.len(), task.model_requests.len(), task.edits.len(), task.reviews.len()));
        for event in task.events.iter().rev().take(30).rev() {
            text.push_str(&excerpt(&event.message));
            text.push('\n');
        }
        text.push_str(
            "\nRecent model requests (full prompts and responses remain in the journal)\n",
        );
        for request in task.model_requests.iter().rev().take(10).rev() {
            text.push_str(&format!(
                "{} · {}\n",
                excerpt(request["purpose"].as_str().unwrap_or("model")),
                if request["interrupted"] == true {
                    "interrupted"
                } else if request["response"].is_string() {
                    "response saved"
                } else {
                    "pending / failed"
                }
            ));
        }
    }
    text
}

fn edit_diff(edit: &super::runner::PendingEdit, label: &str) -> String {
    let mut text = format!("{label} · {}\n{}\n\n", edit.file, edit.reason);
    for change in diff::lines(
        edit.before.as_deref().unwrap_or(""),
        edit.after.as_deref().unwrap_or(""),
    ) {
        match change {
            diff::Result::Left(line) => text.push_str(&format!("- {line}\n")),
            diff::Result::Right(line) => text.push_str(&format!("+ {line}\n")),
            diff::Result::Both(line, _) => text.push_str(&format!("  {line}\n")),
        }
    }
    text
}

fn wrapped(text: &str, width: usize) -> String {
    let width = width.max(1);
    let mut result = String::new();
    let mut col = 0;
    let text = text.replace('\t', "    ");
    for c in text.chars() {
        if c == '\n' {
            result.push(c);
            col = 0;
            continue;
        }
        let size = c.width().unwrap_or(0);
        if col + size > width {
            result.push('\n');
            col = 0;
        }
        result.push(c);
        col += size;
    }
    result
}

fn sync_scroll_bounds(view: &mut View, line_count: usize, viewport_height: u16) {
    view.scroll_max = line_count
        .saturating_sub(viewport_height as usize)
        .min(u16::MAX as usize) as u16;
    view.scroll = if view.follow {
        view.scroll_max
    } else {
        view.scroll.min(view.scroll_max)
    };
}

fn render(frame: &mut ratatui::Frame, snapshot: &Snapshot, view: &mut View) {
    let area = frame.area();
    let composer_rows = wrapped(&view.composer.text, area.width.saturating_sub(2) as usize)
        .lines()
        .count()
        .max(
            view.composer
                .position(area.width.saturating_sub(2) as usize)
                .0
                + 1,
        );
    let composer_height = (composer_rows.min(u16::MAX as usize - 2) as u16 + 2)
        .clamp(3, 8)
        .min(area.height.saturating_sub(5).max(1));
    let [header, main, status, composer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(composer_height),
    ])
    .areas(area);
    let phase = snapshot
        .task
        .as_ref()
        .map(|t| format!("{:?} · {:?}", t.mode, t.phase))
        .unwrap_or_else(|| "Conversation".into());
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" MOOSEDev ", Style::default().fg(Color::Cyan)),
                Span::raw(visible(&format!("{} · {}", snapshot.model, phase))),
            ]),
            Line::raw(visible(&format!(
                " {} · {}",
                snapshot.conversation.root.display(),
                snapshot.endpoint
            ))),
            Line::raw(format!(
                " {}  Conversation   Activity   Diff   Knowledge   Review   Journal · Tab switches",
                ["●", "◉", "◇", "◆", "□", "≡"][view.tab]
            )),
        ]),
        header,
    );
    let inner = Block::default().borders(Borders::ALL).inner(main);
    view.content_area = inner;
    view.soft_breaks.clear();
    let mut body = if view.tab == 3 {
        match &snapshot.task {
            Some(task) => knowledge_body(task, view, inner.width as usize),
            None => Text::raw("No active task. Graph context appears here after work begins."),
        }
    } else {
        view.knowledge_headers.clear();
        let text = body(snapshot, view);
        let mut lines = Vec::new();
        push_wrapped_text(
            &mut lines,
            &mut view.soft_breaks,
            text,
            inner.width as usize,
        );
        Text::from(lines)
    };
    let line_count = body.height();
    view.line_count = line_count;
    if let Some(selection) = view.selection {
        if std::mem::take(&mut view.copy_pending) {
            let rows: Vec<String> = body.lines.iter().map(ToString::to_string).collect();
            view.copy_request = Some(selection::text(&rows, &view.soft_breaks, selection));
        }
        for (index, line) in body.lines.iter_mut().enumerate() {
            if let Some((from, to)) = selection.columns(index) {
                *line = selection::highlight(std::mem::take(line), from, to);
            }
        }
    }
    let paragraph = Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(
        [
            " Conversation ",
            " Activity ",
            " Diff ",
            " Knowledge ",
            " Review ",
            " Journal ",
        ][view.tab],
    ));
    sync_scroll_bounds(view, line_count, main.height.saturating_sub(2));
    if view.tab == 3 && view.knowledge_reveal_selection {
        if let Some(header) = view
            .knowledge_headers
            .iter()
            .find(|header| Some(header.sequence) == view.knowledge_selected)
        {
            let viewport = inner.height as usize;
            let scroll = view.scroll as usize;
            if header.start < scroll {
                view.scroll = header.start.min(u16::MAX as usize) as u16;
            } else if header.end > scroll.saturating_add(viewport) {
                view.scroll = header.end.saturating_sub(viewport).min(u16::MAX as usize) as u16;
            }
            view.scroll = view.scroll.min(view.scroll_max);
        }
        view.knowledge_reveal_selection = false;
    }
    frame.render_widget(paragraph.scroll((view.scroll, 0)), main);
    let spinner = if snapshot.busy {
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][view.tick % 10]
    } else {
        "●"
    };
    let notice = if view.notice.is_empty() {
        snapshot.status.as_str()
    } else {
        view.notice.as_str()
    };
    let help = if view.tab == 3 {
        "Click query headers · drag selects and copies · Alt-↑/↓ select · Alt-←/→ collapse/expand · wheel/PageUp/PageDown scroll"
    } else {
        "Enter send · Ctrl-J newline · Alt/Shift-Enter when supported · Esc interrupt · /help · drag selects and copies · wheel/PageUp/PageDown scroll"
    };
    frame.render_widget(
        Paragraph::new(format!("{spinner} {}\n{help}", visible(notice))).style(
            Style::default().fg(if snapshot.busy {
                Color::Yellow
            } else {
                Color::Cyan
            }),
        ),
        status,
    );
    render_composer(frame, composer, &view.composer);
}

fn render_composer(frame: &mut ratatui::Frame, area: Rect, composer: &Composer) {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let (row, col) = composer.position(inner.width as usize);
    let scroll = row.saturating_sub(inner.height.saturating_sub(1) as usize);
    frame.render_widget(
        Paragraph::new(wrapped(&composer.text, inner.width as usize))
            .scroll((scroll as u16, 0))
            .block(Block::default().borders(Borders::ALL).title(" Message ")),
        area,
    );
    if inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position(Position::new(
            inner.x + col.min(inner.width.saturating_sub(1) as usize) as u16,
            inner.y + (row - scroll).min(inner.height.saturating_sub(1) as usize) as u16,
        ));
    }
}

fn move_knowledge_selection(view: &mut View, down: bool) {
    if view.knowledge_headers.is_empty() {
        return;
    }
    let current = view
        .knowledge_selected
        .and_then(|sequence| {
            view.knowledge_headers
                .iter()
                .position(|header| header.sequence == sequence)
        })
        .unwrap_or_else(|| {
            if down {
                0
            } else {
                view.knowledge_headers.len() - 1
            }
        });
    let next = if down {
        (current + 1).min(view.knowledge_headers.len() - 1)
    } else {
        current.saturating_sub(1)
    };
    view.knowledge_selected = Some(view.knowledge_headers[next].sequence);
    view.knowledge_reveal_selection = true;
    view.follow = false;
}

fn key(view: &mut View, key: KeyEvent) -> Option<Command> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    view.notice.clear();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Some(Command::Interrupt),
            KeyCode::Char('d') if view.composer.text.is_empty() => return Some(Command::Quit),
            KeyCode::Char('a') => view.composer.home(),
            KeyCode::Char('e') => view.composer.end(),
            KeyCode::Char('j') => view.composer.insert("\n"),
            KeyCode::Char('u') => {
                view.composer.text.clear();
                view.composer.cursor = 0;
            }
            _ => {}
        }
        return None;
    }
    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
        {
            view.composer.insert("\n")
        }
        KeyCode::Enter => {
            if view.composer.text.len() > 16_000 {
                view.notice = "Message exceeds 16000 bytes; shorten it before sending.".into();
                return None;
            }
            let text = view.composer.submit();
            if text.trim() == "/quit" {
                return Some(Command::Quit);
            }
            if text.trim() == "/expand" {
                view.activity_expanded = !view.activity_expanded;
                return None;
            }
            if !text.trim().is_empty() {
                view.follow = true;
                view.tab = 0;
                return Some(Command::Input(text));
            }
        }
        KeyCode::Esc => return Some(Command::Interrupt),
        KeyCode::Char(c) => view.composer.insert(&c.to_string()),
        KeyCode::Backspace => view.composer.backspace(),
        KeyCode::Delete => view.composer.delete(),
        KeyCode::Left if view.tab == 3 && key.modifiers.contains(KeyModifiers::ALT) => {
            if let Some(sequence) = view.knowledge_selected {
                view.knowledge_collapsed.insert(sequence);
                view.knowledge_reveal_selection = true;
            }
        }
        KeyCode::Right if view.tab == 3 && key.modifiers.contains(KeyModifiers::ALT) => {
            if let Some(sequence) = view.knowledge_selected {
                view.knowledge_collapsed.remove(&sequence);
                view.knowledge_reveal_selection = true;
            }
        }
        KeyCode::Left => view.composer.left(),
        KeyCode::Right => view.composer.right(),
        KeyCode::Home => view.composer.home(),
        KeyCode::End => view.composer.end(),
        KeyCode::PageUp => {
            view.follow = false;
            view.scroll = view.scroll.saturating_sub(12);
        }
        KeyCode::PageDown => {
            view.scroll = view.scroll.saturating_add(12).min(view.scroll_max);
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
            if view.tab == 3 {
                move_knowledge_selection(view, false);
            } else {
                view.follow = false;
                view.scroll = view.scroll.saturating_sub(1);
            }
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
            if view.tab == 3 {
                move_knowledge_selection(view, true);
            } else {
                view.scroll = view.scroll.saturating_add(1).min(view.scroll_max)
            }
        }
        KeyCode::Up => view.composer.vertical(false),
        KeyCode::Down => view.composer.vertical(true),
        KeyCode::Tab | KeyCode::BackTab => {
            let offset = if key.code == KeyCode::Tab { 1 } else { 5 };
            view.tab = (view.tab + offset) % 6;
            clear_selection(view);
            view.scroll = 0;
            view.follow = view.tab < 2;
        }
        _ => {}
    }
    None
}

fn clear_selection(view: &mut View) {
    view.press = None;
    view.selection = None;
    view.copy_pending = false;
}

/// The content position under the pointer, clamped into the pane so a drag
/// that leaves it keeps extending the selection along the nearest edge.
fn content_position(view: &View, event: &MouseEvent) -> Pos {
    let area = view.content_area;
    let row = event
        .row
        .clamp(area.y, area.bottom().saturating_sub(1).max(area.y));
    let column = event
        .column
        .clamp(area.x, area.right().saturating_sub(1).max(area.x));
    Pos {
        line: (view.scroll as usize + (row - area.y) as usize)
            .min(view.line_count.saturating_sub(1)),
        col: (column - area.x) as usize,
    }
}

fn toggle_knowledge_header(view: &mut View, line: usize) -> bool {
    let Some(header) = view
        .knowledge_headers
        .iter()
        .find(|header| line >= header.start && line < header.end)
        .copied()
    else {
        return false;
    };
    view.knowledge_selected = Some(header.sequence);
    if !view.knowledge_collapsed.remove(&header.sequence) {
        view.knowledge_collapsed.insert(header.sequence);
    }
    view.knowledge_reveal_selection = true;
    view.follow = false;
    true
}

fn mouse(view: &mut View, event: MouseEvent) -> bool {
    let area = view.content_area;
    let inside = event.column >= area.x
        && event.column < area.right()
        && event.row >= area.y
        && event.row < area.bottom();
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let had_selection = view.selection.is_some();
            clear_selection(view);
            if inside {
                view.press = Some(content_position(view, &event));
            }
            had_selection
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let Some(anchor) = view.press else {
                return false;
            };
            // Dragging past an edge scrolls toward it; the content must stop
            // following new output or the text would move under the pointer.
            let scroll = if event.row < area.y {
                view.scroll.saturating_sub(MOUSE_SCROLL_LINES)
            } else if event.row >= area.bottom() {
                view.scroll
                    .saturating_add(MOUSE_SCROLL_LINES)
                    .min(view.scroll_max)
            } else {
                view.scroll
            };
            let scrolled = scroll != view.scroll;
            view.scroll = scroll;
            let head = content_position(view, &event);
            let selection =
                (head != anchor || view.selection.is_some()).then_some(Selection { anchor, head });
            let changed = scrolled || selection != view.selection;
            if selection.is_some() {
                view.follow = false;
            }
            view.selection = selection;
            changed
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(press) = view.press.take() else {
                return false;
            };
            if view.selection.is_some() {
                // The rendered rows live in `render`; it fills `copy_request`.
                view.copy_pending = true;
                true
            } else {
                view.tab == 3 && toggle_knowledge_header(view, press.line)
            }
        }
        MouseEventKind::ScrollUp => {
            view.follow = false;
            view.scroll = view.scroll.saturating_sub(MOUSE_SCROLL_LINES);
            true
        }
        MouseEventKind::ScrollDown => {
            view.scroll = view
                .scroll
                .saturating_add(MOUSE_SCROLL_LINES)
                .min(view.scroll_max);
            true
        }
        _ => false,
    }
}

async fn show(
    conversation: Conversation,
    runner: Option<Runner>,
    startup: StartupOptions,
    provider: ProviderSettings,
    startup_notice: Option<String>,
) -> Result<()> {
    let mut screen = Screen::enter()?;
    let (input, input_rx) = mpsc::unbounded_channel();
    let (output, mut output_rx) = mpsc::unbounded_channel();
    let mut snapshot = Snapshot {
        conversation: conversation.clone(),
        task: runner.as_ref().map(|r| r.task.clone()),
        status: "Connecting…".into(),
        busy: true,
        endpoint: provider.config.base_url.clone(),
        model: provider.config.model.clone(),
        live: Default::default(),
        standing_read_paths: Vec::new(),
    };
    let mut controller = tokio::spawn(
        Controller::new(conversation, runner, startup, provider, input_rx, output)
            .with_startup_notice(startup_notice)
            .run(),
    );
    let mut view = View {
        follow: true,
        ..View::default()
    };
    let mut interval = tokio::time::interval(Duration::from_millis(40));
    let result = async {
        let mut dirty = true;
        loop {
            tokio::select! {
                update = output_rx.recv() => {
                    dirty = true;
                    match update {
                        Some(Update::State(state)) => snapshot = *state,
                        Some(Update::Progress(super::progress::Progress::Status(text))) => {
                            snapshot.status = text;
                        }
                        Some(Update::Progress(_)) => {},
                        Some(Update::RestoreInput(text)) => view.retained_inputs.push_back(text),
                        Some(Update::Closed) | None => break,
                    }
                },
                result = &mut controller => {
                    result?;
                    break;
                },
                _ = interval.tick() => {
                    view.tick += 1;
                    while event::poll(Duration::ZERO)? {
                        let (command, event_dirty) = match event::read()? {
                            Event::Key(event) => (key(&mut view, event), true),
                            Event::Paste(text) => {
                                view.composer.insert(&text);
                                (None, true)
                            }
                            Event::Mouse(event) => (None, mouse(&mut view, event)),
                            Event::Resize(_, _) => {
                                // Rewrapping moves every cell a selection names.
                                clear_selection(&mut view);
                                (None, true)
                            }
                            _ => (None, false),
                        };
                        dirty |= event_dirty;
                        if let Some(command) = command {
                            let quit = matches!(command, Command::Quit);
                            input.send(command).map_err(|_| anyhow::anyhow!("session controller stopped"))?;
                            if quit {
                                view.notice = "Saving session…".into();
                                // An unavailable startup server must not trap the terminal.
                                finish_controller(&mut controller).await;
                                return Ok::<(), anyhow::Error>(());
                            }
                        }
                    }
                    if view.composer.text.is_empty() {
                        if let Some(text) = view.retained_inputs.pop_front() {
                            view.composer.insert(&text);
                            view.notice = "Command restored. Submit it after reviewing the displayed gate.".into();
                            dirty = true;
                        }
                    }
                    if dirty || (snapshot.busy && view.tick.is_multiple_of(3)) {
                        screen.terminal.draw(|frame| render(frame, &snapshot, &mut view))?;
                        dirty = false;
                    }
                    if let Some(text) = view.copy_request.take().filter(|text| !text.is_empty()) {
                        view.notice = match clipboard::copy(&text) {
                            Ok(clipboard::Route::Tool) => {
                                format!("Copied {} characters", text.chars().count())
                            }
                            Ok(clipboard::Route::Terminal) => format!(
                                "Sent {} characters to the terminal's clipboard (OSC 52); if paste is empty, this terminal does not support it",
                                text.chars().count()
                            ),
                            Err(error) => format!("Copy failed: {error}"),
                        };
                        dirty = true;
                    }
                }
            }
        }
        Ok(())
    }.await;
    if !controller.is_finished() {
        let _ = input.send(Command::Quit);
        finish_controller(&mut controller).await;
    }
    result
}

async fn finish_controller(controller: &mut tokio::task::JoinHandle<()>) {
    if tokio::time::timeout(Duration::from_secs(3), &mut *controller)
        .await
        .is_err()
    {
        controller.abort();
    }
}

/// Which conversation an interactive start opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// A fresh conversation.
    New,
    /// The newest saved conversation with unfinished work, else a fresh one.
    Last,
    /// A specific saved conversation.
    Conversation(String),
}

pub async fn interactive(
    root: PathBuf,
    daemon: Option<String>,
    daemon_exe: Option<PathBuf>,
    launch: Launch,
) -> Result<()> {
    let root = root.canonicalize()?;
    let (provider,config_error)=match ProviderSettings::load(&root) {Ok(provider)=>(provider,None),Err(error)=>(ProviderSettings::fallback(),Some(format!("Model configuration failed: {error:#}. Use /model <endpoint> <model> to configure this session.")))};
    let mut conversation = match launch {
        Launch::New => Conversation::new(root.clone()),
        Launch::Conversation(id) => Conversation::load(&root, &id)?,
        Launch::Last => match Conversation::last_unfinished(&root)? {
            Some(summary) => {
                let mut conversation = Conversation::load(&root, &summary.id)?;
                // Say what was reopened: the transcript alone does not show
                // that this is a resumption rather than a continuation.
                conversation.push(
                    "system",
                    format!(
                        "Resumed conversation {} ({}). /new starts a fresh conversation; /resume lists the others.",
                        summary.id,
                        summary.task_status()
                    ),
                );
                conversation
            }
            None => Conversation::new(root.clone()),
        },
    };
    if conversation.messages.is_empty() {
        conversation.push("system","Welcome to MOOSEDev. Ask about this project or describe a change.\nThe harness supplies project knowledge and asks for approval before edits.\nUse /model to discover local models, /help for commands.");
    }
    show(
        conversation,
        None,
        StartupOptions {
            root,
            daemon,
            daemon_exe,
        },
        provider,
        config_error,
    )
    .await
}

/// Compatibility entry point for an existing headless task.
pub async fn run(runner: Runner) -> Result<()> {
    let root = runner.task.root.clone();
    let conversation = conversation_for_task(&runner.task)?;
    let provider = ProviderSettings::load(&root)?;
    let daemon = Some(runner.daemon_url().to_string());
    show(
        conversation,
        Some(runner),
        StartupOptions {
            root,
            daemon,
            daemon_exe: None,
        },
        provider,
        None,
    )
    .await
}

fn conversation_for_task(task: &Task) -> Result<Conversation> {
    for id in Conversation::list(&task.root)? {
        let conversation = Conversation::load(&task.root, &id)?;
        if conversation.active_task.as_ref() == Some(&task.id) {
            return Ok(conversation);
        }
    }
    let mut conversation = Conversation::new(task.root.clone());
    // A stable compatibility ID makes concurrent first opens contend on the
    // same lease; the controller saves only after acquiring that lease.
    conversation.id.clone_from(&task.id);
    conversation.active_task = Some(task.id.clone());
    conversation.tasks.push(task.id.clone());
    conversation.push(
        "system",
        format!("Opened task {}. {}", task.id, task.objective),
    );
    Ok(conversation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_content(text: &Text<'_>) -> String {
        text.lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn task_fixture(root: PathBuf) -> Task {
        serde_json::from_value(serde_json::json!({
            "id":uuid::Uuid::new_v4().to_string(), "root":root, "objective":"Explain the parser", "mode":"Plan", "phase":"Planning",
            "events":[], "last_response":"", "knowledge_revision":"v1", "read_files":[], "check_results":[], "model_requests":[],
            "schema":2,"snapshots":{},"capture_due":false,"final_capture":false,"after_review":"Planning","resume_phase":"Planning","steps":0,"capture_operations":[],"capture_cursor":0,"source":{}
        })).unwrap()
    }
    #[test]
    fn journal_view_summarizes_without_serializing_large_prompts_or_events() {
        let mut task = task_fixture(PathBuf::from("/project"));
        let huge = "FULL_PROMPT_PAYLOAD".repeat(100_000);
        task.model_requests
            .push(serde_json::json!({"purpose":"harness_action","prompt":huge,"response":huge}));
        task.events.push(super::super::runner::Event {
            message: "x".repeat(100_000),
        });
        let snapshot = Snapshot {
            conversation: Conversation::new(PathBuf::from("/project")),
            task: Some(task),
            status: String::new(),
            busy: false,
            endpoint: String::new(),
            model: String::new(),
            live: Default::default(),
            standing_read_paths: Vec::new(),
        };
        let summary = journal_summary(&snapshot);
        assert!(summary.len() < 2_000);
        assert!(!summary.contains("FULL_PROMPT_PAYLOAD"));
        assert!(summary.contains("harness_action · response saved"));
        assert!(summary.contains(".moosedev/harness/tasks/"));
    }
    fn snapshot_for(task: Task) -> Snapshot {
        Snapshot {
            conversation: Conversation::new(PathBuf::from("/project")),
            task: Some(task),
            status: String::new(),
            busy: false,
            endpoint: String::new(),
            model: String::new(),
            live: Default::default(),
            standing_read_paths: Vec::new(),
        }
    }
    fn review_view() -> View {
        View {
            tab: 4,
            ..Default::default()
        }
    }
    const PRESERVE: &str = "https://moosedev.dev/kg/Constraint/preserve-names";
    const LABELS: &str = "https://moosedev.dev/kg/Requirement/label-intent";
    fn symbolic_task() -> Task {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.symbolic = Some(
            serde_json::from_value(serde_json::json!({
                "obligations": {"labels.py": [PRESERVE, LABELS], "util.py": []},
                "obligations_digest": "0123456789abcdef0123",
                "knowledge_revision": "accepted-v3",
                "scope_escapes": 1, "noop_continuations": 2, "retypes": 0,
                "check_history": [{"command": "pytest -q", "success": false, "after_edit": true},
                                   {"command": "pytest -q", "success": true, "after_edit": true}]
            }))
            .unwrap(),
        );
        task.approved_change_scope = Some(serde_json::from_value(serde_json::json!({
            "version": 2, "knowledge_revision": "accepted-v3", "files": {"labels.py": null},
            "obligation_iris": [PRESERVE, LABELS],
            "definition_scopes": [{"file": "labels.py", "symbol": "labels/render_name().", "source_digest": "d"}],
            "checks": ["pytest -q"], "approval_cycle": "cycle-1"
        }))
        .unwrap());
        task
    }
    fn review_item(operation_id: &str, links: Option<serde_json::Value>) -> ReviewItem {
        serde_json::from_value(serde_json::json!({
            "intent_links": links,
            "request": {"operation_id": operation_id, "proposals": [{
                "kind": "Lesson", "title": "Strip before comparing", "description": "Whitespace differs.",
                "evidence": ["Event 9: capture note"]}]},
            "response": {"proposals": []},
            "reason": "Final checkpoint"
        }))
        .unwrap()
    }
    #[test]
    fn approval_gate_explains_symbolic_derivation() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingPlan;
        task.plan = Some(super::super::runner::Plan {
            summary: "Trim label whitespace".into(),
            files: vec!["labels.py".into()],
            checks: vec!["pytest -q".into()],
        });
        let text = gate(&task, &[]);
        assert!(text.contains("PLAN · human approval required"));
        assert!(text.contains(SYMBOLIC_APPROVAL));
        assert!(text.contains("no model call"));
        assert!(text.ends_with("/approve to execute · send feedback to revise"));
        assert!(!text.contains("Active sandbox grants"));

        task.permission_grants = vec![serde_json::from_value(serde_json::json!({
            "id": "grant-1",
            "justification": "Fetch dependencies",
            "read_paths": [],
            "write_paths": [],
            "network": true,
            "approved_at": "2026-09-21T00:00:00Z"
        }))
        .unwrap()];
        let text = gate(&task, &[]);
        assert!(text.contains("Active sandbox grants: 1 task · 0 standing"));
        assert!(text.ends_with("/approve to execute · send feedback to revise"));
        // Standing paths are in force with no approval, so re-approving a plan
        // must show them even when the task itself has been granted nothing.
        let standing = ["/opt/toolchain".to_string()];
        task.permission_grants.clear();
        let text = gate(&task, &standing);
        assert!(
            text.contains("Active sandbox grants: 0 task · 1 standing"),
            "{text}"
        );
    }
    #[test]
    fn input_gate_shows_the_request_it_waits_on() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingInput;
        task.last_response = "action failed validation after three attempts. Provide human guidance before retrying; pending work is preserved. replace old_text must match exactly once; found 0".into();
        let text = gate(&task, &[]);
        assert!(text.starts_with("INPUT NEEDED\naction failed validation after three attempts."));
        assert!(text.ends_with(
            "Reply with guidance (the task returns to Plan for re-approval), or /plan."
        ));

        task.last_response = "Which database should the skeleton use?".into();
        assert!(gate(&task, &[]).contains("Which database should the skeleton use?"));

        task.last_response.clear();
        assert_eq!(gate(&task, &[]), "Your input is needed. Reply below.");

        task.turn_finished = true;
        assert!(gate(&task, &[]).starts_with("Your turn."));
    }
    #[test]
    fn permission_gate_displays_exact_capabilities_and_decision_commands() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingPermission;
        task.pending_permission = Some(
            serde_json::from_value(serde_json::json!({
                "request_id": "permission-1",
                "command": "cargo install example",
                "justification": "Install the required local tool",
                "read_paths": ["/opt/toolchain"],
                "write_paths": ["/tmp/tool-cache"],
                "network": true,
                "revision": "accepted-v3"
            }))
            .unwrap(),
        );
        let text = gate(&task, &[]);
        assert!(text.contains("PERMISSION REQUEST · human approval required"));
        assert!(text.contains("Request: permission-1"));
        assert!(text.contains("Command:\n  cargo install example"));
        assert!(text.contains("Read access:\n  /opt/toolchain"));
        assert!(text.contains("Write access (create, modify, and delete):\n  /tmp/tool-cache"));
        assert!(text.contains("Network: enabled"));
        assert!(text.contains("/approve grants this access for the current task"));
        assert!(text.contains("/deny refuses it"));
    }
    #[test]
    fn spec_gate_warns_when_no_component_will_anchor_the_records() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingSpecApproval;
        task.pending_spec = Some(serde_json::from_value(serde_json::json!({
            "preview": {
                "operation_id": "spec-2",
                "owner_id": "task-1",
                "path": "badciv-map.md",
                "source_sha256": "0123456789abcdef",
                "knowledge_revision": "accepted-v3",
                "entries": [{
                    "draft": {"kind": "Constraint", "title": "No SQLite", "description": "The map crate never opens SQLite.", "evidence": ["badciv-map.md:5"]},
                    "disposition": {"kind": "new", "iri": "https://moosedev.dev/kg/Constraint/no-sqlite"}
                }],
                "retirements": [],
                "component": null,
                "previous_approval_iri": null,
                "already_approved": false
            },
            "uncited": [
                {"start": 20, "end": 50, "heading": "## Faction Rules"},
                {"start": 61, "end": 61, "heading": ""}
            ]
        })).unwrap());
        let text = gate(&task, &[]);
        assert!(
            text.contains("COMPONENT · none · the records below will not be linked"),
            "{text}"
        );
        assert!(
            text.contains("UNCITED · 2 range(s) of badciv-map.md no record above cites; they will not become project knowledge:\n  lines 20-50 (## Faction Rules)\n  line 61\n"),
            "{text}"
        );
        assert!(
            text.contains("run /approve-spec badciv-map.md <dir/ | file | .> instead"),
            "{text}"
        );
        assert!(text.contains("NEW · Constraint · No SQLite"), "{text}");
    }
    #[test]
    fn spec_gate_renders_exact_records_and_separate_execution_approval() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingSpecApproval;
        task.pending_spec = Some(serde_json::from_value(serde_json::json!({
            "preview": {
                "operation_id": "spec-1",
                "owner_id": "task-1",
                "path": "specs/labels.md",
                "source_sha256": "0123456789abcdef",
                "knowledge_revision": "accepted-v3",
                "entries": [
                    {
                        "draft": {"kind": "Requirement", "title": "Preserve labels", "description": "Labels retain meaningful whitespace.", "evidence": ["specs/labels.md:7-8"]},
                        "disposition": {"kind": "new", "iri": "https://moosedev.dev/kg/Requirement/preserve-labels"}
                    },
                    {
                        "draft": {"kind": "Constraint", "title": "Local only", "description": "Processing remains local.", "evidence": ["specs/labels.md:11"]},
                        "disposition": {"kind": "reuse", "iri": "https://moosedev.dev/kg/Constraint/local-only"},
                        "existing": {"iri": "https://moosedev.dev/kg/Constraint/local-only", "kind": "Constraint", "title": "Existing local processing", "description": "Processing remains local.\n\nEvidence:\n- specs/original.md:4"}
                    },
                    {
                        "draft": {"kind": "Constraint", "title": "Bounded labels", "description": "Labels are bounded.", "evidence": ["specs/labels.md:12"]},
                        "disposition": {"kind": "supersede", "iri": "https://moosedev.dev/kg/Constraint/bounded-v2", "previous_iri": "https://moosedev.dev/kg/Constraint/bounded-v1"},
                        "existing": {"iri": "https://moosedev.dev/kg/Constraint/bounded-v1", "kind": "Constraint", "title": "Bounded labels", "description": "Labels had the old bound.\n\nEvidence:\n- specs/labels.md:3"}
                    }
                ],
                "retirements": [
                    {"iri": "https://moosedev.dev/kg/Requirement/old", "kind": "Requirement", "title": "Old behavior", "description": "The old claim.\n\nEvidence:\n- specs/labels.md:2", "disposition": "retract"},
                    {"iri": "https://moosedev.dev/kg/Constraint/shared", "kind": "Constraint", "title": "Shared behavior", "description": "The shared claim.\n\nEvidence:\n- specs/shared.md:8", "disposition": "retain_shared"}
                ],
                "component": {"iri": "https://moosedev.dev/kg/SystemComponent/labels", "name": "labels", "new": true, "covers": ["labels/"], "added": ["labels/"]},
                "previous_approval_iri": "https://moosedev.dev/kg/ArchitecturalDecision/approval-v1",
                "already_approved": false
            }
        })).unwrap());
        let text = gate(&task, &[]);
        assert!(text.contains("SPEC APPROVAL · human approval required"));
        assert!(text.contains(
            "COMPONENT · labels · NEW\nCovers: labels/\nIRI: https://moosedev.dev/kg/SystemComponent/labels\n"
        ));
        assert!(text.contains(
            "NEW · Requirement · Preserve labels\nExtracted claim: Labels retain meaningful whitespace."
        ));
        assert!(text.contains("Evidence:\n  specs/labels.md:7-8"));
        assert!(text.contains("Existing claim and evidence:\nProcessing remains local."));
        assert!(text.contains("Existing claim and evidence:\nLabels had the old bound."));
        assert!(text.contains("Existing claim and evidence:\nThe old claim."));
        assert!(text.contains("REUSE · Constraint · Local only"));
        assert!(text.contains("SUPERSEDE · Constraint · Bounded labels"));
        assert!(text.contains("RETRACT · Requirement · Old behavior"));
        assert!(text.contains("RETAIN SHARED · Constraint · Shared behavior"));
        assert!(text.contains("SUPERSEDE PRIOR APPROVAL\nIRI: https://moosedev.dev/kg/ArchitecturalDecision/approval-v1"));
        assert!(text.contains("Approval marker: spec-approval: specs/labels.md"));
        assert!(text.contains("A separate /approve is still required before code execution."));
    }
    #[test]
    fn review_tab_renders_derived_obligations_for_the_approved_plan() {
        let text = text_content(&body(&snapshot_for(symbolic_task()), &review_view()));
        assert!(text.contains("Derived scope\nlabels.py\n  Constraint · https://moosedev.dev/kg/Constraint/preserve-names\n  Requirement · https://moosedev.dev/kg/Requirement/label-intent\nutil.py · ungoverned\n"));
        assert!(text.contains("  def labels.py · labels/render_name().\n"));
        assert!(text.contains("Revision accepted-v3 · obligations 0123456789ab\n"));
        assert!(text.contains("Scope escapes 1 · no-op continuations 2 · retypes 0\n"));
        assert!(text.contains("  FAIL · pytest -q · after edit\n  PASS · pytest -q · after edit\n"));
        let mut bare = task_fixture(PathBuf::from("/project"));
        bare.symbolic = Some(Default::default());
        let text = text_content(&body(&snapshot_for(bare), &review_view()));
        assert!(text.contains("No approved plan scope derived yet."));
    }
    #[test]
    fn knowledge_tab_renders_current_context_and_search_history() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.knowledge_context = Some(super::super::runner::KnowledgeContextSnapshot {
            topic: "Explain the parser".into(),
            revision: "accepted-v4".into(),
            context: "Requirement · Parser output stays deterministic".into(),
            files: vec![super::super::runner::KnowledgeFileDossier {
                file: "src/parser.rs".into(),
                dossier: "Parser dossier".into(),
            }],
            governing_rules: vec![super::super::protocol::GoverningRule {
                iri: "https://moosedev.dev/kg/Constraint/deterministic".into(),
                label: "Deterministic parser".into(),
                kind: "Constraint".into(),
                claim: "Parsing must not depend on iteration order.".into(),
                via: "src/parser.rs".into(),
            }],
            records: vec![],
            delivery_receipt: None,
        });
        task.knowledge_searches = vec![
            super::super::runner::KnowledgeSearchResult {
                query: "parser".into(),
                revision: "accepted-v4".into(),
                context: "Requirement · Parser output stays deterministic".into(),
                evidence_iris: vec!["https://moosedev.dev/kg/Requirement/parser".into()],
                records: vec![],
                delivery_receipt: Some(super::super::protocol::ContextDeliveryReceipt {
                    max_bytes: Some(4096),
                    context_bytes: 96,
                    records: vec![super::super::protocol::ContextRecordDelivery {
                        iri: "https://moosedev.dev/kg/Requirement/parser".into(),
                        kind: "Requirement".into(),
                        tier: super::super::protocol::ContextRecordDeliveryTier::FirstSentence,
                        reason: "complete claim did not fit".into(),
                    }],
                }),
            },
            super::super::runner::KnowledgeSearchResult {
                query: "missing".into(),
                revision: "accepted-v4".into(),
                context: String::new(),
                evidence_iris: vec![],
                records: vec![],
                delivery_receipt: None,
            },
        ];

        let view = View {
            tab: 3,
            ..Default::default()
        };
        let text = text_content(&body(&snapshot_for(task), &view));
        assert!(text.contains("CURRENT WORKING CONTEXT\nTopic: Explain the parser"));
        assert!(text.contains("PROJECT RULES\nDeterministic parser"));
        assert!(text.contains("FILE DOSSIERS\n\nsrc/parser.rs\nParser dossier"));
        assert!(text.contains("SEARCH 1 · 1 accepted record(s)\nQuery: parser"));
        assert!(text.contains("Delivery: 96 bytes of 4096"));
        assert!(text.contains("- first_sentence: Requirement"));
        assert!(text.contains("SEARCH 2 · 0 accepted record(s)\nQuery: missing"));
        assert!(text.contains("No accepted project knowledge matched."));
    }
    #[test]
    fn structured_knowledge_history_groups_records_and_defaults_to_latest_open() {
        let record = |iri: &str, kind: &str, title: &str, claim: &str, via: &str| {
            super::super::protocol::ContextRecord {
                iri: iri.into(),
                kind: kind.into(),
                title: title.into(),
                claim: claim.into(),
                provenance: vec![via.into()],
            }
        };
        let mut task = task_fixture(PathBuf::from("/project"));
        task.knowledge_turns = vec![
            super::super::runner::KnowledgeTurn {
                sequence: 0,
                query: "Explain the old parser behavior".into(),
                retrieval_topic: "Explain the parser".into(),
                revision: "accepted-old".into(),
                records: vec![record(
                    "https://moosedev.dev/kg/Constraint/old",
                    "Constraint",
                    "Old parser constraint",
                    "The old parser is stable.",
                    "topic match",
                )],
                files: vec![],
                searches: vec![],
            },
            super::super::runner::KnowledgeTurn {
                sequence: 1,
                query: "Show the new parser requirement".into(),
                retrieval_topic: "Explain the parser Show the new parser requirement".into(),
                revision: "accepted-new".into(),
                records: vec![record(
                    "https://moosedev.dev/kg/Requirement/new",
                    "Requirement",
                    "New parser requirement",
                    "Output must stay deterministic.",
                    "current inventory",
                )],
                files: vec!["src/parser.rs".into()],
                searches: vec![super::super::runner::KnowledgeSearchResult {
                    query: "parser lessons".into(),
                    revision: "accepted-new".into(),
                    context: "legacy model text".into(),
                    evidence_iris: vec!["https://moosedev.dev/kg/Lesson/parser".into()],
                    records: vec![record(
                        "https://moosedev.dev/kg/Lesson/parser",
                        "Lesson",
                        "Parser ordering lesson",
                        "Sort before rendering.",
                        "topic match",
                    )],
                    delivery_receipt: None,
                }],
            },
        ];
        let mut view = View {
            tab: 3,
            ..Default::default()
        };
        let rendered = knowledge_body(&task, &mut view, 80);
        let text = text_content(&rendered);
        assert!(text.contains("▶ Query 1 · 1 record — Explain the old parser behavior"));
        assert!(!text.contains("Old parser constraint"));
        assert!(text.contains("▼ Query 2 · 2 records — Show the new parser requirement"));
        assert!(text.contains("[Requirement] New parser requirement"));
        assert!(text.contains("Output must stay deterministic."));
        assert!(text.contains("via · current inventory"));
        assert!(text.contains("https://moosedev.dev/kg/Requirement/new"));
        assert!(text.contains("Model search 1 · 1 — parser lessons"));
        assert!(text.contains("[Lesson] Parser ordering lesson"));
        assert_eq!(view.knowledge_selected, Some(1));
        assert!(view.knowledge_collapsed.contains(&0));
        assert!(!view.knowledge_collapsed.contains(&1));
        assert!(rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| {
                span.content.contains("Show the new parser requirement")
                    && span.style.fg == Some(Color::Yellow)
                    && span.style.add_modifier.contains(Modifier::BOLD)
            }));
    }

    #[test]
    fn knowledge_headers_toggle_by_wrapped_mouse_hit_and_alt_navigation() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.knowledge_turns = [
            (0, "A deliberately long first human query that wraps"),
            (1, "A deliberately long newest human query that wraps"),
        ]
        .into_iter()
        .map(|(sequence, query)| super::super::runner::KnowledgeTurn {
            sequence,
            query: query.into(),
            retrieval_topic: query.into(),
            revision: "accepted-v1".into(),
            records: vec![],
            files: vec![],
            searches: vec![],
        })
        .collect();
        let mut view = View {
            tab: 3,
            ..Default::default()
        };
        view.line_count = knowledge_body(&task, &mut view, 18).height();
        let first = view.knowledge_headers[0];
        assert!(first.end - first.start > 1, "header should wrap");
        assert!(view.soft_breaks.contains(&(first.start + 1)));
        view.content_area = Rect::new(1, 2, 18, 12);
        let click_row = view.content_area.y + first.start as u16 + 1;
        let at = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // A click toggles on release, so a drag that starts on a header can
        // select its text instead.
        let left = MouseButton::Left;
        assert!(!mouse(
            &mut view,
            at(MouseEventKind::Down(left), 2, click_row)
        ));
        assert!(view.knowledge_collapsed.contains(&0));
        assert!(mouse(&mut view, at(MouseEventKind::Up(left), 2, click_row)));
        assert!(!mouse(
            &mut view,
            at(MouseEventKind::Down(left), 2, click_row)
        ));
        assert!(mouse(
            &mut view,
            at(MouseEventKind::Drag(left), 6, click_row)
        ));
        assert!(mouse(&mut view, at(MouseEventKind::Up(left), 6, click_row)));
        assert!(view.copy_pending, "a drag is a selection, not a click");
        assert!(!view.knowledge_collapsed.contains(&0));
        assert!(!view.knowledge_collapsed.contains(&1));

        key(&mut view, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        assert_eq!(view.knowledge_selected, Some(0));
        key(&mut view, KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert!(view.knowledge_collapsed.contains(&0));
        key(&mut view, KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert!(!view.knowledge_collapsed.contains(&0));
    }
    #[test]
    fn link_review_renders_derived_bindings_with_predicate_and_basis() {
        let mut task = symbolic_task();
        let links = serde_json::json!({"operation_id": "link-1", "revision": "accepted-v3", "bindings": [
            {"record_iri": PRESERVE, "file": "labels.py", "symbol": "labels/render_name().", "source_digest": "d"}]});
        task.reviews.push(review_item("cap-0", Some(links.clone())));
        task.symbolic.as_mut().unwrap().association = Some(serde_json::from_value(serde_json::json!({
            "status": "awaiting_review", "link_operation_id": "link-1",
            "page": {
                "knowledge_revision": "accepted-v3",
                "index": {"revision": "idx", "producer": "scip-python", "status": "current", "refresh_action": "not_requested"},
                "scope_digest": "s",
                "bindings": [{
                    "file": "labels.py", "symbol": "labels/render_name().", "name": "render_name", "kind": "function",
                    "definition_range": {"start": {"line": 4, "col": 0}, "end": {"line": 6, "col": 0}},
                    "scope_basis": "changed_definition", "source_digest": "d",
                    "record_iri": PRESERVE, "record_kind": "Constraint", "assertion_digest": "a",
                    "predicate": "constrains", "basis": "obligation", "candidate_digest": "c"}],
                "skipped": [
                    {"file": "labels.py", "symbol": "labels/render_name().(name)", "reason": "parameter"},
                    {"file": "labels.py", "symbol": "labels/_tmp.", "reason": "local"},
                    {"file": "labels.py", "symbol": "labels/_tmp2.", "reason": "local"}],
                "ungoverned": ["util.py"],
                "unresolved": [{"file": "new.py", "reason": "not indexed"}]
            }
        }))
        .unwrap());
        let text = text_content(&body(&snapshot_for(task.clone()), &review_view()));
        assert!(text.contains("Code associations · link-1\n"));
        assert!(text.contains("Constraint · https://moosedev.dev/kg/Constraint/preserve-names\nlabels.py · render_name (function) · line 5\n-constrains-> plan obligation\n"));
        assert!(text.contains("Skipped: Local 2 · Parameter 1\n"));
        assert!(text.contains("Ungoverned: util.py\n"));
        assert!(text.contains("Unresolved: new.py · not indexed\n"));
        assert!(!text.contains("Derivation detail unavailable"));
        // A journal whose batch moved on keeps the plain binding list.
        task.symbolic.as_mut().unwrap().association = None;
        let text = text_content(&body(&snapshot_for(task), &review_view()));
        assert!(text.contains("labels.py · labels/render_name().\n"));
        assert!(text.contains("Derivation detail unavailable\n"));
    }
    #[test]
    fn final_review_renders_the_note_and_typed_dispositions() {
        let mut task = symbolic_task();
        task.phase = Phase::AwaitingReview;
        task.reviews.push(review_item("cap-1", None));
        task.symbolic.as_mut().unwrap().capture_note = Some(serde_json::from_value(serde_json::json!({
            "operation_id": "type-1", "capture_operation_id": "cap-1", "note_event": 9,
            "note": "Names are stripped before comparison.", "status": "typed",
            "response": {
                "revision": "accepted-v3", "typing_mode": "sensor", "typing_note": "sensor added one proposal",
                "thresholds": {"restates": 0.8, "refines": 0.55, "refines_containment": 0.6, "tiebreak_band": 0.08},
                "proposals": [
                    {"proposal": {"kind": "ArchitecturalDecision", "title": "Trim label whitespace", "description": "d", "evidence": []},
                     "origin": "symbolic_decision", "resolved_by": "symbolic",
                     "disposition": {"kind": "restates", "candidate_iri": LABELS, "score": 0.9, "confidence": 0.9, "receipt_operation_id": "r1"}},
                    {"proposal": {"kind": "Lesson", "title": "Strip before comparing", "description": "d", "evidence": []},
                     "origin": "symbolic_lesson", "resolved_by": "symbolic",
                     "disposition": {"kind": "refines", "candidate_iri": PRESERVE, "score": 0.6, "containment": 0.7, "confidence": 0.6333, "receipt_operation_id": "r2"}},
                    {"proposal": {"kind": "Pattern", "title": "Normalize then compare", "description": "d", "evidence": []},
                     "origin": "llm_sensor", "resolved_by": "symbolic",
                     "disposition": {"kind": "distinct", "receipt_operation_id": "r3"}}
                ]
            }
        }))
        .unwrap());
        let text = text_content(&body(&snapshot_for(task), &review_view()));
        assert!(text.contains("REVIEW 1\nFinal checkpoint\n\nCapture note\nNames are stripped before comparison.\nTyping: symbolic with sensor · sensor added one proposal\n"));
        assert!(text.contains("SymbolicDecision · ArchitecturalDecision · Trim label whitespace — restates https://moosedev.dev/kg/Requirement/label-intent; no record proposed\n"));
        assert!(text.contains("SymbolicLesson · Lesson · Strip before comparing — refines https://moosedev.dev/kg/Constraint/preserve-names (0.63)\n"));
        assert!(text.contains("LlmSensor · Pattern · Normalize then compare — new record\n"));
        assert!(text.contains("/accept 1 · /reject 1"));
    }
    #[test]
    fn opening_a_task_reuses_its_conversation_without_saving_before_a_lease() {
        let root = std::env::temp_dir().join(format!("md-tui-task-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task = task_fixture(root.clone());
        let conversation = conversation_for_task(&task).unwrap();
        assert_eq!(conversation.id, task.id);
        assert!(!root.join(".moosedev").exists());
        assert_eq!(conversation_for_task(&task).unwrap().id, task.id);
        let mut existing = conversation;
        existing.id = uuid::Uuid::new_v4().to_string();
        existing.save().unwrap();
        assert_eq!(conversation_for_task(&task).unwrap().id, existing.id);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unicode_editing_and_paste_preserve_character_boundaries() {
        let mut composer = Composer::default();
        composer.insert("aλ🦌\n日本語");
        composer.home();
        composer.backspace();
        assert_eq!(composer.text, "aλ🦌日本語");
        composer.left();
        composer.delete();
        assert_eq!(composer.text, "aλ日本語");
        composer.left();
        composer.insert("é");
        assert_eq!(composer.text, "aéλ日本語");
        composer.insert("\u{1b}[31m");
        assert!(!composer.text.contains('\u{1b}'));
    }
    #[test]
    fn control_j_adds_a_newline_without_submitting() {
        let mut view = View::default();
        view.composer.insert("hello");
        assert!(key(
            &mut view,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)
        )
        .is_none());
        view.composer.insert("world");
        assert_eq!(view.composer.text, "hello\nworld");
        assert!(
            matches!(key(&mut view,KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE)),Some(Command::Input(text)) if text=="hello\nworld")
        );
    }
    #[test]
    fn enter_submits_modified_enter_adds_newline_and_escape_interrupts() {
        let mut view = View::default();
        view.composer.insert("hello");
        assert!(key(
            &mut view,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
        )
        .is_none());
        assert_eq!(view.composer.text, "hello\n");
        assert!(key(&mut view, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)).is_none());
        assert_eq!(view.composer.text, "hello\n\n");
        assert!(
            matches!(key(&mut view,KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE)),Some(Command::Input(text)) if text=="hello\n\n")
        );
        assert!(matches!(
            key(&mut view, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Command::Interrupt)
        ));
    }
    #[test]
    fn keyboard_enhancement_reports_modified_enter() {
        let flags = keyboard_enhancement_flags();
        assert!(flags.contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
        assert!(flags.contains(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS));
        assert!(flags.contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES));
    }
    #[test]
    fn tab_navigation_includes_knowledge_and_review() {
        let mut view = View::default();
        for expected in 1..=5 {
            key(&mut view, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            assert_eq!(view.tab, expected);
        }
        key(&mut view, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(view.tab, 0);
        key(
            &mut view,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        assert_eq!(view.tab, 5);
    }
    #[test]
    fn oversized_input_stays_in_composer_for_correction() {
        let mut view = View::default();
        view.composer.insert(&"x".repeat(16_001));
        assert!(key(&mut view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).is_none());
        assert_eq!(view.composer.text.len(), 16_001);
        assert!(view.notice.contains("shorten"));
    }
    #[test]
    fn mouse_wheel_scrolls_content_and_pauses_following() {
        let mut view = View {
            scroll: 5,
            scroll_max: 10,
            follow: true,
            ..View::default()
        };
        let event = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        assert!(mouse(&mut view, event(MouseEventKind::ScrollUp)));
        assert_eq!(view.scroll, 4);
        assert!(!view.follow);

        assert!(mouse(&mut view, event(MouseEventKind::ScrollDown)));
        assert_eq!(view.scroll, 5);
        assert!(!view.follow);
    }
    #[test]
    fn dragging_selects_rendered_text_scrolls_past_edges_and_requests_a_copy() {
        let mut conversation = Conversation::new(PathBuf::from("/project"));
        conversation.push(
            "user",
            (1..=30)
                .map(|n| format!("line {n:02} of the transcript"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let snapshot = Snapshot {
            conversation,
            task: None,
            status: String::new(),
            busy: false,
            endpoint: String::new(),
            model: String::new(),
            live: Default::default(),
            standing_read_paths: Vec::new(),
        };
        let mut view = View::default();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 20)).unwrap();
        let mut draw = |view: &mut View| {
            terminal
                .draw(|frame| render(frame, &snapshot, view))
                .unwrap();
            terminal.backend().buffer().clone()
        };
        draw(&mut view);
        let area = view.content_area;
        let at = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let left = MouseButton::Left;
        let row_of = |view: &View, needle: &str| {
            let rows: Vec<String> = body(&snapshot, view)
                .lines
                .iter()
                .map(ToString::to_string)
                .collect();
            rows.iter().position(|row| row.contains(needle)).unwrap()
        };
        let first = row_of(&view, "line 01") as u16;
        let column = |text: &str| area.x + text.len() as u16;

        // A press alone selects nothing; pointer motion without it is ignored.
        assert!(!mouse(&mut view, at(MouseEventKind::Moved, 5, area.y)));
        assert!(!mouse(
            &mut view,
            at(MouseEventKind::Down(left), column("line "), area.y + first)
        ));
        assert!(mouse(
            &mut view,
            at(
                MouseEventKind::Drag(left),
                column("line 02"),
                area.y + first + 1
            )
        ));
        // The same cell again changes nothing, so it must not redraw.
        assert!(!mouse(
            &mut view,
            at(
                MouseEventKind::Drag(left),
                column("line 02"),
                area.y + first + 1
            )
        ));
        let buffer = draw(&mut view);
        let reversed =
            |column: u16, row: u16| buffer[(column, row)].modifier.contains(Modifier::REVERSED);
        assert!(reversed(column("line "), area.y + first));
        assert!(!reversed(column("line"), area.y + first));
        assert!(reversed(area.x, area.y + first + 1));
        assert!(!reversed(column("line 02 "), area.y + first + 1));

        // Dragging below the pane scrolls toward the rest of the content.
        assert!(mouse(
            &mut view,
            at(
                MouseEventKind::Drag(left),
                column("line"),
                area.bottom() + 3
            )
        ));
        assert_eq!(view.scroll, 1);
        assert!(mouse(&mut view, at(MouseEventKind::Up(left), 0, 0)));
        assert!(view.copy_request.is_none());
        draw(&mut view);
        let copied = view.copy_request.take().unwrap();
        assert!(
            copied.starts_with("01 of the transcript\nline 02 of the transcript\n"),
            "{copied:?}"
        );
        // Both end cells are inclusive: the cell after "line" is its space.
        assert!(copied.ends_with("\nline "), "{copied:?}");
        assert!(
            view.selection.is_some(),
            "highlight stays until the next click"
        );

        assert!(mouse(&mut view, at(MouseEventKind::Down(left), 0, 0)));
        assert!(view.selection.is_none());
        mouse(&mut view, at(MouseEventKind::Down(left), area.x, area.y));
        mouse(
            &mut view,
            at(MouseEventKind::Drag(left), area.x + 3, area.y),
        );
        key(&mut view, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(view.selection.is_none() && view.press.is_none());
    }

    #[test]
    fn mouse_wheel_saturates_and_ignores_unrelated_events() {
        let mut view = View {
            scroll: 1,
            follow: true,
            ..View::default()
        };
        let event = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        assert!(mouse(&mut view, event(MouseEventKind::ScrollUp)));
        assert_eq!(view.scroll, 0);
        let state = (view.scroll, view.follow);
        assert!(!mouse(&mut view, event(MouseEventKind::Moved)));
        assert_eq!((view.scroll, view.follow), state);
    }
    #[test]
    fn scrolling_is_clamped_to_the_rendered_viewport() {
        let mut view = View {
            scroll: u16::MAX,
            ..View::default()
        };
        sync_scroll_bounds(&mut view, 20, 8);
        assert_eq!((view.scroll, view.scroll_max), (12, 12));

        let event = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(mouse(&mut view, event));
        assert_eq!(view.scroll, 12);

        assert!(key(
            &mut view,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)
        )
        .is_none());
        assert_eq!(view.scroll, 12);

        view.follow = true;
        sync_scroll_bounds(&mut view, 6, 8);
        assert_eq!((view.scroll, view.scroll_max), (0, 0));
    }
    #[test]
    fn multiline_cursor_matches_visual_wrapping_and_unicode_lines() {
        let mut composer = Composer::default();
        composer.insert("abc\n");
        assert_eq!(composer.position(3), (1, 0));
        composer.insert("日本語\nz");
        composer.vertical(false);
        assert_eq!(&composer.text[..composer.cursor], "abc\n日");
        composer.vertical(true);
        assert_eq!(composer.cursor, composer.text.len());
        assert_eq!(wrapped("日本語", 4), "日本\n語");
    }
    #[test]
    fn completed_diffs_show_added_and_removed_lines() {
        let edit = super::super::runner::PendingEdit {
            file: "parser.rs".into(),
            before: Some("old\nshared\n".into()),
            after: Some("new\nshared\n".into()),
            reason: String::new(),
            revision: "accepted-v1".into(),
        };
        let text = edit_diff(&edit, "APPLIED");
        assert!(text.contains("APPLIED · parser.rs"));
        assert!(text.contains("- old"));
        assert!(text.contains("+ new"));
        assert!(text.contains("  shared"));
    }

    #[test]
    fn conversation_roles_and_assistant_markdown_are_visually_distinct() {
        let mut conversation = Conversation::new(PathBuf::from("/project"));
        conversation.push("user", "Keep **this** literal");
        conversation.push(
            "assistant",
            "## Result\n\nUse **bold** and `code` with [docs](https://example.com).",
        );
        let snapshot = Snapshot {
            conversation,
            task: None,
            status: String::new(),
            busy: true,
            endpoint: String::new(),
            model: String::new(),
            live: std::sync::Arc::new(std::sync::Mutex::new(super::super::session::LiveOutput {
                assistant: "*Still streaming*".into(),
                command: String::new(),
            })),
            standing_read_paths: Vec::new(),
        };

        let text = body(&snapshot, &View::default());
        let plain = text_content(&text);
        assert!(plain.contains("YOU\nKeep **this** literal"));
        assert!(plain.contains("🫎 MOOSEDev\n## Result"));
        assert!(plain.contains("Use bold and code with docs (https://example.com)."));
        assert!(plain.contains("🫎 MOOSEDev\nStill streaming▌"));

        let user = &text.lines[0].spans[0];
        assert_eq!(user.style.fg, Some(Color::Green));
        assert!(user.style.add_modifier.contains(Modifier::BOLD));
        let assistant = text
            .lines
            .iter()
            .find(|line| line.to_string() == "🫎 MOOSEDev")
            .unwrap();
        assert_eq!(assistant.spans[0].style.fg, Some(Color::Cyan));
        assert!(assistant.spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn conversation_composer_and_status_render_at_small_sizes() {
        let mut conversation = Conversation::new(PathBuf::from("/project"));
        conversation.push("user", "Explain the parser");
        let snapshot = Snapshot {
            conversation,
            task: None,
            status: "Reading project knowledge".into(),
            busy: true,
            endpoint: "http://localhost:1234/v1".into(),
            model: "local-model".into(),
            live: std::sync::Arc::new(std::sync::Mutex::new(super::super::session::LiveOutput {
                assistant: "The parser".into(),
                command: String::new(),
            })),
            standing_read_paths: Vec::new(),
        };
        for (width, height) in [(100, 30), (40, 12), (10, 5)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, &snapshot, &mut View::default()))
                .unwrap();
            if width == 100 {
                let output: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(output.contains("Explain the parser"));
                assert!(output.contains("Message"));
                assert!(output.contains("Reading project knowledge"));
            }
        }
    }
}

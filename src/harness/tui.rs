//! Thin interactive frontend. Human review commands never enter the model action surface.
use super::protocol::{
    AssociatePage, DerivedBasis, SpecComponentPlan, SpecDisposition, SpecPrepareResponse,
    SpecRetirementDisposition, TypedDisposition, TypingMode,
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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) const MAX_RUN_STEPS: usize = 32;
const MOUSE_SCROLL_LINES: u16 = 1;

#[derive(Debug)]
pub enum Action {
    Step,
    Run,
    Approve,
    ApprovePolicy,
    ApprovePermission,
    DenyPermission,
    /// Answer the pending harness question with an option key.
    Choose(String),
    Permissions,
    RevokePermission(String),
    Accept,
    Reject,
    NoKnowledge,
    Plan,
    Cancel,
    Resume,
    Answer(String),
    Rework(String),
    /// With a path (and the paths it covers): extract and preview a spec for
    /// approval. Alone: approve the pending preview.
    ApproveSpec(Option<(String, Vec<String>)>),
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
        // At the plan gate the argument is "<n> <option>", an open choice.
        Action::Choose(argument) if runner.task.phase == Phase::AwaitingPlan => {
            runner.choose_plan_option(&argument)
        }
        Action::Choose(key) => runner.choose(&key).await,
        Action::Permissions => Ok(()),
        Action::RevokePermission(id) => runner.revoke_permission(&id),
        Action::Accept => runner.review(true).await,
        Action::Reject => runner.review(false).await,
        Action::NoKnowledge => runner.confirm_no_knowledge().await,
        Action::Plan => runner.mode_plan().await,
        Action::Cancel => runner.cancel().await,
        Action::Resume => runner.resume().await,
        // The same judgment as a message in the interactive session: an
        // answer to a handback or to a park where the approved plan stands
        // continues it, anything else is guidance that returns to Plan.
        Action::Answer(text) => {
            anyhow::ensure!(
                runner.task.phase == Phase::AwaitingInput,
                "no question awaiting an answer"
            );
            runner.submit_message(text).await
        }
        Action::Rework(note) => runner.rework(note).await,
        Action::ApproveSpec(Some((path, covers))) => {
            runner.begin_spec_approval(&path, &covers).await
        }
        Action::ApproveSpec(None) => {
            anyhow::ensure!(
                runner.task.phase == Phase::AwaitingSpecApproval,
                "there is no spec approval pending; use approve-spec ID PATH [COVERED...] first"
            );
            runner.approve_spec().await
        }
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

/// What the language servers said after the last applied edit, for the
/// header: clean, the error count, or unknown when they did not settle.
fn checker_span(task: &Task) -> Option<Span<'static>> {
    let diagnostics = task.diagnostics.as_ref()?;
    let servers = diagnostics.servers.join(", ");
    let counts: Vec<String> = [
        (diagnostics.errors.len(), "error"),
        (diagnostics.warnings.len(), "warning"),
        (diagnostics.lints.len(), "lint"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, kind)| format!("{count} {kind}(s)"))
    .collect();
    let (text, color) = if !diagnostics.settled {
        (format!(" · {servers} ?"), Color::Yellow)
    } else if counts.is_empty() {
        (format!(" · {servers} ✓"), Color::Green)
    } else {
        let color = if diagnostics.errors.is_empty() {
            Color::Yellow
        } else {
            Color::Red
        };
        (format!(" · {servers}: {}", counts.join(", ")), color)
    };
    Some(Span::styled(visible(&text), Style::default().fg(color)))
}

fn visible(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// A human gate as typed parts, so the pane can tell what the harness is
/// asking from the plan or request it shows and from the commands that answer
/// it; `to_plain` is the same content as text.
#[derive(Debug, Default)]
struct Gate {
    accent: Color,
    parts: Vec<GatePart>,
    /// What the human can do; always rendered last, after a rule.
    actions: Vec<GateAction>,
}

#[derive(Debug, Clone, PartialEq)]
enum GatePart {
    /// The phase and what it needs, in the gate's accent.
    Header(String),
    /// A titled group inside a long gate, after a blank line.
    Section(String),
    /// The plan summary, request, or question, with its own line structure.
    Body(String),
    /// A labelled value; a block value sits indented under its label.
    Field {
        label: String,
        value: String,
        block: bool,
    },
    /// Side information.
    Note(String),
}

#[derive(Debug, Clone, PartialEq)]
struct GateAction {
    command: String,
    description: String,
}

/// Widest command the action column pads to; longer ones push their
/// description along instead of widening every row.
const ACTION_COLUMN: usize = 22;

impl Gate {
    fn new(accent: Color, header: impl Into<String>) -> Self {
        Self {
            accent,
            parts: vec![GatePart::Header(header.into())],
            actions: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.parts.is_empty() && self.actions.is_empty()
    }

    fn section(&mut self, text: impl Into<String>) -> &mut Self {
        self.parts.push(GatePart::Section(text.into()));
        self
    }

    fn body(&mut self, text: impl Into<String>) -> &mut Self {
        let text = text.into();
        if !text.is_empty() {
            self.parts.push(GatePart::Body(text));
        }
        self
    }

    /// A field on one line, or under its label when the value spans lines.
    fn field(&mut self, label: impl Into<String>, value: impl Into<String>) -> &mut Self {
        let value = value.into();
        let block = value.contains('\n');
        self.parts.push(GatePart::Field {
            label: label.into(),
            value,
            block,
        });
        self
    }

    /// A field whose value always sits indented under its label.
    fn block(&mut self, label: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.parts.push(GatePart::Field {
            label: label.into(),
            value: value.into(),
            block: true,
        });
        self
    }

    fn note(&mut self, text: impl Into<String>) -> &mut Self {
        self.parts.push(GatePart::Note(text.into()));
        self
    }

    fn action(&mut self, command: impl Into<String>, description: impl Into<String>) -> &mut Self {
        self.actions.push(GateAction {
            command: command.into(),
            description: description.into(),
        });
        self
    }

    #[cfg(test)]
    fn to_plain(&self) -> String {
        let mut lines = Vec::new();
        for part in &self.parts {
            match part {
                GatePart::Header(text) | GatePart::Body(text) | GatePart::Note(text) => {
                    lines.push(text.clone())
                }
                GatePart::Section(text) => {
                    lines.push(String::new());
                    lines.push(text.clone());
                }
                GatePart::Field {
                    label,
                    value,
                    block: false,
                } => lines.push(format!("{label}: {value}")),
                GatePart::Field { label, value, .. } => {
                    lines.push(format!("{label}:"));
                    lines.extend(value.split('\n').map(indented));
                }
            }
        }
        if !self.actions.is_empty() && !self.parts.is_empty() {
            lines.push(String::new());
        }
        lines.extend(
            self.actions
                .iter()
                .map(|action| format!("{}  {}", action.command, action.description)),
        );
        lines.join("\n")
    }

    /// Styled rows; `width` sizes the rule above the actions (the pane wraps
    /// long rows afterwards, as it does every row).
    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        let mut lines = Vec::new();
        for part in &self.parts {
            match part {
                // Embedded line breaks are rows, as in a body: a scoping
                // diagnostic's "\n[output truncated]" gets its own.
                GatePart::Header(text) => {
                    let style = Style::default()
                        .fg(self.accent)
                        .add_modifier(Modifier::BOLD);
                    for line in visible(text).split('\n') {
                        lines.push(Line::from(Span::styled(line.to_owned(), style)));
                    }
                }
                GatePart::Section(text) => {
                    lines.push(Line::default());
                    for line in visible(text).split('\n') {
                        lines.push(Line::from(Span::styled(
                            line.to_owned(),
                            Style::default().add_modifier(Modifier::BOLD),
                        )));
                    }
                }
                GatePart::Body(text) => {
                    for line in visible(text).split('\n') {
                        lines.push(body_line(line, self.accent));
                    }
                }
                GatePart::Field {
                    label,
                    value,
                    block: false,
                } => lines.push(Line::from(vec![
                    Span::styled(format!("{}: ", visible(label)), dim),
                    Span::raw(visible(value)),
                ])),
                GatePart::Field { label, value, .. } => {
                    lines.push(Line::from(Span::styled(
                        format!("{}:", visible(label)),
                        dim,
                    )));
                    lines.extend(
                        visible(value)
                            .split('\n')
                            .map(|line| Line::raw(indented(line))),
                    );
                }
                GatePart::Note(text) => {
                    for line in visible(text).split('\n') {
                        lines.push(Line::from(Span::styled(
                            line.to_owned(),
                            dim.add_modifier(Modifier::ITALIC),
                        )));
                    }
                }
            }
        }
        if !self.actions.is_empty() {
            if !self.parts.is_empty() {
                lines.push(Line::from(Span::styled(
                    "─".repeat(width.clamp(8, 60)),
                    dim,
                )));
            }
            let column = self
                .actions
                .iter()
                .map(|action| visible(&action.command).width())
                .filter(|width| *width <= ACTION_COLUMN)
                .max()
                .unwrap_or(0);
            for action in &self.actions {
                let command = visible(&action.command);
                let pad = column.saturating_sub(command.width()) + 2;
                lines.push(Line::from(vec![
                    Span::styled(
                        command,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(visible(&action.description), dim),
                ]));
            }
        }
        lines
    }
}

/// A block field's value line, indented under its label (blank lines stay
/// blank).
fn indented(line: &str) -> String {
    if line.is_empty() {
        String::new()
    } else {
        format!("  {line}")
    }
}

/// One line of a gate body: headings bold, list markers in the accent, the
/// rest as written.
fn body_line(line: &str, accent: Color) -> Line<'static> {
    let content = line.trim_start();
    let indent = &line[..line.len() - content.len()];
    if content.starts_with('#') {
        return Line::from(Span::styled(
            line.to_owned(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    let digits = content.chars().take_while(char::is_ascii_digit).count();
    let marker = if ["- ", "* ", "+ "].iter().any(|m| content.starts_with(m)) {
        2
    } else if digits > 0 && content[digits..].starts_with(". ") {
        digits + 2
    } else {
        0
    };
    if marker == 0 {
        return Line::raw(line.to_owned());
    }
    Line::from(vec![
        Span::raw(indent.to_owned()),
        Span::styled(content[..marker].to_owned(), Style::default().fg(accent)),
        Span::raw(content[marker..].to_owned()),
    ])
}

fn gate(task: &Task, standing: &[String]) -> Gate {
    match task.phase {
        Phase::AwaitingPlan => task
            .plan
            .as_ref()
            .map(|plan| plan_gate(task, plan, standing))
            .unwrap_or_default(),
        Phase::AwaitingSpecApproval => task
            .pending_spec
            .as_ref()
            .map(|pending| {
                spec_approval_gate(
                    &pending.preview,
                    &pending.uncited,
                    pending.scoping_failed.as_deref(),
                )
            })
            .unwrap_or_else(|| {
                let mut gate = Gate::new(Color::Yellow, "SPEC APPROVAL · preview unavailable");
                gate.action("/approve-spec <path>", "run it again");
                gate
            }),
        Phase::AwaitingPolicy => {
            let edit = task.pending_edit.as_ref();
            let mut gate = Gate::new(
                Color::Yellow,
                format!(
                    "EDIT APPROVAL · {}",
                    edit.map_or("pending edit", |e| e.file.as_str())
                ),
            );
            gate.body(edit.map_or("", |e| e.reason.as_str()))
                .action("Tab", "view the Diff and Review")
                .action("/approve", "authorizes this exact edit")
                .action("a message", "sends feedback instead");
            gate
        }
        Phase::AwaitingPermission => task
            .pending_permission
            .as_ref()
            .map(|pending| permission_gate(pending, standing))
            .unwrap_or_else(|| {
                let mut gate =
                    Gate::new(Color::LightRed, "PERMISSION REQUEST · details unavailable");
                gate.action("/deny", "deny it, then retry the task");
                gate
            }),
        Phase::AwaitingChoice => task
            .pending_choice
            .as_ref()
            .map(choice_gate)
            .unwrap_or_else(|| {
                let mut gate = Gate::new(Color::Magenta, "HARNESS QUESTION · details unavailable");
                gate.action("a message", "sends guidance to replan");
                gate
            }),
        Phase::AwaitingReview => review_gate(task),
        // A park where the approved plan stands, or a question the model
        // asked under it: the reply is judged against the plan.
        Phase::AwaitingInput
            if task.plan_stands_park
                || (task.handed_back
                    && !task.turn_finished
                    && task.mode == super::runner::Mode::Auto) =>
        {
            let mut gate = Gate::new(Color::Yellow, "INPUT NEEDED");
            gate.body(task.last_response.as_str())
                .action("a message", "replies to continue the approved plan")
                .action("/plan", "replans");
            gate
        }
        Phase::AwaitingInput => {
            if task.turn_finished {
                let mut gate = Gate::default();
                gate.note("Your turn. Ask a follow-up or describe the next change.");
                gate
            } else if task.last_response.is_empty() {
                Gate::new(Color::Yellow, "Your input is needed. Reply below.")
            } else {
                // A question, an exhausted repair, or an interrupted action:
                // show what is being waited on, not only that something is.
                let mut gate = Gate::new(Color::Yellow, "INPUT NEEDED");
                gate.body(task.last_response.as_str())
                    .action(
                        "a message",
                        "replies with guidance (the task returns to Plan for re-approval)",
                    )
                    .action("/plan", "replans");
                gate
            }
        }
        Phase::Cancelled => {
            let mut gate = Gate::new(Color::LightRed, "Interrupted; obligations are saved.");
            gate.action("/continue", "resumes the work")
                .action("a message", "sends follow-up guidance");
            gate
        }
        Phase::Complete => {
            let mut gate = Gate::new(Color::Green, "Task complete.");
            gate.note("Describe the next request to continue this conversation.");
            gate
        }
        Phase::Incomplete => {
            // The journal's last line says which checks failed and whether
            // anything was captured.
            let mut gate = Gate::new(
                Color::Yellow,
                "Task ended incomplete: the model stayed stuck and the harness finished it as best it could.",
            );
            gate.note("Any records it captured are marked unverified. Describe the next request to continue.");
            gate
        }
        _ => Gate::default(),
    }
}

/// Rule labels the plan gate names before counting the rest.
const OPEN_RULES_SHOWN: usize = 8;

/// The plan and what it leaves to the human: the rules it leaves open, which
/// approval defers (marked when its summary speaks to them), and its open
/// choices.
fn plan_gate(task: &Task, plan: &super::runner::Plan, standing: &[String]) -> Gate {
    let or_none = |items: &[String], separator: &str| {
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(separator)
        }
    };
    let mut gate = Gate::new(Color::Yellow, "PLAN · human approval required");
    gate.body(plan.summary.as_str())
        .field("Files", or_none(&plan.files, ", "))
        .block("Checks", or_none(&plan.checks, "\n"));
    let grants = !task.permission_grants.is_empty() || !standing.is_empty();
    if grants {
        // Grants outlive a replan, so re-approval must not hide them, and
        // standing paths are in force whether or not any exist.
        gate.field(
            "Active sandbox grants",
            format!(
                "{} task · {} standing",
                task.permission_grants.len(),
                standing.len()
            ),
        );
    }
    if !plan.open_rules.is_empty() {
        let mut labels: Vec<String> = plan
            .open_rules
            .iter()
            .take(OPEN_RULES_SHOWN)
            .map(|rule| {
                if rule.mentioned {
                    format!("{} (mentioned in the summary)", rule.label)
                } else {
                    rule.label.clone()
                }
            })
            .collect();
        let more = plan.open_rules.len().saturating_sub(OPEN_RULES_SHOWN);
        if more > 0 {
            labels.push(format!("… and {more} more"));
        }
        gate.block(
            format!("Leaves open {} rule(s)", plan.open_rules.len()),
            labels.join("\n"),
        );
    }
    let satisfied = plan.satisfied_claims();
    if !satisfied.is_empty() {
        // A claim for the human to weigh: the plan says the existing code
        // already holds these rules, so it neither implements nor defers them.
        let mut iris: Vec<String> = satisfied.iter().take(OPEN_RULES_SHOWN).cloned().collect();
        let more = satisfied.len().saturating_sub(OPEN_RULES_SHOWN);
        if more > 0 {
            iris.push(format!("… and {more} more"));
        }
        gate.block(
            format!("Says {} rule(s) already hold", satisfied.len()),
            iris.join("\n"),
        );
    }
    let stubs = plan.stub_files();
    if !stubs.is_empty() {
        // Approving the plan approves these: finishing will not require
        // their stubs to be written.
        gate.block(
            format!("Leaves stubs in {} file(s)", stubs.len()),
            stubs.join("\n"),
        );
    }
    let unchanged = plan.unchanged_files();
    if !unchanged.is_empty() {
        // Listed for reference: finishing will not require them to be edited.
        gate.block(
            format!("Leaves unchanged {} file(s)", unchanged.len()),
            unchanged.join("\n"),
        );
    }
    for (index, choice) in plan.open_choices.iter().enumerate() {
        let chosen = choice
            .answer
            .as_ref()
            .map_or(String::new(), |answer| format!("; chosen: {answer}"));
        gate.field(
            format!("Open choice {}", index + 1),
            format!(
                "{} [{}] (default: {}{chosen})",
                choice.question,
                choice.options.join(" / "),
                choice.default
            ),
        );
    }
    gate.note(SYMBOLIC_APPROVAL).action(
        "/approve",
        if plan.open_rules.is_empty() {
            "execute the plan"
        } else {
            "execute the plan; defers the open rules"
        },
    );
    for index in 1..=plan.open_choices.len() {
        gate.action(
            format!("/choose {index} <option>"),
            format!("answer open choice {index}"),
        );
    }
    gate.action("a message", "sends feedback that revises the plan");
    if grants {
        gate.action("/permissions", "lists the active sandbox grants");
    }
    gate
}

fn review_gate(task: &Task) -> Gate {
    let mut gate = Gate::new(
        Color::LightBlue,
        format!("KNOWLEDGE REVIEW · {} operation(s)", task.reviews.len()),
    );
    gate.body(
        task.capture_reason
            .as_deref()
            .unwrap_or("Review the captured evidence before completion."),
    );
    match review_evidence(task).len() {
        0 => {}
        n => {
            gate.field(
                "Evidence",
                format!("{n} fact(s) the harness checked, see Review"),
            );
        }
    }
    let rework = rework_state(task);
    if rework == Some(true) {
        gate.note("Evidence shows unfinished work.")
            .action("/rework <note>", "sends it back to work");
    }
    gate.action("Tab", "view Review")
        .action(
            "/accept [operation]",
            "accepts one operation, or all displayed",
        )
        .action(
            "/reject [operation]",
            "rejects one operation, or all displayed",
        )
        .action("/no-knowledge", "confirms the no-change assessment");
    if rework == Some(false) {
        gate.action("/rework <note>", "sends it back to work");
    }
    gate
}

fn permission_gate(request: &super::runner::PendingPermission, standing: &[String]) -> Gate {
    let mut gate = if request.is_refused() {
        Gate::new(
            Color::Red,
            "PERMISSION REQUEST · cannot be granted as asked",
        )
    } else {
        Gate::new(
            Color::LightRed,
            "PERMISSION REQUEST · human approval required",
        )
    };
    gate.field("Request", request.request_id.as_str())
        .block("Command", request.command.as_str())
        .block("Reason", request.justification.as_str());
    if request.read_paths.is_empty() {
        gate.field("Read access", "none");
    } else {
        gate.block("Read access", request.read_paths.join("\n"));
    }
    if request.write_paths.is_empty() {
        gate.field("Write access", "none");
    } else {
        gate.block(
            "Write access (create, modify, and delete)",
            request.write_paths.join("\n"),
        );
    }
    gate.field(
        "Network",
        if request.network {
            "enabled"
        } else {
            "disabled"
        },
    );
    if !standing.is_empty() {
        // What is already ambient, so the human judges the delta and not the
        // whole surface.
        gate.field("Already granted standing (every task)", standing.join(", "));
    }
    for line in request.findings.lines() {
        gate.note(format!("Note: {line}"));
    }
    // A refused request is still shown: the human sees what was asked and why
    // it was turned down, instead of the task dying silently.
    match &request.refusal {
        Some(refusal) => {
            gate.block("Refused", refusal.as_str()).action(
                "/deny",
                "dismisses it and tells the model to ask for something narrower",
            );
        }
        None => {
            gate.action(
                "/approve",
                "grants this access for the current task and runs the command",
            )
            .action("/deny", "refuses it");
        }
    }
    gate
}

fn choice_gate(choice: &super::runner::PendingChoice) -> Gate {
    let mut gate = Gate::new(Color::Magenta, "HARNESS QUESTION");
    gate.body(choice.prompt.as_str());
    for option in &choice.options {
        let default = if option.key == choice.default {
            " (default)"
        } else {
            ""
        };
        gate.action(
            format!("/choose {}", option.key),
            format!("{}{default}", option.label),
        );
    }
    gate.action("a message", "sends guidance: the task returns to Plan");
    gate
}

fn spec_approval_gate(
    preview: &SpecPrepareResponse,
    uncited: &[SpecUncited],
    scoping_failed: Option<&str>,
) -> Gate {
    let mut gate = Gate::new(Color::Yellow, "SPEC APPROVAL · human approval required");
    gate.field("Source", preview.path.as_str())
        .field("SHA-256", preview.source_sha256.as_str())
        .field("Knowledge revision", preview.knowledge_revision.as_str());
    if preview.already_approved {
        gate.section("UNCHANGED · this source and active record set are already approved");
    }
    match &preview.component {
        Some(plan) => {
            gate.section(format!(
                "COMPONENT · {} · {}",
                plan.name,
                component_state(plan)
            ))
            .field("Covers", plan.covers.join(" "))
            .field("IRI", plan.iri.as_str())
            .note(if preview.parts.is_empty() {
                "Every record below concerns this component; its rules govern the files it covers."
            } else {
                "Records without a part below concern this component; its rules govern every file it covers, parts included."
            });
            for part in &preview.parts {
                gate.section(format!(
                    "PART · {} · {} · Covers {} · {} record(s) · stated by \"{}\"",
                    part.plan.name,
                    component_state(&part.plan),
                    part.plan.covers.join(" "),
                    part.records.len(),
                    part.stated_by,
                ))
                .field("IRI", part.plan.iri.as_str());
            }
            if let Some(diagnostic) = scoping_failed {
                gate.section(format!(
                    "SCOPING FAILED · every record governs component {}. {diagnostic}",
                    plan.name
                ));
            }
        }
        // Floating records are findable only by lexical luck: say so before
        // the human accepts, and name the command that anchors them.
        None => {
            gate.section("COMPONENT · none").body(format!(
                "The records below will not be linked to any component or code, so their Constraints will not govern implementation. To anchor them, run /approve-spec {} <dir/ | file | .> instead (an unchanged batch is re-approved with the link).",
                preview.path
            ));
        }
    }
    for (index, entry) in preview.entries.iter().enumerate() {
        let part = preview
            .parts
            .iter()
            .find(|part| part.records.contains(&index))
            .map(|part| format!(" · part {}", part.plan.name))
            .unwrap_or_default();
        let (effect, identity) = match &entry.disposition {
            SpecDisposition::New { iri } => ("NEW", iri.clone()),
            SpecDisposition::Reuse { iri } => ("REUSE", iri.clone()),
            SpecDisposition::Supersede { iri, previous_iri } => {
                ("SUPERSEDE", format!("{previous_iri} -> {iri}"))
            }
        };
        gate.section(format!(
            "{effect} · {} · {}{part}",
            entry.draft.kind, entry.draft.title
        ))
        .field("Extracted claim", entry.draft.description.as_str())
        .field("IRI", identity)
        .block("Evidence", entry.draft.evidence.join("\n"));
        if let Some(existing) = &entry.existing {
            gate.field(
                "Existing accepted record",
                format!("{} · {}", existing.title, existing.iri),
            )
            .block("Existing claim and evidence", existing.description.as_str());
        }
    }
    for retirement in &preview.retirements {
        let effect = match retirement.disposition {
            SpecRetirementDisposition::Retract => "RETRACT",
            SpecRetirementDisposition::RetainShared => "RETAIN SHARED",
        };
        gate.section(format!(
            "{effect} · {} · {}",
            retirement.kind, retirement.title
        ))
        .field("IRI", retirement.iri.as_str())
        .block(
            "Existing claim and evidence",
            retirement.description.as_str(),
        );
    }
    if let Some(previous) = &preview.previous_approval_iri {
        gate.section(if preview.already_approved {
            "CURRENT APPROVAL"
        } else {
            "SUPERSEDE PRIOR APPROVAL"
        })
        .field("IRI", previous.as_str());
    }
    // What the batch leaves out is as much a part of the judgment as what it
    // holds: a section no record cites will never govern anything.
    if !uncited.is_empty() {
        gate.section(format!(
            "UNCITED · {} range(s) of {} no record above cites; they will not become project knowledge",
            uncited.len(),
            preview.path
        ))
        .body(
            uncited
                .iter()
                .map(|range| format!("  {}", range.describe()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    gate.section(format!("Approval marker: spec-approval: {}", preview.path))
        .note("A separate /approve is still required before code execution.")
        .action(
            "/approve-spec",
            "records this exact batch (or say ‘I approve the spec’)",
        );
    gate
}

fn component_state(plan: &SpecComponentPlan) -> String {
    if plan.new {
        "NEW".to_string()
    } else if plan.added.is_empty() {
        "existing".to_string()
    } else {
        format!("existing, adding {}", plan.added.join(" "))
    }
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
    let mut control = snapshot.task.as_ref().map_or_else(Gate::default, |task| {
        gate(task, &snapshot.standing_read_paths)
    });
    if let Some(task) = &snapshot.task {
        if !task.reviews.is_empty() && task.phase != Phase::AwaitingReview {
            control
                .note(format!(
                    "{} knowledge review(s) pending",
                    task.reviews.len()
                ))
                .action("/review", "shows them, or view Review");
        }
    }
    if !snapshot.conversation.queued.is_empty() {
        control.note(format!(
            "QUEUED · {} message(s), delivered before the next action",
            snapshot.conversation.queued.len()
        ));
    }
    if !control.is_empty() {
        lines.extend(control.lines(view.content_area.width as usize));
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
                    let drops = task.review_drops.get(&review.request.operation_id);
                    for (index, proposal) in review.request.proposals.iter().enumerate() {
                        let dropped = drops.is_some_and(|drops| drops.contains(&index));
                        text.push_str(&format!(
                            "\n[{}]{} {} · {}\n{}\n\nEvidence\n{}\n",
                            index + 1,
                            if dropped { " [dropped]" } else { "" },
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
                        for record in proposal.requirement.iter().chain(&proposal.motivated_by) {
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
                if task.reviews.is_empty() {
                    // No card shows the note's evidence: a no-knowledge review.
                    text.push_str(&evidence_lines(review_evidence(task)));
                }
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
/// The harness's facts beside a capture note, one per line.
fn evidence_lines(evidence: &[String]) -> String {
    if evidence.is_empty() {
        return String::new();
    }
    let mut text = String::from("Evidence (checked by the harness)\n");
    for fact in evidence {
        text.push_str(&format!("- {fact}\n"));
    }
    text
}

/// At the final review, whether its evidence shows work left unfinished;
/// `None` elsewhere, where /rework does not apply.
fn rework_state(task: &Task) -> Option<bool> {
    task.at_final_review().then(|| {
        review_evidence(task).iter().any(|fact| {
            fact.starts_with("Planned files not edited")
                || fact.starts_with("Stubs left")
                || fact.starts_with("Required checks failed")
        })
    })
}

/// The current capture note's evidence, if any.
fn review_evidence(task: &Task) -> &[String] {
    task.symbolic
        .as_ref()
        .and_then(|symbolic| symbolic.capture_note.as_ref())
        .map_or(&[], |note| note.evidence.as_slice())
}

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
    text.push_str(&evidence_lines(&note.evidence));
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
        if !proposal.names_rules.is_empty() {
            text.push_str(&format!(
                "  Names governing rule(s): {} — accepting records a decision about them.\n",
                proposal.names_rules.join(", ")
            ));
        }
    }
    for dropped in &typed.dropped {
        text.push_str(&format!(
            "Refused · {} · {} — {}\n",
            dropped.kind, dropped.title, dropped.reason
        ));
    }
    if !review.request.proposals.is_empty() {
        text.push_str("/drop <n> leaves proposal n out of this capture; /accept keeps the rest.\n");
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
            Line::from(
                [
                    Span::styled(" MOOSEDev ", Style::default().fg(Color::Cyan)),
                    Span::raw(visible(&format!("{} · {}", snapshot.model, phase))),
                ]
                .into_iter()
                .chain(snapshot.task.as_ref().and_then(checker_span))
                .collect::<Vec<_>>(),
            ),
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
        KeyCode::PageDown => scroll_down(view, 12),
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
                scroll_down(view, 1)
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

/// Scrolls toward the tail; reaching it resumes following new output, so
/// scrolling back down after reading history keeps the tail in view again.
fn scroll_down(view: &mut View, lines: u16) {
    view.scroll = view.scroll.saturating_add(lines).min(view.scroll_max);
    if view.scroll >= view.scroll_max {
        view.follow = true;
    }
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
            scroll_down(view, MOUSE_SCROLL_LINES);
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
    startup_notices: Vec<String>,
) -> Result<()> {
    let terminated = super::crash::termination()?;
    tokio::pin!(terminated);
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
            .with_startup_notices(startup_notices)
            .run(),
    );
    let mut view = View {
        follow: true,
        ..View::default()
    };
    let mut interval = tokio::time::interval(Duration::from_millis(40));
    // What ended the loop, for the crash log if the controller must be aborted.
    let mut ending = String::from("the interface stopping");
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
                // A closed terminal or a `kill` ends the session like /quit:
                // the controller saves, the screen is restored, the marker goes.
                signal = &mut terminated => {
                    super::crash::log(&format!("session ended by signal {signal}"));
                    ending = format!("signal {signal}");
                    break;
                },
                _ = interval.tick() => {
                    // A panic on any thread restores the terminal from the
                    // hook; drawing after that paints over the shell.
                    anyhow::ensure!(
                        SCREEN_ACTIVE.load(Ordering::SeqCst),
                        "the interface stopped after a panic; see .moosedev/harness/crash.log"
                    );
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
                                finish_controller(&mut controller, "/quit", &snapshot).await;
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
        finish_controller(&mut controller, &ending, &snapshot).await;
    }
    result
}

/// Give the controller three seconds to save and stop, then abort it: a quit
/// must not hang on a server that never answers. Work still running then
/// (a model step's cancellation, a spec extraction whose daemon result would
/// be recorded) is dropped, and the exit is otherwise clean, so the crash log
/// names what was abandoned.
async fn finish_controller(
    controller: &mut tokio::task::JoinHandle<()>,
    ending: &str,
    snapshot: &Snapshot,
) {
    if tokio::time::timeout(Duration::from_secs(3), &mut *controller)
        .await
        .is_err()
    {
        controller.abort();
        let task = snapshot
            .task
            .as_ref()
            .map_or_else(|| "no task".into(), |task| format!("task {}", task.id));
        let work = if snapshot.busy {
            format!("work in flight: {}", snapshot.status)
        } else {
            format!("last status: {}", snapshot.status)
        };
        super::crash::log(&format!(
            "session controller did not stop within 3 s of {ending} and was aborted; abandoned {task}, {work}"
        ));
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

/// `notices` (earlier sessions' unclean ends) open the transcript.
pub async fn interactive(
    root: PathBuf,
    daemon: Option<String>,
    daemon_exe: Option<PathBuf>,
    launch: Launch,
    notices: Vec<String>,
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
        notices.into_iter().chain(config_error).collect(),
    )
    .await
}

/// Compatibility entry point for an existing headless task.
pub async fn run(runner: Runner, notices: Vec<String>) -> Result<()> {
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
        notices,
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
    #[test]
    fn the_header_shows_what_the_language_server_last_said() {
        let mut task = task_fixture(PathBuf::from("/project"));
        assert!(checker_span(&task).is_none(), "no checker, no indicator");
        let snapshot = |settled, errors: usize| crate::harness::runner::DiagnosticsSnapshot {
            servers: vec!["rust-analyzer".into()],
            settled,
            errors: (0..errors)
                .map(|line| crate::harness::runner::Finding {
                    file: "src/lib.rs".into(),
                    line: line as u32 + 1,
                    column: 1,
                    message: "mismatched types".into(),
                    detail: None,
                    definition: None,
                    declared: Vec::new(),
                    fixes: vec![],
                    fixes_complete: false,
                })
                .collect(),
            warnings: vec![],
            lints: vec![],
            linter: None,
            finish_refused: false,
        };
        for (settled, errors, shown) in [
            (true, 0, " · rust-analyzer ✓"),
            (true, 2, " · rust-analyzer: 2 error(s)"),
            (false, 2, " · rust-analyzer ?"),
        ] {
            task.diagnostics = Some(snapshot(settled, errors));
            assert_eq!(checker_span(&task).unwrap().content, shown);
        }
        let mut linted = snapshot(true, 0);
        linted.linter = Some("clippy".into());
        linted.lints = snapshot(true, 3).errors;
        task.diagnostics = Some(linted);
        assert_eq!(
            checker_span(&task).unwrap().content,
            " · rust-analyzer: 3 lint(s)"
        );
        // The compiler's warnings count too: not green while any remain.
        let mut warned = snapshot(true, 1);
        warned.warnings = snapshot(true, 6).errors;
        task.diagnostics = Some(warned);
        assert_eq!(
            checker_span(&task).unwrap().content,
            " · rust-analyzer: 1 error(s), 6 warning(s)"
        );
    }

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
    fn the_plan_gate_names_open_rules_and_choices() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingPlan;
        let rule = |n: usize| super::super::runner::OpenRule {
            iri: format!("urn:rule:{n}"),
            label: format!("Rule {n}"),
            kind: "Constraint".into(),
            mentioned: n == 2,
        };
        task.plan = Some(super::super::runner::Plan {
            summary: "Trim label whitespace".into(),
            files: vec!["labels.py".into()],
            checks: vec!["pytest -q".into()],
            addresses: vec![],
            satisfied: vec![],
            stubs: vec![],
            unchanged: vec![],
            open_rules: vec![rule(1), rule(2)],
            open_choices: vec![super::super::runner::OpenChoice {
                question: "Which separator?".into(),
                options: vec!["space".into(), "dash".into(), "none".into()],
                default: "space".into(),
                answer: None,
            }],
        });
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.contains(
                "\nLeaves open 2 rule(s):\n  Rule 1\n  Rule 2 (mentioned in the summary)\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "\nOpen choice 1: Which separator? [space / dash / none] (default: space)\n"
            ),
            "{text}"
        );
        assert!(
            text.ends_with("\n\n/approve  execute the plan; defers the open rules\n/choose 1 <option>  answer open choice 1\na message  sends feedback that revises the plan"),
            "{text}"
        );

        let plan = task.plan.as_mut().unwrap();
        plan.open_rules = (1..=10).map(rule).collect();
        plan.open_choices[0].answer = Some("dash".into());
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.contains("Leaves open 10 rule(s):\n  Rule 1\n  Rule 2 (mentioned in the summary)\n  Rule 3\n  Rule 4\n  Rule 5\n  Rule 6\n  Rule 7\n  Rule 8\n  … and 2 more\n"),
            "{text}"
        );
        assert!(text.contains("(default: space; chosen: dash)"), "{text}");
        assert!(!text.contains("already hold"), "{text}");

        let plan = task.plan.as_mut().unwrap();
        plan.satisfied = vec!["urn:rule:held".into()];
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.contains("\nSays 1 rule(s) already hold:\n  urn:rule:held\n"),
            "{text}"
        );

        let plan = task.plan.as_mut().unwrap();
        plan.stubs = vec!["labels.py".into()];
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.contains("\nLeaves stubs in 1 file(s):\n  labels.py\n"),
            "{text}"
        );

        let plan = task.plan.as_mut().unwrap();
        plan.stubs.clear();
        plan.unchanged = vec!["labels.py".into()];
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.contains("\nLeaves unchanged 1 file(s):\n  labels.py\n"),
            "{text}"
        );

        let plan = task.plan.as_mut().unwrap();
        plan.open_rules.clear();
        plan.open_choices.clear();
        plan.satisfied.clear();
        plan.unchanged.clear();
        let text = gate(&task, &[]).to_plain();
        assert!(!text.contains("Leaves open") && !text.contains("Open choice"));
        assert!(!text.contains("Leaves stubs"), "{text}");
        assert!(!text.contains("Leaves unchanged"), "{text}");
        assert!(!text.contains("/choose") && text.contains("/approve  execute the plan\n"));
    }

    #[test]
    fn the_plan_gate_renders_as_styled_sections_with_actions_last() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingPlan;
        task.plan = Some(super::super::runner::Plan {
            summary: "## Approach\nTrim label whitespace\n- strip both ends".into(),
            files: vec!["labels.py".into()],
            checks: vec!["pytest -q".into()],
            addresses: vec![],
            satisfied: vec![],
            stubs: vec![],
            unchanged: vec![],
            open_rules: vec![],
            open_choices: vec![],
        });
        let lines = gate(&task, &[]).lines(40);
        let row = |index: usize| lines[index].to_string();
        let dim = Style::default().fg(Color::DarkGray);
        let bold = Style::default().add_modifier(Modifier::BOLD);

        assert_eq!(row(0), "PLAN · human approval required");
        assert_eq!(lines[0].spans[0].style, bold.fg(Color::Yellow));
        // The summary keeps its lines, in the default colour, not the accent.
        assert_eq!(row(1), "## Approach");
        assert_eq!(lines[1].spans[0].style, bold);
        assert_eq!(lines[2].spans[0].style, Style::default());
        assert_eq!(row(3), "- strip both ends");
        assert_eq!(lines[3].spans[1].style, Style::default().fg(Color::Yellow));
        assert_eq!(lines[3].spans[2].style, Style::default());
        // Fields: a dim label and a plain value, block values indented.
        assert_eq!(row(4), "Files: labels.py");
        assert_eq!(lines[4].spans[0].style, dim);
        assert_eq!(lines[4].spans[1].style, Style::default());
        assert_eq!((row(5), row(6)), ("Checks:".into(), "  pytest -q".into()));
        assert_eq!(row(7), SYMBOLIC_APPROVAL);
        assert_eq!(lines[7].spans[0].style, dim.add_modifier(Modifier::ITALIC));
        // The human's actions come last, after a rule, as commands in a
        // column beside dim descriptions.
        assert_eq!(row(8), "─".repeat(40));
        assert_eq!(row(9), "/approve   execute the plan");
        assert_eq!(row(10), "a message  sends feedback that revises the plan");
        assert_eq!(lines.len(), 11);
        let command = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        for line in &lines[9..] {
            assert_eq!(line.spans[0].style, command);
            assert_eq!(line.spans[2].style, dim);
        }

        // The conversation pane shows the gate the same way.
        let mut view = View::default();
        view.content_area.width = 40;
        let text = text_content(&conversation_body(&snapshot_for(task), &view));
        assert!(
            text.ends_with(&format!(
                "{}\n/approve   execute the plan\na message  sends feedback that revises the plan",
                "─".repeat(40)
            )),
            "{text}"
        );
    }

    /// A section's embedded line breaks are rows, as a body's are: a long
    /// scoping diagnostic's "\n[output truncated]" suffix keeps its own row.
    #[test]
    fn gate_headers_and_sections_split_embedded_line_breaks_into_rows() {
        let mut gate = Gate::new(Color::Yellow, "SPEC · approval\nsecond line");
        gate.section(
            "SCOPING FAILED · every record governs component x. error: y\n[output truncated]",
        );
        let lines = gate.lines(40);
        let rows: Vec<String> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(
            rows,
            [
                "SPEC · approval",
                "second line",
                "",
                "SCOPING FAILED · every record governs component x. error: y",
                "[output truncated]",
            ]
        );
        let bold = Style::default().add_modifier(Modifier::BOLD);
        assert_eq!(lines[1].spans[0].style, bold.fg(Color::Yellow));
        assert_eq!(lines[4].spans[0].style, bold);
    }

    #[test]
    fn approval_gate_explains_symbolic_derivation() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingPlan;
        task.plan = Some(super::super::runner::Plan {
            summary: "Trim label whitespace".into(),
            files: vec!["labels.py".into()],
            checks: vec!["pytest -q".into()],
            addresses: vec![],
            satisfied: vec![],
            stubs: vec![],
            unchanged: vec![],
            open_rules: vec![],
            open_choices: vec![],
        });
        let text = gate(&task, &[]).to_plain();
        assert!(text.contains("PLAN · human approval required"));
        assert!(text.contains(SYMBOLIC_APPROVAL));
        assert!(text.contains("no model call"));
        assert!(text.starts_with(
            "PLAN · human approval required\nTrim label whitespace\nFiles: labels.py\nChecks:\n  pytest -q\n"
        ));
        assert!(text.ends_with("a message  sends feedback that revises the plan"));
        assert!(!text.contains("Active sandbox grants") && !text.contains("/permissions"));

        task.permission_grants = vec![serde_json::from_value(serde_json::json!({
            "id": "grant-1",
            "justification": "Fetch dependencies",
            "read_paths": [],
            "write_paths": [],
            "network": true,
            "approved_at": "2026-09-21T00:00:00Z"
        }))
        .unwrap()];
        let text = gate(&task, &[]).to_plain();
        assert!(text.contains("Active sandbox grants: 1 task · 0 standing"));
        assert!(text.ends_with("/permissions  lists the active sandbox grants"));
        // Standing paths are in force with no approval, so re-approving a plan
        // must show them even when the task itself has been granted nothing.
        let standing = ["/opt/toolchain".to_string()];
        task.permission_grants.clear();
        let text = gate(&task, &standing).to_plain();
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
        let text = gate(&task, &[]).to_plain();
        assert!(text.starts_with("INPUT NEEDED\naction failed validation after three attempts."));
        assert!(text.ends_with(
            "\n\na message  replies with guidance (the task returns to Plan for re-approval)\n/plan  replans"
        ));

        task.last_response = "Which database should the skeleton use?".into();
        assert!(gate(&task, &[])
            .to_plain()
            .contains("Which database should the skeleton use?"));

        task.last_response.clear();
        assert_eq!(
            gate(&task, &[]).to_plain(),
            "Your input is needed. Reply below."
        );

        task.turn_finished = true;
        assert!(gate(&task, &[]).to_plain().starts_with("Your turn."));

        // A park where the approved plan stands shows its reason, not only
        // "Your turn", and says the reply continues the plan.
        task.plan_stands_park = true;
        task.last_response =
            "The model keeps asking to read files whose current text it already has (a.rs last)."
                .into();
        let text = gate(&task, &[]).to_plain();
        assert!(text.starts_with("INPUT NEEDED\nThe model keeps asking to read files"));
        assert!(
            text.ends_with("\n\na message  replies to continue the approved plan\n/plan  replans")
        );
    }
    #[test]
    fn the_final_review_gate_offers_rework_and_says_when_work_is_left() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingReview;
        assert!(!gate(&task, &[]).to_plain().contains("/rework"));
        // `final_capture` is the runner's own; a journal can carry it.
        let mut value = serde_json::to_value(&task).unwrap();
        value["final_capture"] = true.into();
        let mut task: Task = serde_json::from_value(value).unwrap();
        assert!(task.at_final_review());
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.ends_with("/no-knowledge  confirms the no-change assessment\n/rework <note>  sends it back to work"),
            "{text}"
        );
        assert!(!text.contains("unfinished work"));
        task.symbolic = Some(super::super::runner::SymbolicState {
            capture_note: Some(super::super::runner::CaptureNoteState {
                operation_id: "note".into(),
                capture_operation_id: "capture".into(),
                note_event: 0,
                note: "Done.".into(),
                status: "captured".into(),
                response: None,
                evidence: vec!["Planned files not edited: b.rs.".into()],
            }),
            ..Default::default()
        });
        let text = gate(&task, &[]).to_plain();
        // Unfinished work puts /rework first, under a note that says why.
        assert!(
            text.contains("Evidence: 1 fact(s) the harness checked, see Review\nEvidence shows unfinished work.\n\n/rework <note>  sends it back to work\nTab  view Review\n"),
            "{text}"
        );
        assert!(text.ends_with("/no-knowledge  confirms the no-change assessment"));
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
        let text = gate(&task, &[]).to_plain();
        assert!(text.contains("PERMISSION REQUEST · human approval required"));
        assert!(text.contains("Request: permission-1"));
        assert!(text.contains("Command:\n  cargo install example"));
        assert!(text.contains("Read access:\n  /opt/toolchain"));
        assert!(text.contains("Write access (create, modify, and delete):\n  /tmp/tool-cache"));
        assert!(text.contains("Network: enabled"));
        assert!(text.contains("Reason:\n  Install the required local tool"));
        assert!(text.contains(
            "\n\n/approve  grants this access for the current task and runs the command\n/deny  refuses it"
        ));
    }
    #[test]
    fn choice_gate_lists_each_option_as_a_command_with_the_default_marked() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingChoice;
        task.pending_choice = Some(super::super::runner::PendingChoice {
            id: "choice-1".into(),
            kind: super::super::runner::ChoiceKind::ScopeAdd {
                file: "src/error.rs".into(),
            },
            prompt: "The model wants to edit `src/error.rs`, which is outside the approved plan (src/codes.rs).".into(),
            options: [
                ("add", "Add src/error.rs to the approved plan; the model makes its edit"),
                ("replan", "Return to Plan to rework the plan"),
                ("refuse", "Refuse: the model continues within the plan"),
            ]
            .into_iter()
            .map(|(key, label)| super::super::runner::ChoiceOption {
                key: key.into(),
                label: label.into(),
            })
            .collect(),
            default: "add".into(),
        });
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.starts_with("HARNESS QUESTION\nThe model wants to edit `src/error.rs`"),
            "{text}"
        );
        assert!(
            text.contains(
                "/choose add  Add src/error.rs to the approved plan; the model makes its edit (default)\n"
            ),
            "{text}"
        );
        assert!(text.contains("/choose replan  Return to Plan to rework the plan\n"));
        assert!(text.contains("/choose refuse  Refuse: the model continues within the plan\n"));
        assert!(text.ends_with("a message  sends guidance: the task returns to Plan"));
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
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.contains("\nCOMPONENT · none\nThe records below will not be linked"),
            "{text}"
        );
        assert!(
            text.contains("UNCITED · 2 range(s) of badciv-map.md no record above cites; they will not become project knowledge\n  lines 20-50 (## Faction Rules)\n  line 61\n"),
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
        let text = gate(&task, &[]).to_plain();
        assert!(text.contains("SPEC APPROVAL · human approval required"));
        assert!(text.contains(
            "\nCOMPONENT · labels · NEW\nCovers: labels/\nIRI: https://moosedev.dev/kg/SystemComponent/labels\n"
        ));
        assert!(text.contains(
            "NEW · Requirement · Preserve labels\nExtracted claim: Labels retain meaningful whitespace."
        ));
        assert!(text.contains("Evidence:\n  specs/labels.md:7-8"));
        assert!(text.contains("Existing claim and evidence:\n  Processing remains local.\n\n  Evidence:\n  - specs/original.md:4"));
        assert!(text.contains("Existing claim and evidence:\n  Labels had the old bound."));
        assert!(text.contains("Existing claim and evidence:\n  The old claim."));
        assert!(text.contains("REUSE · Constraint · Local only"));
        assert!(text.contains("SUPERSEDE · Constraint · Bounded labels"));
        assert!(text.contains("RETRACT · Requirement · Old behavior"));
        assert!(text.contains("RETAIN SHARED · Constraint · Shared behavior"));
        assert!(text.contains("SUPERSEDE PRIOR APPROVAL\nIRI: https://moosedev.dev/kg/ArchitecturalDecision/approval-v1"));
        assert!(text.contains("Approval marker: spec-approval: specs/labels.md"));
        assert!(text.ends_with("A separate /approve is still required before code execution.\n\n/approve-spec  records this exact batch (or say ‘I approve the spec’)"));
    }
    #[test]
    fn spec_gate_shows_each_part_and_tags_the_records_it_governs() {
        let mut task = task_fixture(PathBuf::from("/project"));
        task.phase = Phase::AwaitingSpecApproval;
        task.pending_spec = Some(serde_json::from_value(serde_json::json!({
            "preview": {
                "operation_id": "spec-2", "owner_id": "task-1", "path": "spec.md",
                "source_sha256": "0123456789abcdef", "knowledge_revision": "accepted-v3",
                "entries": [
                    {"draft": {"kind": "Constraint", "title": "Rust only", "description": "Written in Rust.", "evidence": ["spec.md:1"]},
                     "disposition": {"kind": "new", "iri": "https://moosedev.dev/kg/Constraint/rust"}},
                    {"draft": {"kind": "Requirement", "title": "Faction weaknesses", "description": "Each faction has a weakness.", "evidence": ["spec.md:2"]},
                     "disposition": {"kind": "new", "iri": "https://moosedev.dev/kg/Requirement/factions"}}
                ],
                "retirements": [],
                "component": {"iri": "https://moosedev.dev/kg/SystemComponent/spec", "name": "spec", "new": true, "covers": ["."], "added": ["."]},
                "parts": [{"plan": {"iri": "https://moosedev.dev/kg/SystemComponent/sim", "name": "sim", "new": true, "covers": ["sim/"], "added": ["sim/"]},
                           "stated_by": "sim crate responsibility", "records": [1]}]
            }
        })).unwrap());
        let text = gate(&task, &[]).to_plain();
        assert!(
            text.contains("Records without a part below concern this component"),
            "{text}"
        );
        assert!(text.contains(
            "\nPART · sim · NEW · Covers sim/ · 1 record(s) · stated by \"sim crate responsibility\"\nIRI: https://moosedev.dev/kg/SystemComponent/sim\n"
        ), "{text}");
        assert!(
            text.contains("NEW · Requirement · Faction weaknesses · part sim\n"),
            "{text}"
        );
        assert!(text.contains("NEW · Constraint · Rust only\n"), "{text}");

        // A failed scoping says every record stays with the spec's component.
        let pending = task.pending_spec.as_mut().unwrap();
        pending.preview.parts.clear();
        pending.scoping_failed =
            Some("part sim is stated by record 0, which does not name it".into());
        let text = gate(&task, &[]).to_plain();
        assert!(text.contains(
            "SCOPING FAILED · every record governs component spec. part sim is stated by record 0"
        ), "{text}");
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
                decided_by: Vec::new(),
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
                     "disposition": {"kind": "distinct", "receipt_operation_id": "r3"},
                     "names_rules": ["Preserve names", "Label intent"]}
                ],
                "dropped": [{"kind": "Constraint", "title": "TUI owns configuration", "reason": "not typed from a coding note"}]
            }
        }))
        .unwrap());
        task.review_drops.insert("cap-1".into(), vec![0]);
        let text = text_content(&body(&snapshot_for(task), &review_view()));
        assert!(text.contains("REVIEW 1\nFinal checkpoint\n\nCapture note\nNames are stripped before comparison.\nTyping: symbolic with sensor · sensor added one proposal\n"));
        assert!(text.contains("SymbolicDecision · ArchitecturalDecision · Trim label whitespace — restates https://moosedev.dev/kg/Requirement/label-intent; no record proposed\n"));
        assert!(text.contains("SymbolicLesson · Lesson · Strip before comparing — refines https://moosedev.dev/kg/Constraint/preserve-names (0.63)\n"));
        assert!(text.contains("LlmSensor · Pattern · Normalize then compare — new record\n  Names governing rule(s): Preserve names, Label intent — accepting records a decision about them.\n"));
        assert!(text.contains("/accept 1 · /reject 1"));
        // Refused proposals are named, numbers match /drop, and a dropped
        // proposal is marked.
        assert!(text.contains(
            "Refused · Constraint · TUI owns configuration — not typed from a coding note\n"
        ));
        assert!(text.contains("/drop <n> leaves proposal n out of this capture"));
        assert!(
            text.contains("[1] [dropped] Lesson · Strip before comparing\n"),
            "{text}"
        );
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

        // Scrolling back down to the tail resumes following it.
        while view.scroll + MOUSE_SCROLL_LINES < view.scroll_max {
            assert!(mouse(&mut view, event(MouseEventKind::ScrollDown)));
            assert!(!view.follow, "not at the tail yet: {}", view.scroll);
        }
        assert!(mouse(&mut view, event(MouseEventKind::ScrollDown)));
        assert_eq!(view.scroll, 10);
        assert!(view.follow);
        sync_scroll_bounds(&mut view, 30, 10);
        assert_eq!(view.scroll, 20, "following keeps new output in view");

        assert!(mouse(&mut view, event(MouseEventKind::ScrollUp)));
        assert!(!view.follow);
    }
    #[test]
    fn keyboard_scrolling_down_to_the_tail_resumes_following() {
        for tab in [0, 1] {
            let mut view = View {
                tab,
                follow: true,
                ..View::default()
            };
            sync_scroll_bounds(&mut view, 40, 10);
            assert_eq!(view.scroll, 30);

            key(
                &mut view,
                KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            );
            assert_eq!((view.scroll, view.follow), (18, false));
            key(&mut view, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
            assert_eq!((view.scroll, view.follow), (17, false));
            // Output arriving while scrolled up does not move the view.
            sync_scroll_bounds(&mut view, 45, 10);
            assert_eq!(view.scroll, 17);

            key(
                &mut view,
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            );
            assert_eq!((view.scroll, view.follow), (29, false));
            key(&mut view, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
            assert_eq!((view.scroll, view.follow), (30, false));
            key(
                &mut view,
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            );
            assert_eq!((view.scroll, view.follow), (35, true));
            sync_scroll_bounds(&mut view, 50, 10);
            assert_eq!(view.scroll, 40, "the tail stays in view");

            // Alt-Down onto the last line resumes following too.
            key(&mut view, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
            assert!(!view.follow);
            key(&mut view, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
            assert_eq!((view.scroll, view.follow), (40, true));
        }
        // The transcript tabs follow by default when switched to.
        let mut view = View {
            tab: 5,
            ..View::default()
        };
        key(&mut view, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(view.tab == 0 && view.follow);
        key(&mut view, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(view.tab == 1 && view.follow);
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

//! Thin interactive frontend. Human review commands never enter the model action surface.
use super::runner::{Phase, Runner, Task};
use super::{
    session::{Command, Controller, Conversation, Snapshot, Update},
    startup::{ProviderSettings, StartupOptions},
};
use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use ratatui::{
    layout::{Position, Rect},
    text::{Line, Span},
};
use std::{
    collections::VecDeque,
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

#[derive(Debug)]
pub enum Action {
    Step,
    Run,
    Approve,
    ApprovePolicy,
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
static PANIC_HOOK: Once = Once::new();

fn restore_terminal() {
    if SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
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
            execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
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

#[derive(Default)]
struct View {
    tab: usize,
    scroll: u16,
    follow: bool,
    composer: Composer,
    tick: usize,
    activity_expanded: bool,
    notice: String,
    retained_inputs: VecDeque<String>,
}

fn visible(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

fn gate(task: &Task) -> String {
    match task.phase {
        Phase::AwaitingPlan => task.plan.as_ref().map(|plan|format!("PLAN · human approval required\n{}\nFiles: {}\nChecks:\n{}\n/approve to execute · send feedback to revise",plan.summary,plan.files.join(", "),plan.checks.join("\n"))).unwrap_or_default(),
        Phase::AwaitingPolicy => format!("EDIT APPROVAL · {}\n{}\nView Diff (Tab), then /approve or send feedback.",task.pending_edit.as_ref().map(|e|e.file.as_str()).unwrap_or("pending edit"),task.pending_edit.as_ref().map(|e|e.reason.as_str()).unwrap_or("")),
        Phase::AwaitingReview => format!("KNOWLEDGE REVIEW · {} operation(s)\n{}\nView Knowledge (Tab) · /accept [operation] · /reject [operation] · /no-knowledge",task.reviews.len(),task.capture_reason.as_deref().unwrap_or("Review the captured evidence before completion.")),
        Phase::AwaitingInput => if task.turn_finished { "Your turn. Ask a follow-up or describe the next change.".into() } else { "Your input is needed. Reply below.".into() },
        Phase::Cancelled => "Interrupted; obligations are saved. /continue resumes, or send follow-up guidance.".into(),
        Phase::Complete => "Task complete. Describe the next request to continue this conversation.".into(),
        _ => String::new(),
    }
}

fn body(snapshot: &Snapshot, view: &View) -> String {
    let mut text = String::new();
    match view.tab {
        0 | 1 => {
            for message in &snapshot.conversation.messages {
                if message.role == "activity" && view.tab == 0 && !view.activity_expanded {
                    continue;
                }
                let label = match message.role.as_str() {
                    "user" => "YOU",
                    "assistant" => "MOOSEDev",
                    "activity" => "ACTIVITY",
                    _ => "SESSION",
                };
                text.push_str(&format!("{label}\n{}\n\n", message.text));
            }
            let live = snapshot.live.lock().unwrap();
            if !live.assistant.is_empty() {
                text.push_str(&format!("MOOSEDev\n{}▌\n\n", live.assistant));
            }
            if !live.command.is_empty() {
                text.push_str(&format!("COMMAND OUTPUT\n{}\n\n", live.command));
            }
            if let Some(task) = &snapshot.task {
                text.push_str(&gate(task));
                if !task.reviews.is_empty() && task.phase != Phase::AwaitingReview {
                    text.push_str(&format!(
                        "\n\n{} knowledge review(s) pending · /review or view Knowledge",
                        task.reviews.len()
                    ));
                }
            }
            if !snapshot.conversation.queued.is_empty() {
                text.push_str(&format!(
                    "\n\nQUEUED · {} message(s), delivered before the next action",
                    snapshot.conversation.queued.len()
                ));
            }
        }
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
                for (index, review) in task.reviews.iter().enumerate() {
                    text.push_str(&format!("REVIEW {}\n{}\n", index + 1, review.reason));
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
    visible(&text)
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
                " {}  Conversation   Activity   Diff   Knowledge   Journal · Tab switches",
                ["●", "◉", "◇", "◆", "≡"][view.tab]
            )),
        ]),
        header,
    );
    let body = wrapped(&body(snapshot, view), main.width.saturating_sub(2) as usize);
    let line_count = body.lines().count();
    let paragraph = Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(
        [
            " Conversation ",
            " Activity ",
            " Diff ",
            " Knowledge ",
            " Journal ",
        ][view.tab],
    ));
    if view.follow {
        view.scroll = line_count
            .saturating_sub(main.height.saturating_sub(2) as usize)
            .min(u16::MAX as usize) as u16;
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
    frame.render_widget(Paragraph::new(format!("{spinner} {}\nEnter send · Alt-Enter newline · Esc interrupt · /help · PageUp/PageDown scroll",visible(notice))).style(Style::default().fg(if snapshot.busy {Color::Yellow} else {Color::Cyan})),status);
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
        KeyCode::Left => view.composer.left(),
        KeyCode::Right => view.composer.right(),
        KeyCode::Home => view.composer.home(),
        KeyCode::End => view.composer.end(),
        KeyCode::PageUp => {
            view.follow = false;
            view.scroll = view.scroll.saturating_sub(12);
        }
        KeyCode::PageDown => {
            view.scroll = view.scroll.saturating_add(12);
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
            view.follow = false;
            view.scroll = view.scroll.saturating_sub(1);
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
            view.scroll = view.scroll.saturating_add(1)
        }
        KeyCode::Up => view.composer.vertical(false),
        KeyCode::Down => view.composer.vertical(true),
        KeyCode::Tab | KeyCode::BackTab => {
            let offset = if key.code == KeyCode::Tab { 1 } else { 4 };
            view.tab = (view.tab + offset) % 5;
            view.scroll = 0;
            view.follow = view.tab < 2;
        }
        _ => {}
    }
    None
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
                        dirty = true;
                        let command = match event::read()? {
                            Event::Key(event) => key(&mut view, event),
                            Event::Paste(text) => {
                                view.composer.insert(&text);
                                None
                            }
                            _ => None,
                        };
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

pub async fn interactive(
    root: PathBuf,
    daemon: Option<String>,
    daemon_exe: Option<PathBuf>,
    resume: Option<String>,
) -> Result<()> {
    let root = root.canonicalize()?;
    let (provider,config_error)=match ProviderSettings::load(&root) {Ok(provider)=>(provider,None),Err(error)=>(ProviderSettings::fallback(),Some(format!("Model configuration failed: {error:#}. Use /model <endpoint> <model> to configure this session.")))};
    let mut conversation = match resume {
        Some(id) => Conversation::load(&root, &id)?,
        None => Conversation::new(root.clone()),
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
    fn task_fixture(root: PathBuf) -> Task {
        serde_json::from_value(serde_json::json!({
            "id":uuid::Uuid::new_v4().to_string(), "root":root, "objective":"Explain the parser", "mode":"Plan", "phase":"Planning",
            "events":[], "last_response":"", "knowledge_revision":"v1", "read_files":[], "check_results":[], "model_requests":[],
            "schema":1,"snapshots":{},"capture_due":false,"final_capture":false,"after_review":"Planning","resume_phase":"Planning","steps":0,"capture_operations":[],"capture_cursor":0,"source":{}
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
        };
        let summary = journal_summary(&snapshot);
        assert!(summary.len() < 2_000);
        assert!(!summary.contains("FULL_PROMPT_PAYLOAD"));
        assert!(summary.contains("harness_action · response saved"));
        assert!(summary.contains(".moosedev/harness/tasks/"));
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
    fn enter_submits_alt_enter_adds_newline_and_escape_interrupts() {
        let mut view = View::default();
        view.composer.insert("hello");
        assert!(key(&mut view, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)).is_none());
        assert_eq!(view.composer.text, "hello\n");
        assert!(
            matches!(key(&mut view,KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE)),Some(Command::Input(text)) if text=="hello\n")
        );
        assert!(matches!(
            key(&mut view, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Command::Interrupt)
        ));
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

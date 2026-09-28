//! `moosedev code`: the coding harness, a client of the daemon-owned
//! project-memory workflow. Interactive by default; every other command is
//! headless, prints JSON and exits non-zero on error, for scripts and
//! pipelines.
use crate::harness::{
    crash,
    runner::{default_daemon_url, Runner},
    startup::ProviderSettings,
    tui::{self, Action},
};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

const HELP: &str = "moosedev code: the MOOSEDev coding harness
Usage: moosedev code [--project DIR] [--daemon URL] [--daemon-exe PATH] [--new] [COMMAND]

  interactive              Reopen the newest conversation with unfinished work,
                            or open a new one (the default); --new always opens a new one
  resume-session ID        Resume a saved conversation

  new OBJECTIVE            Create a task in Plan mode
  status ID                Print the durable task state
  step ID                  Advance one workflow step
  run ID                   Advance up to 32 steps, stopping at human gates
  approve ID               Approve the current plan and enter Auto
  approve-policy ID        Approve the pending policy-gated edit
  approve-permission ID    Approve the pending sandbox permission request
  deny-permission ID       Deny the pending sandbox permission request
  choose ID KEY            Answer the pending harness question with an option
  permissions ID           List active task-scoped permission grants
  revoke-permission ID GRANT
                            Revoke an active permission grant
  review ID accept|reject  Review pending knowledge proposals
  no-knowledge ID          Confirm no durable knowledge changed
  plan ID                  Return to planning
  cancel ID                Cancel while preserving task state
  resume ID                Resume an interrupted or cancelled task
  answer ID TEXT           Answer the runner's pending question or park
  rework ID TEXT           At the final review, send the work back with a note
  tui ID                   Open the interactive task interface

Headless commands print JSON and require a running daemon.
Interactive startup discovers the local model and connects or starts moosedev.
";

struct Args {
    root: PathBuf,
    daemon: Option<String>,
    daemon_exe: Option<PathBuf>,
    /// `--new`: open a fresh conversation instead of the last unfinished one.
    fresh: bool,
    command: String,
    arguments: Vec<String>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Args>> {
    let mut args = args.into_iter();
    let mut root = crate::project::project_root();
    let mut daemon = None;
    let mut daemon_exe = None;
    let mut fresh = false;
    loop {
        match args.next().as_deref() {
            None => {
                return Ok(Some(Args {
                    root,
                    daemon,
                    daemon_exe,
                    fresh,
                    command: "interactive".into(),
                    arguments: vec![],
                }))
            }
            Some("--help" | "-h") => return Ok(None),
            Some("--project") => {
                root = args
                    .next()
                    .context("--project requires a directory")?
                    .into()
            }
            Some("--daemon") => daemon = Some(args.next().context("--daemon requires a URL")?),
            Some("--daemon-exe") => {
                daemon_exe = Some(args.next().context("--daemon-exe requires a path")?.into())
            }
            Some("--new") => fresh = true,
            Some(command) if command.starts_with('-') => bail!("unknown option {command}"),
            Some(command) => {
                return Ok(Some(Args {
                    root,
                    daemon,
                    daemon_exe,
                    fresh,
                    command: command.into(),
                    arguments: args.collect(),
                }))
            }
        }
    }
}

fn action(command: &str, args: &[String]) -> Result<Action> {
    let expected = match command {
        "review" => 2,
        "answer" | "rework" => 2,
        "revoke-permission" => 2,
        "choose" => 2,
        _ => 1,
    };
    if matches!(command, "answer" | "rework") {
        anyhow::ensure!(args.len() >= expected, "{command} requires ID and text");
    } else {
        anyhow::ensure!(
            args.len() == expected,
            "{command} requires {expected} argument(s)"
        );
    }
    Ok(match command {
        "step" => Action::Step,
        "run" => Action::Run,
        "approve" => Action::Approve,
        "approve-policy" => Action::ApprovePolicy,
        "approve-permission" => Action::ApprovePermission,
        "deny-permission" => Action::DenyPermission,
        "permissions" => Action::Permissions,
        "revoke-permission" => Action::RevokePermission(args[1].clone()),
        "choose" => Action::Choose(args[1].clone()),
        "review" => match args[1].as_str() {
            "accept" => Action::Accept,
            "reject" => Action::Reject,
            _ => bail!("review expects accept or reject"),
        },
        "no-knowledge" => Action::NoKnowledge,
        "plan" => Action::Plan,
        "cancel" => Action::Cancel,
        "resume" => Action::Resume,
        "answer" => Action::Answer(args[1..].join(" ")),
        "rework" => Action::Rework(args[1..].join(" ")),
        _ => bail!("unknown command {command}; use --help"),
    })
}

/// Run `moosedev code` with the arguments after `code`. An error is printed
/// as `{"error": …}` and the process exits 1, as scripts expect; it is also
/// kept in the crash log, since an interactive session's terminal may be gone.
pub async fn main(args: Vec<String>) -> Result<()> {
    if let Err(error) = run(args).await {
        crash::log(&format!("exited with an error: {error:#}"));
        eprintln!("{}", serde_json::json!({"error": format!("{error:#}")}));
        std::process::exit(1);
    }
    Ok(())
}

async fn run(args: Vec<String>) -> Result<()> {
    let Some(args) = parse_args(args)? else {
        print!("{HELP}");
        return Ok(());
    };
    let root = args
        .root
        .canonicalize()
        .context("resolve project directory")?;
    let root = crate::project::project_root_from(&root)
        .unwrap_or(&root)
        .to_path_buf();
    // From here on a panic, a fatal error or a silent death leaves evidence
    // under .moosedev/harness (badciv task c83c10f8 died and left none).
    crash::install(&root, std::env::args().collect::<Vec<_>>().join(" "));
    let previous_sessions = crash::previous_sessions();
    if !is_interactive(&args.command) {
        for report in &previous_sessions {
            eprintln!("{report}");
        }
    }
    // Match the daemon's explicit environment configuration without changing cwd.
    load_dotenv_file(&root.join(".env"))?;
    // Every model command runs confined and none runs outside it: a sandbox
    // that cannot start is named here, once, with what to do, as plain text.
    if advances_tasks(&args.command) {
        if let Err(problem) = crate::harness::executor::sandbox_readiness().await {
            eprintln!("{problem}");
            std::process::exit(1);
        }
    }
    if matches!(args.command.as_str(), "interactive" | "resume-session") {
        let launch = if args.command == "resume-session" {
            anyhow::ensure!(
                args.arguments.len() == 1,
                "resume-session requires one conversation ID"
            );
            tui::Launch::Conversation(args.arguments[0].clone())
        } else {
            anyhow::ensure!(args.arguments.is_empty(), "interactive takes no arguments");
            if args.fresh {
                tui::Launch::New
            } else {
                tui::Launch::Last
            }
        };
        let _session = crash::begin_session();
        return tui::interactive(
            root,
            args.daemon,
            args.daemon_exe,
            launch,
            previous_sessions,
        )
        .await;
    }
    let daemon = args
        .daemon
        .map(Ok)
        .unwrap_or_else(|| default_daemon_url(&root))?;
    if args.command == "new" {
        let objective = args.arguments.join(" ");
        anyhow::ensure!(!objective.trim().is_empty(), "new requires an objective");
        let runner = Runner::create(root, daemon, objective).await?;
        println!("{}", serde_json::to_string_pretty(&runner.task)?);
        return Ok(());
    }
    let id = args
        .arguments
        .first()
        .context("command requires a task ID")?;
    // Validate the complete invocation before opening the task or performing work.
    let operation = match args.command.as_str() {
        "tui" | "status" => {
            anyhow::ensure!(
                args.arguments.len() == 1,
                "{} requires one task ID",
                args.command
            );
            None
        }
        command => Some(action(command, &args.arguments)?),
    };
    let mut runner = Runner::load(root.clone(), daemon, id)?;
    if args.command == "tui" {
        let _session = crash::begin_session();
        return tui::run(runner, previous_sessions).await;
    }
    let result = if let Some(operation) = operation {
        // The same moosedev.toml roles as the interactive session. A broken
        // file must not strand a task, so operations that ask no model proceed.
        match ProviderSettings::load(&root) {
            Ok(provider) => runner.configure_provider(&provider, None),
            Err(_)
                if matches!(
                    operation,
                    Action::Cancel
                        | Action::Permissions
                        | Action::DenyPermission
                        | Action::RevokePermission(_)
                ) => {}
            Err(error) => return Err(error.context("model configuration")),
        }
        // Dropping the operation interrupts generation/verification; cancellation
        // preserves its durable obligations rather than declaring success.
        let interrupted = tokio::select! {
            result = tui::execute(&mut runner, operation) => Some(result),
            _ = tokio::signal::ctrl_c() => None,
        };
        match interrupted {
            Some(result) => result,
            None => runner.cancel().await,
        }
    } else {
        Ok(())
    };
    if args.command == "permissions" {
        println!(
            "{}",
            serde_json::to_string_pretty(&runner.task.permission_grants)?
        );
    } else {
        println!("{}", serde_json::to_string_pretty(&runner.task)?);
    }
    result
}

/// Commands that open the full-screen interface, where a notice belongs in
/// the transcript rather than on stderr.
fn is_interactive(command: &str) -> bool {
    matches!(command, "interactive" | "resume-session" | "tui")
}

/// Commands that can run a model's commands or checks. Reading, listing,
/// cancelling and revoking must work even where the sandbox cannot start.
fn advances_tasks(command: &str) -> bool {
    !matches!(
        command,
        "status" | "permissions" | "cancel" | "deny-permission" | "revoke-permission"
    )
}

fn load_dotenv_file(path: &Path) -> Result<()> {
    match dotenvy::from_path(path) {
        Ok(()) => Ok(()),
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("load dotenv {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_commands_that_can_advance_a_task_need_the_sandbox() {
        for command in [
            "interactive",
            "resume-session",
            "run",
            "step",
            "approve",
            "answer",
            "rework",
            "resume",
            "tui",
        ] {
            assert!(advances_tasks(command), "{command}");
        }
        for command in [
            "status",
            "permissions",
            "cancel",
            "deny-permission",
            "revoke-permission",
        ] {
            assert!(!advances_tasks(command), "{command}");
        }
    }

    #[test]
    fn dotenv_is_optional_but_malformed_configuration_is_an_error() {
        let root =
            std::env::temp_dir().join(format!("moosedev-code-dotenv-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join(".env");
        load_dotenv_file(&path).unwrap();
        std::fs::write(&path, "# Empty configuration is valid\n").unwrap();
        load_dotenv_file(&path).unwrap();
        std::fs::write(&path, "=malformed\n").unwrap();
        let error = load_dotenv_file(&path).unwrap_err();
        assert!(error.to_string().contains("load dotenv"));
        assert!(error.to_string().contains(path.to_str().unwrap()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn human_commands_reject_ambiguous_arguments() {
        assert!(action("review", &["task".into(), "yes".into()]).is_err());
        assert!(action("approve", &["task".into(), "extra".into()]).is_err());
        assert!(action("deny-permission", &["task".into(), "extra".into()]).is_err());
        assert!(action("revoke-permission", &["task".into()]).is_err());
        assert!(matches!(
            action("revoke-permission", &["task".into(), "grant-1".into()]).unwrap(),
            Action::RevokePermission(id) if id == "grant-1"
        ));
        assert!(action("choose", &["task".into()]).is_err());
        assert!(matches!(
            action("choose", &["task".into(), "add".into()]).unwrap(),
            Action::Choose(key) if key == "add"
        ));
        assert!(action("no-knowledge", &[]).is_err());
        assert!(action("run", &["task".into()]).is_ok());
    }

    #[test]
    fn options_precede_objective_and_do_not_consume_it() {
        let args = parse_args(["--project", "/tmp", "new", "fix", "--help"].map(String::from))
            .unwrap()
            .unwrap();
        assert_eq!(args.root, PathBuf::from("/tmp"));
        assert_eq!(args.arguments, ["fix", "--help"]);
        assert!(parse_args(["--daemon".into()]).is_err());
    }

    #[test]
    fn default_is_conversation_and_help_remains_explicit() {
        let default = parse_args([]).unwrap().unwrap();
        assert_eq!(default.command, "interactive");
        assert!(!default.fresh, "a bare start reopens unfinished work");
        assert!(parse_args(["--new".into()]).unwrap().unwrap().fresh);
        assert!(parse_args(["--help".into()]).unwrap().is_none());
        let args = parse_args(
            ["--daemon-exe", "/tmp/moosedev", "resume-session", "session"].map(String::from),
        )
        .unwrap()
        .unwrap();
        assert_eq!(args.daemon_exe, Some(PathBuf::from("/tmp/moosedev")));
        assert_eq!(args.command, "resume-session");
        assert_eq!(args.arguments, ["session"]);
        assert!(action("resume", &["task".into()]).is_ok());
    }
}

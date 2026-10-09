//! Look requests: a command while planning is answered by the harness
//! (simL1: qwen, offered only literal search in Plan mode, searched 250+
//! times a prompt; OpenCode's plan agent left planning with `ls`, globs and
//! a build).
use super::*;

fn journaled(runner: &Runner, prefix: &str) -> Vec<String> {
    runner
        .task
        .events
        .iter()
        .filter(|event| event.message.starts_with(prefix))
        .map(|event| event.message.clone())
        .collect()
}

async fn look(fixture: &Fixture, runner: &mut Runner, command: &str) -> String {
    fixture.conversational(json!({"action":"command","command":command}));
    runner.advance().await.unwrap();
    assert_ne!(
        runner.task.phase,
        Phase::AwaitingInput,
        "{command}: {}",
        runner.task.last_response
    );
    runner.task.last_response.clone()
}

#[tokio::test]
async fn a_planning_command_is_a_look_the_harness_answers() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    std::fs::create_dir_all(fixture.root.join("src")).unwrap();
    std::fs::write(
        fixture.root.join("src/lib.rs"),
        "fn a() {}\nfn b() {}\nfn c() {}\n",
    )
    .unwrap();
    let mut runner = fixture.interactive().await;
    assert_eq!(runner.task.mode, Mode::Plan);

    let listing = look(&fixture, &mut runner, "ls -la").await;
    assert!(
        listing.contains("answered by the harness's file list")
            && listing.contains("code.txt")
            && listing.contains("src/"),
        "{listing}"
    );
    let found = look(&fixture, &mut runner, "grep -rn original .").await;
    assert!(found.contains("code.txt:1:original"), "{found}");
    let slice = look(&fixture, &mut runner, "sed -n '2,3p' src/lib.rs").await;
    assert!(
        slice.contains("src/lib.rs, lines 2-3 of 3:\n    2  fn b() {}"),
        "{slice}"
    );
    // A whole-file view is the read itself.
    look(&fixture, &mut runner, "cat src/lib.rs").await;
    assert!(runner.task.read_files.contains(&"src/lib.rs".to_string()));
    assert_eq!(
        journaled(&runner, "Look request: cat src/lib.rs"),
        vec!["Look request: cat src/lib.rs\nAnswered by: a read of src/lib.rs"]
    );
    // A look that would change something is declined, with the way forward.
    let declined = look(&fixture, &mut runner, "rm code.txt").await;
    assert!(
        declined.contains("Declined: `rm` changes files")
            && declined.contains("put this in the plan"),
        "{declined}"
    );
    assert!(fixture.root.join("code.txt").exists());
    let outside = look(&fixture, &mut runner, "cat /etc/hosts").await;
    assert!(outside.contains("outside the project"), "{outside}");
    // Read-only git runs host-side; the fixture is no repository, so git
    // says so, and that is the answer.
    let git = look(&fixture, &mut runner, "git log --oneline -3").await;
    assert!(git.contains("answered by read-only git"), "{git}");
    // Nothing planned, approved, gated or run as a check.
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(runner.task.plan.is_none());
    assert!(runner.task.permission_grants.is_empty());
    assert!(journaled(&runner, "Command: ").is_empty());
    assert_eq!(intent_details(&runner, "look_answered").len(), 7);
}

#[tokio::test]
async fn a_look_runs_in_the_sandbox_when_the_harness_cannot_answer_it_itself() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    let answer = look(&fixture, &mut runner, "echo look-ok | head -1").await;
    assert!(
        answer.contains("answered by the read-only sandbox") && answer.contains("look-ok"),
        "{answer}"
    );
    assert!(journaled(&runner, "Command: ").is_empty());
}

#[tokio::test]
async fn a_look_asked_again_is_served_once_then_recovered() {
    let _env_lock = ENVIRONMENT.lock().await;
    let _recover_off = RecoverOff::set();
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    look(&fixture, &mut runner, "ls").await;
    let again = look(&fixture, &mut runner, "ls").await;
    assert!(
        again.starts_with("You asked this look at event") && again.contains("code.txt"),
        "{again}"
    );
    // A look between them is looking, not progress.
    look(&fixture, &mut runner, "grep -rn original .").await;
    fixture.conversational(json!({"action":"command","command":"ls"}));
    runner.advance().await.unwrap();
    assert_eq!(runner.task.phase, Phase::AwaitingInput);
    assert!(
        runner
            .task
            .last_response
            .starts_with("The model keeps asking looks it was already answered"),
        "{}",
        runner.task.last_response
    );
}

#[tokio::test]
async fn with_plan_looks_off_planning_offers_no_command() {
    let _env_lock = ENVIRONMENT.lock().await;
    let _looks_off = PlanLooksOff::set();
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    fixture.conversational(json!({"action":"search","query":"original"}));
    runner.advance().await.unwrap();
    let request = requests_of_kind(&fixture, "model")
        .into_iter()
        .find(|request| request["schema"] == "harness_action")
        .unwrap();
    let names: Vec<&str> = request["body"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"command"), "{names:?}");
    let prompt = request["body"]["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("Allowed actions now: read, search, inspect, question, reply, plan."));
}

#[tokio::test]
async fn a_look_the_harness_cannot_show_is_answered_not_raised() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = Fixture::new().await;
    std::fs::create_dir_all(fixture.root.join("src")).unwrap();
    std::fs::write(fixture.root.join("src/lib.rs"), "fn a() {}\n").unwrap();
    std::fs::write(fixture.root.join(".env"), "API_KEY=secret-value\n").unwrap();
    let mut runner = fixture.interactive().await;
    let directory = look(&fixture, &mut runner, "cat src/").await;
    assert!(
        directory.contains("Listing of `src`") && directory.contains("lib.rs"),
        "{directory}"
    );
    let protected = look(&fixture, &mut runner, "head -n 5 .env").await;
    assert!(
        protected.contains("Declined: `.env` cannot be shown")
            && !protected.contains("secret-value"),
        "{protected}"
    );
    let found = look(&fixture, &mut runner, "grep -rn API_KEY .").await;
    assert!(!found.contains("secret-value"), "{found}");
    let climbing = look(&fixture, &mut runner, "cat ../outside").await;
    assert!(climbing.contains("outside the project"), "{climbing}");
}

#[tokio::test]
async fn a_repeated_whole_file_view_is_judged_as_a_read() {
    let _env_lock = ENVIRONMENT.lock().await;
    let _recover_off = RecoverOff::set();
    let fixture = Fixture::new().await;
    let mut runner = fixture.interactive().await;
    look(&fixture, &mut runner, "cat code.txt").await;
    fixture.conversational(json!({"action":"command","command":"cat code.txt"}));
    runner.advance().await.unwrap();
    assert!(
        !runner.task.last_response.starts_with("You asked this look"),
        "{}",
        runner.task.last_response
    );
    assert!(intent_details(&runner, "empty_look").is_empty());
}

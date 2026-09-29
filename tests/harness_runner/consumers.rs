//! Small consumers of the step's state (context plan item 7): a file an
//! earlier approved plan of the task listed joins the plan without asking
//! (`MOOSEDEV_HARNESS_SCOPE_AUTO_ADD`), and a model replan while every
//! current error is in the approved files is held once
//! (`MOOSEDEV_HARNESS_REPLAN_HOLD`).
use super::mock::*;
use moosedev::harness::protocol::GoverningRule;
use moosedev::harness::runner::{ChoiceKind, DiagnosticsSnapshot, Finding, Mode, Phase, Runner};
use serde_json::{json, Value};

/// A harness switch set to `off` for the life of the guard. Hold
/// [`ENVIRONMENT`] with it.
struct SwitchOff(&'static str);

impl SwitchOff {
    fn new(name: &'static str) -> Self {
        std::env::set_var(name, "off");
        Self(name)
    }
}

impl Drop for SwitchOff {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

/// Answer the scripted action, advancing through whatever the harness does
/// by itself first.
async fn step(fixture: &Fixture, runner: &mut Runner, action: Value) {
    fixture.conversational(action);
    for _ in 0..4 {
        if fixture.shared.lock().unwrap().replies.is_empty() {
            return;
        }
        runner.advance().await.unwrap();
    }
    panic!("the scripted action was never requested");
}

/// Plan from Planning until the plan awaits approval, confirming a review
/// the replan's checkpoint may raise.
async fn propose(fixture: &Fixture, runner: &mut Runner, plan: Value) {
    fixture.conversational(plan);
    for _ in 0..4 {
        if runner.task.phase == Phase::AwaitingPlan {
            return;
        }
        match runner.task.phase {
            Phase::AwaitingReview => runner.confirm_no_knowledge().await.unwrap(),
            _ => runner.advance().await.unwrap(),
        }
    }
    assert_eq!(
        runner.task.phase,
        Phase::AwaitingPlan,
        "{}",
        runner.task.last_response
    );
}

/// A first approved plan over labels.py and other.py, an edit under it, then
/// a replan narrowed to labels.py and approved.
async fn narrowed_plan(fixture: &Fixture) -> Runner {
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = fixture.interactive().await;
    step(
        fixture,
        &mut runner,
        json!({"action":"read","file":"labels.py"}),
    )
    .await;
    propose(fixture, &mut runner, json!({"action":"plan","summary":"Add a helper and update the constant","files":["labels.py","other.py"],"checks":["true"]})).await;
    runner.approve_plan().await.unwrap();
    add_helper(fixture);
    runner.advance().await.unwrap();
    runner.advance().await.unwrap();
    assert_eq!(runner.task.edits.len(), 1);
    step(
        fixture,
        &mut runner,
        json!({"action":"replan","reason":"The constant can stay"}),
    )
    .await;
    assert_eq!(runner.task.mode, Mode::Plan);
    propose(fixture, &mut runner, json!({"action":"plan","summary":"Add the helper only","files":["labels.py"],"checks":["true"]})).await;
    runner.approve_plan().await.unwrap();
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["labels.py"]);
    assert_eq!(runner.task.approved_plans.len(), 2);
    runner
}

const EDIT_OTHER: &str =
    r#"{"action":"replace","file":"other.py","old_text":"x = 1","new_text":"x = 2"}"#;

fn edit_other() -> Value {
    serde_json::from_str(EDIT_OTHER).unwrap()
}

#[tokio::test]
async fn a_file_an_earlier_approved_plan_listed_joins_the_plan_without_asking() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = narrowed_plan(&fixture).await;
    let approved = journal_value(&runner)["approved_revision"].clone();
    step(&fixture, &mut runner, edit_other()).await;
    assert!(runner.task.pending_choice.is_none());
    assert!(intent_details(&runner, "choice_asked").is_empty());
    assert_eq!(intent_details(&runner, "scope_auto_added"), ["other.py"]);
    assert!(intent_details(&runner, "scope_escape_replan").is_empty());
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);
    assert_eq!(journal_value(&runner)["approved_revision"], approved);
    assert_eq!(
        runner.task.plan.as_ref().unwrap().files,
        ["labels.py", "other.py"]
    );
    assert_eq!(
        runner.task.approved_plans.last().unwrap().files,
        ["labels.py", "other.py"]
    );
    assert!(runner
        .task
        .events
        .iter()
        .any(|event| event.message
            == "Harness added other.py to the approved plan: an earlier approved plan of this task listed it."));
    // The edit goes on in the same step, as any edit to a planned file.
    assert_eq!(runner.task.edits.len(), 2);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("other.py")).unwrap(),
        "x = 2\n"
    );
}

#[tokio::test]
async fn an_earlier_planned_file_bringing_unaddressed_rules_is_still_asked_about() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = narrowed_plan(&fixture).await;
    fixture.shared.lock().unwrap().file_rules = vec![(
        "other.py".into(),
        GoverningRule {
            iri: "urn:rule:frozen-constants".into(),
            label: "Module constants are frozen".into(),
            kind: "Constraint".into(),
            claim: "hasDescription: A module-level constant is never reassigned.\n".into(),
            via: "via: linked to other.py".into(),
            decided_by: Vec::new(),
        },
    )];
    step(&fixture, &mut runner, edit_other()).await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert_eq!(
        runner.task.pending_choice.as_ref().unwrap().kind,
        ChoiceKind::ScopeAdd {
            file: "other.py".into()
        }
    );
    assert!(intent_details(&runner, "scope_auto_added").is_empty());
    assert_eq!(
        intent_details(&runner, "scope_auto_add_refused"),
        ["other.py: Module constants are frozen"]
    );
    // The amendment is undone.
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["labels.py"]);
    assert_eq!(
        runner.task.approved_plans.last().unwrap().files,
        ["labels.py"]
    );
}

/// Off, the file is asked about as before; the human's refuse keeps it out
/// for the task, so switched on again it is still asked about.
#[tokio::test]
async fn switched_off_or_declined_an_earlier_planned_file_is_asked_about() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = narrowed_plan(&fixture).await;
    {
        let _off = SwitchOff::new("MOOSEDEV_HARNESS_SCOPE_AUTO_ADD");
        step(&fixture, &mut runner, edit_other()).await;
    }
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert!(intent_details(&runner, "scope_auto_added").is_empty());
    assert!(intent_details(&runner, "scope_auto_add_refused").is_empty());
    runner.choose("refuse").await.unwrap();
    assert!(runner
        .task
        .symbolic
        .as_ref()
        .unwrap()
        .scope_declined
        .contains("other.py"));
    assert_eq!(
        journal_value(&runner)["symbolic"]["scope_declined"],
        json!(["other.py"])
    );

    step(&fixture, &mut runner, edit_other()).await;
    assert_eq!(runner.task.phase, Phase::AwaitingChoice);
    assert!(intent_details(&runner, "scope_auto_added").is_empty());
    assert_eq!(
        intent_details(&runner, "choice_asked"),
        [
            "scope_add other.py: add, replan, refuse (default add)",
            "scope_add other.py: add, replan, refuse (default add)"
        ]
    );
    assert_eq!(runner.task.plan.as_ref().unwrap().files, ["labels.py"]);
}

/// A failed run whose compiler errors point at `file`.
fn failing_at(file: &str) -> Value {
    json!({"action":"command","command":format!("printf 'error[E0308]: mismatched types\\n  --> {file}:1:1\\n'; exit 1")})
}

/// The approved labels.py plan, other.py on disk outside it.
async fn approved(fixture: &Fixture) -> Runner {
    std::fs::write(fixture.root.join("other.py"), "x = 1\n").unwrap();
    let mut runner = planned_symbolic_runner(fixture).await;
    runner.approve_plan().await.unwrap();
    runner
}

fn replan() -> Value {
    json!({"action":"replan","reason":"The helper needs its own module"})
}

#[tokio::test]
async fn a_replan_while_every_error_is_planned_is_held_once() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = approved(&fixture).await;
    step(&fixture, &mut runner, failing_at("labels.py")).await;
    let diagnostics = DiagnosticsSnapshot {
        servers: vec!["pyright".into()],
        settled: true,
        errors: vec![Finding {
            file: "labels.py".into(),
            line: 2,
            column: 12,
            message: "\"normalize\" is not defined".into(),
            detail: None,
            definition: None,
            declared: Vec::new(),
            fixes: vec![],
            fixes_complete: false,
        }],
        warnings: vec![],
        lints: vec![],
        linter: None,
        finish_refused: false,
    };
    runner.task.diagnostics = Some(diagnostics);
    step(&fixture, &mut runner, replan()).await;
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(runner.task.phase, Phase::Working);
    assert!(intent_details(&runner, "model_replan").is_empty());
    assert_eq!(
        intent_details(&runner, "replan_held"),
        ["labels.py: The helper needs its own module"]
    );
    assert_eq!(
        runner.task.symbolic.as_ref().unwrap().replan_held_at,
        Some(0)
    );
    let held = runner.task.last_response.clone();
    assert!(
        held.starts_with(
            "Replan held once: every current error is in the approved files (labels.py). Errors:\nLanguage server (pyright) after your last edit: 1 error(s)"
        ),
        "{held}"
    );
    assert!(held.contains("\"normalize\" is not defined"), "{held}");
    assert!(
        held.contains(
            "Latest failed run:\nlabels.py:\n  error[E0308]: mismatched types\n  --> labels.py:1:1\n"
        ),
        "{held}"
    );
    assert!(
        held.ends_with("\nFix them within the plan; replan again if the plan itself is wrong."),
        "{held}"
    );

    // The second replan at the same edit count goes through.
    step(&fixture, &mut runner, replan()).await;
    assert_eq!(runner.task.mode, Mode::Plan);
    assert_eq!(
        intent_details(&runner, "model_replan"),
        ["The helper needs its own module"]
    );
    assert_eq!(intent_details(&runner, "replan_held").len(), 1);
}

/// A path the failed output names outside its error lines (here a test
/// runner's progress line naming other.py) does not defeat the hold: the
/// error files are read from the error lines and their locations.
#[tokio::test]
async fn a_path_named_outside_the_error_lines_does_not_defeat_the_hold() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = approved(&fixture).await;
    let command = "printf 'collecting other.py\\nerror[E0308]: mismatched types\\n  --> labels.py:1:1\\n'; exit 1";
    step(
        &fixture,
        &mut runner,
        json!({"action":"command","command":command}),
    )
    .await;
    step(&fixture, &mut runner, replan()).await;
    assert_eq!(runner.task.mode, Mode::Auto);
    assert_eq!(
        intent_details(&runner, "replan_held"),
        ["labels.py: The helper needs its own module"]
    );
}

#[tokio::test]
async fn a_replan_is_not_held_with_an_error_outside_the_plan_or_no_error() {
    let _env_lock = ENVIRONMENT.lock().await;
    let fixture = symbolic_fixture().await;
    let mut runner = approved(&fixture).await;
    step(&fixture, &mut runner, failing_at("other.py")).await;
    step(&fixture, &mut runner, replan()).await;
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(intent_details(&runner, "replan_held").is_empty());

    let fixture = symbolic_fixture().await;
    let mut runner = approved(&fixture).await;
    step(
        &fixture,
        &mut runner,
        json!({"action":"command","command":"true"}),
    )
    .await;
    step(&fixture, &mut runner, replan()).await;
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(intent_details(&runner, "replan_held").is_empty());
}

#[tokio::test]
async fn replan_hold_off_lets_the_replan_through() {
    let _env_lock = ENVIRONMENT.lock().await;
    let _off = SwitchOff::new("MOOSEDEV_HARNESS_REPLAN_HOLD");
    let fixture = symbolic_fixture().await;
    let mut runner = approved(&fixture).await;
    step(&fixture, &mut runner, failing_at("labels.py")).await;
    step(&fixture, &mut runner, replan()).await;
    assert_eq!(runner.task.mode, Mode::Plan);
    assert!(intent_details(&runner, "replan_held").is_empty());
    assert_eq!(
        intent_details(&runner, "model_replan"),
        ["The helper needs its own module"]
    );
}

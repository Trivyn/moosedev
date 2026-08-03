//! Calibration probe for `align_concepts` — measures resolution rate and
//! false-commit rate over a labeled set of software-architecture terms, instead
//! of tuning thresholds from anecdotal failures (Lesson 23663f6e).
//!
//! Runs against a COPY of the real project store (separate RocksDB lock, so it
//! coexists with a live `moosedev serve`). ZERO writes to the live store.
//!
//!   cp -r .moosedev /tmp/moosedev-align-probe && rm -f /tmp/moosedev-align-probe/*.sock
//!   cargo run --release --example alignment_probe -- /tmp/moosedev-align-probe ontologies
//!
//! Per-ranker detail (which ranker had the gold class, and at what rank) comes
//! from MOOSE's own instrumentation — no reimplementation of the ranking math:
//!
//!   RUST_LOG=moose::alignment=debug \
//!   MOOSE_DEBUG_TRACE_IRI="$(…comma-separated class IRIs…)" \
//!   cargo run --release --example alignment_probe -- …

use std::path::Path;

use moose::alignment::AlignmentOutcome;
use moosedev::alignment::align_concept;
use moosedev::graph::AppState;

/// One labeled probe case. `gold` lists every ontology class local-name that a
/// reviewer would accept; empty means "no class in this ontology fits — the
/// honest outcome is abstention, and any commit is a FALSE COMMIT".
struct Case {
    label: &'static str,
    definition: &'static str,
    gold: &'static [&'static str],
}

/// Positives are real software-architecture terms a developer might introduce.
/// Cases 1-5 paraphrase the ontology's own `skos:definition` closely (easy — they
/// isolate whether the definition channel is consulted at all); 6-12 are novel
/// domain terms whose label shares no token with the gold class label (realistic).
/// The negatives are out-of-domain: nothing in an 11-class architecture ontology
/// covers them, so abstention is the correct answer.
const CASES: &[Case] = &[
    // ── near-verbatim positives (definition channel should nail these) ──
    Case {
        label: "Retrospective Insight",
        definition: "A generalizable insight learned in hindsight.",
        gold: &["Lesson"],
    },
    Case {
        label: "Technology Choice",
        definition: "A recorded choice about a system's structure or behavior and its reasoning.",
        gold: &["ArchitecturalDecision"],
    },
    Case {
        label: "Rejected Option",
        definition: "A different possible choice considered during the decision process.",
        gold: &["Alternative"],
    },
    Case {
        label: "Design Justification",
        definition: "The reasoning behind an architectural decision.",
        gold: &["Rationale"],
    },
    Case {
        label: "Accepted Trade-off",
        definition: "The outcome of a decision, including accepted trade-offs.",
        gold: &["Consequence"],
    },
    // ── realistic novel positives (label shares no token with gold label) ──
    Case {
        label: "Circuit Breaker",
        definition: "A design pattern that stops calls to a failing downstream service after a \
                     failure threshold, to prevent cascading failure.",
        gold: &["Pattern"],
    },
    Case {
        label: "Sidecar",
        definition: "A reusable structure in which a helper process runs alongside the main \
                     service to provide cross-cutting behavior.",
        gold: &["Pattern"],
    },
    Case {
        label: "God Object",
        definition: "A recurring problematic structure where one class knows or does too much.",
        gold: &["AntiPattern"],
    },
    Case {
        label: "Big Ball of Mud",
        definition: "A recurring problematic architecture with no discernible structure.",
        gold: &["AntiPattern"],
    },
    Case {
        label: "Message Broker",
        definition: "An element of the software system that routes messages between services, \
                     which decisions and constraints act upon.",
        gold: &["SystemComponent"],
    },
    Case {
        label: "Rate Limit",
        definition: "A hard restriction on requests per interval that the design must satisfy.",
        gold: &["Constraint"],
    },
    Case {
        label: "Service Level Objective",
        definition: "A target reliability level that motivates and drives the design.",
        // Defensibly either a driver (Requirement) or a hard limit (Constraint).
        gold: &["Requirement", "Constraint"],
    },
    // ── hard negatives (nothing in this ontology fits; abstain is correct) ──
    Case {
        label: "Sprint Velocity",
        definition: "The average number of story points a team completes per sprint.",
        gold: &[],
    },
    Case {
        label: "Quarterly Budget",
        definition: "The money allocated to a department for a three-month period.",
        gold: &[],
    },
    Case {
        label: "Onboarding Checklist",
        definition: "A list of steps for a new hire to complete during their first week.",
        gold: &[],
    },
];

fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

/// Outcome of one case, collapsed to the three numbers that matter.
enum Verdict {
    CorrectCommit,
    FalseCommit,
    Abstained,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut args = std::env::args().skip(1);
    let data_dir = args
        .next()
        .unwrap_or_else(|| "/tmp/moosedev-align-probe".to_string());
    let onto_dir = args.next().unwrap_or_else(|| "ontologies".to_string());

    let mut state = AppState::bootstrap(Path::new(&data_dir), Path::new(&onto_dir))?;
    state.build_alignment_index().await?;

    println!(
        "== align_concepts calibration probe ({} cases over {} ontology classes) ==\n",
        CASES.len(),
        state.arch_vocab.classes.len()
    );

    let mut correct = 0usize;
    let mut false_commits = 0usize;
    let mut abstained_on_positive = 0usize;
    let positives = CASES.iter().filter(|c| !c.gold.is_empty()).count();

    for case in CASES {
        let outcome = align_concept(
            &state,
            case.label,
            Some(case.definition),
            Vec::new(),
        )
        .await?;

        let (verdict, detail) = match &outcome {
            AlignmentOutcome::Resolved { iri, sensor, .. } => {
                let got = local_name(iri);
                let ok = case.gold.contains(&got);
                (
                    if ok {
                        Verdict::CorrectCommit
                    } else {
                        Verdict::FalseCommit
                    },
                    format!("RESOLVED → {got} (sensor {sensor:?})"),
                )
            }
            AlignmentOutcome::Undecided {
                reason,
                top_candidates,
                ..
            } => {
                let ranked: Vec<&str> = top_candidates
                    .iter()
                    .map(|c| local_name(&c.iri))
                    .collect();
                // Where did the gold class land in the surfaced candidate list?
                let gold_rank = case
                    .gold
                    .iter()
                    .filter_map(|g| ranked.iter().position(|r| r == g))
                    .min()
                    .map(|i| format!("gold at candidate #{}", i + 1))
                    .unwrap_or_else(|| "gold ABSENT from candidates".into());
                (
                    Verdict::Abstained,
                    format!("undecided [{}] — {gold_rank}\n      {reason}", ranked.join(", ")),
                )
            }
            other => (Verdict::Abstained, format!("{other:?}")),
        };

        let (mark, gold_str) = match (&verdict, case.gold.is_empty()) {
            (Verdict::CorrectCommit, _) => {
                correct += 1;
                ("✓", case.gold.join("|"))
            }
            (Verdict::FalseCommit, true) => {
                false_commits += 1;
                ("✗ FALSE COMMIT", "(none — should abstain)".into())
            }
            (Verdict::FalseCommit, false) => {
                false_commits += 1;
                ("✗ WRONG CLASS", case.gold.join("|"))
            }
            (Verdict::Abstained, true) => ("✓ (correct abstention)", "(none)".into()),
            (Verdict::Abstained, false) => {
                abstained_on_positive += 1;
                ("· missed", case.gold.join("|"))
            }
        };

        println!("  {mark}  \"{}\"  gold={gold_str}", case.label);
        println!("      {detail}\n");
    }

    println!("== summary ==");
    println!(
        "  positives:            {positives}  (resolved correctly: {correct}, missed: {abstained_on_positive})"
    );
    println!(
        "  negatives:            {}  (false commits: {false_commits})",
        CASES.len() - positives
    );
    println!(
        "  resolution rate on positives: {:.0}%",
        100.0 * correct as f64 / positives as f64
    );
    Ok(())
}

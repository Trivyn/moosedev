// Acceptance harness for the v2 substrate resolver.
//
// Regenerate the substrate with:
//   cargo run --no-default-features -- index
//
// If fixture line numbers drift, repair `tests/fixtures/resolver_sample.json` by
// re-verifying each changed position with:
//   cargo run --no-default-features -- resolve <path> <line>:<col>
//
// Manual rename-continuity gate:
//   1. Resolve and record the symbol for an entity that will not be renamed.
//   2. Make a scratch commit that renames one private function.
//   3. Re-run `moosedev index`.
//   4. Resolve the unrenamed entity again; its symbol string must be byte-identical.
//   5. Reset back to the original commit and re-run `moosedev index`.

use std::path::Path;

use moosedev::code::substrate::{Position, Substrate};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Sample {
    path: String,
    line: u32,
    col: u32,
    expect_symbol: String,
    expect_definition: bool,
    note: String,
}

#[test]
fn acceptance_40_positions() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let data_dir = repo_root.join(".moosedev");
    let meta_path = data_dir.join("substrate").join("meta.json");
    if !meta_path.is_file() {
        eprintln!(
            "skipping substrate resolver acceptance: {} is absent; run `moosedev index`",
            meta_path.display()
        );
        return Ok(());
    }

    let substrate = Substrate::load(&data_dir, repo_root)?;
    let samples: Vec<Sample> = serde_json::from_str(include_str!("fixtures/resolver_sample.json"))?;
    assert_eq!(
        samples.len(),
        40,
        "resolver acceptance fixture must hold 40 positions"
    );

    println!(
        "{:<45} {:<52} {:<8} got",
        "position", "expected suffix", "result"
    );
    println!("{}", "-".repeat(130));

    let mut passed = 0usize;
    for sample in &samples {
        assert!(sample.line > 0, "fixture lines are 1-based");
        assert!(sample.col > 0, "fixture columns are 1-based");

        let got = substrate.resolve(
            &sample.path,
            Position {
                line: sample.line - 1,
                col: sample.col - 1,
            },
        );
        let ok = got.as_ref().is_some_and(|resolution| {
            resolution.symbol.ends_with(&sample.expect_symbol)
                && resolution.is_definition == sample.expect_definition
        });
        if ok {
            passed += 1;
        }

        let expected_role = role_label(sample.expect_definition);
        let got_text = got
            .as_ref()
            .map(|resolution| {
                let role = role_label(resolution.is_definition);
                format!("{} ({role})", resolution.symbol)
            })
            .unwrap_or_else(|| "MISS".to_string());
        println!(
            "{:<45} {:<52} {:<8} {}",
            format!("{}:{}:{}", sample.path, sample.line, sample.col),
            format!("{} ({expected_role})", sample.expect_symbol),
            if ok { "PASS" } else { "FAIL" },
            got_text
        );
        if !ok {
            println!("  note: {}", sample.note);
        }
    }

    assert!(
        passed >= 38,
        "resolver acceptance passed {passed}/{} positions; expected at least 38",
        samples.len()
    );
    Ok(())
}

fn role_label(is_definition: bool) -> &'static str {
    if is_definition {
        "def"
    } else {
        "ref"
    }
}

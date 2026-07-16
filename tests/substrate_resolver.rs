// Acceptance harness for the v2 substrate resolver.
//
// Regenerate the substrate with:
//   cargo run --no-default-features -- index
//
// If source text changes, repair the affected fixture anchor (rather than
// renumbering a position) and re-verify it with:
//   cargo run --no-default-features -- resolve <path> <line>:<col>
//
// Manual rename-continuity gate:
//   1. Resolve and record the symbol for an entity that will not be renamed.
//   2. Make a scratch commit that renames one private function.
//   3. Re-run `moosedev index`.
//   4. Resolve the unrenamed entity again; its symbol string must be byte-identical.
//   5. Reset back to the original commit and re-run `moosedev index`.

use std::path::Path;

use moosedev::code::substrate::{Position, ResolutionMode, Substrate, SubstrateMeta};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Sample {
    path: String,
    anchor: String,
    token: String,
    occurrence: Option<usize>,
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
        let (line, col) = locate_position(repo_root, sample);

        let got = substrate.resolve(
            &sample.path,
            Position {
                line: line - 1,
                col: col - 1,
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
            format!("{}:{line}:{col}", sample.path),
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

#[test]
fn acceptance_tree_sitter_fallback_positions() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let data_dir = repo_root.join(".moosedev");
    let meta_path = data_dir.join("substrate").join("meta.json");
    if !meta_path.is_file() {
        eprintln!(
            "skipping tree-sitter resolver acceptance: {} is absent; run `moosedev index`",
            meta_path.display()
        );
        return Ok(());
    }
    let substrate = Substrate::load(&data_dir, repo_root)?;
    let samples: Vec<Sample> =
        serde_json::from_str(include_str!("fixtures/resolver_sample_ts.json"))?;
    assert_eq!(samples.len(), 10);

    for sample in &samples {
        let (line, col) = locate_position(repo_root, sample);
        let resolution = substrate
            .resolve(
                &sample.path,
                Position {
                    line: line - 1,
                    col: col - 1,
                },
            )
            .unwrap_or_else(|| panic!("tree-sitter miss: {}", sample.note));
        assert!(
            resolution.symbol.ends_with(&sample.expect_symbol),
            "{}: got {}",
            sample.note,
            resolution.symbol
        );
        assert_eq!(resolution.mode, ResolutionMode::TreeSitter);
        assert_eq!(resolution.is_definition, sample.expect_definition);
    }
    Ok(())
}

#[test]
fn repository_dual_producer_substrate_loads() -> anyhow::Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let data_dir = repo_root.join(".moosedev");
    let meta_path = data_dir.join("substrate").join("meta.json");
    if !meta_path.is_file() {
        eprintln!(
            "skipping dual-producer substrate acceptance: {} is absent; run the real dual-producer index at review time",
            meta_path.display()
        );
        return Ok(());
    }
    let meta = SubstrateMeta::load(&data_dir)?;
    if !meta
        .producers
        .iter()
        .any(|producer| producer.name == "scip-typescript")
    {
        eprintln!(
            "skipping dual-producer substrate acceptance: meta.json does not list scip-typescript; run the real dual-producer index at review time"
        );
        return Ok(());
    }

    let definitions = Substrate::load(&data_dir, repo_root)?.definitions();
    assert!(definitions.iter().any(|entry| {
        entry.producer == "rust-analyzer" && entry.symbol.contains("runtime/build_server().")
    }));
    assert!(definitions
        .iter()
        .any(|entry| { entry.producer == "scip-typescript" && entry.file.starts_with("ui/") }));
    Ok(())
}

fn locate_position(repo_root: &Path, sample: &Sample) -> (u32, u32) {
    let path = repo_root.join(&sample.path);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read resolver sample {}: {error}", path.display()));
    let matches = source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(&sample.anchor))
        .collect::<Vec<_>>();
    assert!(
        !matches.is_empty(),
        "anchor not found for {}: {:?}",
        sample.path,
        sample.anchor
    );

    let occurrence = sample.occurrence.unwrap_or(1);
    assert!(
        occurrence > 0 && occurrence <= matches.len(),
        "anchor occurrence {occurrence} out of range for {}: {:?} ({} match(es))",
        sample.path,
        sample.anchor,
        matches.len()
    );
    let (line_index, line) = matches[occurrence - 1];
    let col = line.find(&sample.token).unwrap_or_else(|| {
        panic!(
            "token {:?} not in anchor line for {}: {:?}",
            sample.token, sample.path, sample.anchor
        )
    });
    ((line_index + 1) as u32, (col + 1) as u32)
}

fn role_label(is_definition: bool) -> &'static str {
    if is_definition {
        "def"
    } else {
        "ref"
    }
}

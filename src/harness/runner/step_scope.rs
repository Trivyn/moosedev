//! The files a step works on, and the source that comes with them (context
//! plan item 5, Requirement 6ffabf80). The scope is chosen from the step's
//! state, never from what the model happened to read: the plan's files, the
//! files earlier approved plans of this task listed, then in Auto the files
//! the current errors are in and the approved spec in play (the spec file
//! alone), or in Plan that spec and the files it covers. Scope files not
//! yet in the working set join it as preloaded source (shown as the working
//! set is, never touching recency, read snapshots or the read files), within
//! a small share of the prompt; a read of a file outside a non-empty scope
//! is served as the Last result without joining the working set. An empty
//! scope is today's behaviour everywhere. `MOOSEDEV_HARNESS_SOURCE_SCOPE=off`
//! switches all of it off (Requirement e9166711).
use super::dispatch::error_lines;
use super::source::{files_named_in, protected_source, source_block};
use super::{ContextResponse, Mode, Runner, MAX_FILES};
use crate::harness::protocol::ApprovedSpecStatus;
use std::collections::BTreeSet;

/// At most this many files are preloaded at once.
const PRELOAD_MAX_FILES: usize = 24;
/// Preloaded outlines may take at most this fraction of the prompt budget.
const PRELOAD_SHARE: usize = 10;
/// The scope note is at most this long; the preload check holds it back.
pub(super) const SCOPE_NOTE_BYTES: usize = 400;
/// Bytes a failed run's error lines may take, grouped by file
/// ([`errors_by_file`]).
pub(super) const FAILURE_ERRORS_BYTES: usize = 2_000;

/// Whether source is selected by scope. `MOOSEDEV_HARNESS_SOURCE_SCOPE=off`
/// restores the working set of reads alone.
pub(super) fn enabled() -> bool {
    std::env::var("MOOSEDEV_HARNESS_SOURCE_SCOPE").map_or(true, |value| value.trim() != "off")
}

/// This step's scope, decided at its start (not persisted: it is derived
/// from task state every step).
#[derive(Debug, Default)]
pub(super) struct StepScope {
    /// The scope's files, in scope order, deduplicated.
    pub files: Vec<String>,
    /// Scope files on disk that are not in the working set: over the preload
    /// caps or the prompt's room. The source section names them.
    pub skipped: Vec<String>,
}

/// The scope files that may be preloaded this step, by count and size alone,
/// with their current text; the prompt's room is weighed after the refresh.
#[derive(Debug, Default)]
pub(super) struct PreloadCandidates {
    candidates: Vec<(String, String)>,
    skipped: Vec<String>,
}

impl PreloadCandidates {
    /// The files whose governing rules a Plan-mode refresh asks for.
    pub(super) fn rule_files(&self) -> Vec<String> {
        self.candidates
            .iter()
            .map(|(file, _)| file.clone())
            .collect()
    }
}

fn push_unique(list: &mut Vec<String>, file: &str) {
    if !list.iter().any(|known| known == file) {
        list.push(file.to_string());
    }
}

/// Whether a covered path is a place in the project, not the whole of it.
fn names_place(path: &str) -> bool {
    !matches!(path, "" | "." | "./")
}

/// Whether a covered path takes in `file`: a directory (trailing `/`) takes
/// everything under it, a file path only itself. Whole-project coverage
/// names no place and takes nothing.
fn covered(path: &str, file: &str) -> bool {
    match path {
        whole if !names_place(whole) => false,
        directory if directory.ends_with('/') => file.starts_with(directory),
        exact => file == exact,
    }
}

/// The repository files whose last path segment appears in `output` as a
/// token: the only ones `files_named_in` can find there, so a large repository
/// is not scanned once per file.
fn named_candidates<'a>(output: &str, repo: &'a [String]) -> Vec<&'a String> {
    let names: BTreeSet<&str> = output
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')))
        .filter_map(|token| token.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .collect();
    repo.iter()
        .filter(|file| names.contains(file.rsplit('/').next().unwrap_or(file)))
        .collect()
}

/// A failed run's error lines ([`error_lines`]) in output order, each
/// `-->` location only when it is printed right under an error line: the
/// location of a warning or a note is no error's.
fn failure_error_lines(output: &str) -> Vec<&str> {
    let errors = error_lines(output);
    let mut kept = Vec::new();
    let mut under_error = false;
    for line in output.lines().map(str::trim) {
        let location = line.starts_with("--> ");
        let keep = errors.contains(line) && (!location || under_error);
        if keep {
            kept.push(line);
        }
        under_error = keep && !location;
    }
    kept
}

/// The part of a failed run's `output` its error files are read from: its
/// error lines and their locations ([`failure_error_lines`]), so a path the
/// output names only in passing (a compiling crate, a warning, a test name)
/// is not taken for an error's file. The whole output when it has no error
/// line.
fn failure_error_text(output: &str) -> std::borrow::Cow<'_, str> {
    let lines = failure_error_lines(output);
    if lines.is_empty() {
        output.into()
    } else {
        lines.join("\n").into()
    }
}

/// The error lines of a failed run's `output` ([`failure_error_lines`]), grouped
/// under the repository file each names, or that the `--> file:line` under
/// it points at, in the order the output first names each file; lines naming
/// no project file come last. Within `limit` bytes, with a counted line for
/// the rest. Empty when the output has no error lines.
pub(super) fn errors_by_file(output: &str, repo: &[String], limit: usize) -> String {
    type Groups<'a> = Vec<(Option<String>, Vec<&'a str>)>;
    fn add<'a>(groups: &mut Groups<'a>, file: Option<String>, line: &'a str) {
        match groups.iter_mut().find(|(known, _)| *known == file) {
            Some((_, lines)) if lines.contains(&line) => {}
            Some((_, lines)) => lines.push(line),
            None => groups.push((file, vec![line])),
        }
    }
    let errors: BTreeSet<&str> = failure_error_lines(output).into_iter().collect();
    let candidates = named_candidates(output, repo);
    let named = |line: &str| files_named_in(line, &candidates).into_iter().next();
    let mut groups: Groups = Vec::new();
    // An error line waiting for the location printed under it.
    let mut pending: Option<&str> = None;
    for line in output.lines().map(str::trim) {
        if !errors.contains(line) {
            continue;
        }
        if line.starts_with("--> ") {
            let file = named(line);
            if let Some(error) = pending.take() {
                add(&mut groups, file.clone(), error);
            }
            add(&mut groups, file, line);
            continue;
        }
        if let Some(error) = pending.take() {
            add(&mut groups, None, error);
        }
        match named(line) {
            Some(file) => add(&mut groups, Some(file), line),
            None => pending = Some(line),
        }
    }
    if let Some(error) = pending {
        add(&mut groups, None, error);
    }
    groups.sort_by_key(|(file, _)| file.is_none());
    let mut out = String::new();
    let mut omitted = 0;
    for (file, lines) in groups {
        let heading = format!(
            "{}:\n",
            file.as_deref().unwrap_or("(no project file named)")
        );
        let mut block = String::new();
        for line in lines {
            let entry = format!("  {line}\n");
            let heading_cost = if block.is_empty() { heading.len() } else { 0 };
            if out.len() + block.len() + heading_cost + entry.len() > limit {
                omitted += 1;
                continue;
            }
            if block.is_empty() {
                block.push_str(&heading);
            }
            block.push_str(&entry);
        }
        out.push_str(&block);
    }
    if omitted > 0 {
        out.push_str(&format!("{omitted} more error line(s) not shown.\n"));
    }
    out
}

impl Runner {
    /// Files the current errors are in: the settled language-server errors,
    /// then the files the latest failed command's errors name, once each
    /// ([`failure_error_text`]).
    pub(super) fn current_error_files(&self, repo: &[String]) -> Vec<String> {
        let mut files = Vec::new();
        if let Some(diagnostics) = self.task.diagnostics.as_ref().filter(|d| d.settled) {
            for finding in &diagnostics.errors {
                push_unique(&mut files, &finding.file);
            }
        }
        if let Some(output) = self.latest_failure_output() {
            let text = failure_error_text(output);
            for file in files_named_in(&text, &named_candidates(&text, repo)) {
                push_unique(&mut files, &file);
            }
        }
        files
    }

    /// This step's scope, in order and deduplicated: the plan's files, the
    /// files earlier approved plans listed, then in Auto the current error
    /// files and the approved spec in play ([`Self::specs_in_play`]), so the
    /// builder keeps the spec its plan implements in view; in Plan the spec
    /// in play and the files it covers. Empty when the switch is off or
    /// nothing is in scope.
    pub(super) fn step_scope(&self, repo: &[String]) -> Vec<String> {
        if !enabled() {
            return Vec::new();
        }
        let mut files = Vec::new();
        let planned = self.task.plan.iter().flat_map(|plan| plan.files.iter());
        let approved = self
            .task
            .approved_plans
            .iter()
            .flat_map(|plan| plan.files.iter());
        for file in planned.chain(approved) {
            push_unique(&mut files, file);
        }
        let specs = self.specs_in_play(&files);
        let mut state = Vec::new();
        if self.task.mode == Mode::Auto {
            state = self.current_error_files(repo);
        }
        for spec in specs {
            // The spec itself, so the planner and the builder keep it in view.
            if repo.binary_search(&spec.path).is_ok() {
                push_unique(&mut state, &spec.path);
            }
            // In Auto the plan chose the files to work on; the spec's other
            // files are the planner's.
            if self.task.mode == Mode::Plan {
                for file in repo {
                    if spec.covers.iter().any(|path| covered(path, file)) {
                        push_unique(&mut state, file);
                    }
                }
            }
        }
        for file in &state {
            push_unique(&mut files, file);
        }
        files
    }

    /// The approved specs in play: specs whose approval is current, with
    /// rules still open and a place they cover, that the objective, the
    /// guidance or a read names; else the only such spec; else those that
    /// cover the most of `planned` (the plans' files), when any covers one.
    /// A spec covering the whole project names no place, so it is never in
    /// play here.
    fn specs_in_play(&self, planned: &[String]) -> Vec<&ApprovedSpecStatus> {
        let Some(context) = self.context.as_ref() else {
            return Vec::new();
        };
        let in_play: Vec<_> = context
            .approved_specs
            .iter()
            .filter(|spec| {
                !spec.stale
                    && spec
                        .open_rules
                        .as_ref()
                        .is_some_and(|open| !open.is_empty())
                    && spec.covers.iter().any(|path| names_place(path))
            })
            .collect();
        let text = format!("{}\n{}", self.task.objective, self.task.guidance);
        let named: Vec<_> = in_play
            .iter()
            .filter(|spec| {
                self.task.read_files.contains(&spec.path)
                    || !files_named_in(&text, &[&spec.path]).is_empty()
            })
            .copied()
            .collect();
        if !named.is_empty() {
            return named;
        }
        if in_play.len() == 1 {
            return in_play;
        }
        let covering = |spec: &ApprovedSpecStatus| {
            planned
                .iter()
                .filter(|file| spec.covers.iter().any(|path| covered(path, file)))
                .count()
        };
        let most = in_play.iter().map(|spec| covering(spec)).max().unwrap_or(0);
        if most == 0 {
            return Vec::new();
        }
        in_play
            .into_iter()
            .filter(|spec| covering(spec) == most)
            .collect()
    }

    /// Preloaded files the model has not read, in the working set.
    pub(super) fn preloaded_unread(&self) -> Vec<String> {
        self.task
            .source_preloaded
            .iter()
            .filter(|file| {
                self.task.source.contains_key(*file) && !self.task.read_files.contains(file)
            })
            .cloned()
            .collect()
    }

    /// Whether `file` is preloaded, unread, and was shown in full by the
    /// prompt that produced the current action: what the model has seen of
    /// it is its current source, as for a read.
    pub(super) fn preloaded_in_full(&self, file: &str) -> bool {
        self.task.source_preloaded.contains(file)
            && self.task.source_full.contains(file)
            && self.task.source.contains_key(file)
            && !self.task.read_files.iter().any(|read| read == file)
    }

    /// Whether a model read of `file` is outside this step's non-empty scope:
    /// an existing file neither in the scope nor already read.
    pub(super) fn outside_scope(&self, file: &str) -> bool {
        !self.scope.files.is_empty()
            && !self.scope.files.iter().any(|scoped| scoped == file)
            && !self.task.read_files.iter().any(|read| read == file)
            && self.workspace.read(file).is_ok_and(|text| text.is_some())
    }

    /// Before the step's refresh: decide the scope and which of its files may
    /// be preloaded by count and size (at most [`PRELOAD_MAX_FILES`], their
    /// outlines within a tenth of the prompt budget, the working set under
    /// `MAX_FILES`). Files already preloaded are candidates again, so the
    /// set is chosen afresh each step.
    pub(super) fn scope_candidates(&mut self, repo: &[String]) -> PreloadCandidates {
        self.scope.files = self.step_scope(repo);
        let mut found = PreloadCandidates::default();
        let Ok(budget) = self.prompt_budget() else {
            return found;
        };
        let preloaded = self.preloaded_unread();
        let mut kept = self
            .task
            .source
            .keys()
            .filter(|file| !preloaded.contains(file))
            .count();
        let mut bytes = 0;
        for file in self.scope.files.clone() {
            let loaded = self.task.source.contains_key(&file) && !preloaded.contains(&file);
            if loaded || self.task.read_files.contains(&file) || repo.binary_search(&file).is_err()
            {
                continue;
            }
            let Ok(Some(text)) = self.workspace.read(&file) else {
                continue;
            };
            let outline = source_block(&file, &Some(text.clone())).outline.len();
            if found.candidates.len() < PRELOAD_MAX_FILES
                && bytes + outline <= budget / PRELOAD_SHARE
                && kept + 1 < MAX_FILES
            {
                bytes += outline;
                kept += 1;
                found.candidates.push((file, text));
            } else {
                found.skipped.push(file);
            }
        }
        found
    }

    /// After the refresh: make the preloaded set the candidates that fit.
    /// A candidate joins only while the prompt on `context` keeps full
    /// source's whole share and the observation floor with its outline added
    /// ([`Self::outline_allowance`]), so a preload never shrinks what a step
    /// shows in full and never overflows a prompt that fitted; the rest are
    /// named in the source section. Preloaded files no longer chosen (they
    /// left the scope, or no longer fit) leave the working set, unless the
    /// model read them. Journals `scope_preload` when the set changes.
    pub(super) fn preload_scope(&mut self, context: &ContextResponse, found: PreloadCandidates) {
        let before = (
            self.task.source_preloaded.clone(),
            std::mem::take(&mut self.scope.skipped),
        );
        for file in self.preloaded_unread() {
            self.task.source.remove(&file);
        }
        self.task.source_preloaded.clear();
        let allowance = if found.candidates.is_empty() {
            None
        } else {
            self.outline_allowance(context)
        };
        let mut blocks = self.source_blocks();
        let mut skipped = found.skipped;
        for (file, text) in found.candidates {
            blocks.push(source_block(&file, &Some(text.clone())));
            if allowance.is_some_and(|room| protected_source(&blocks) + SCOPE_NOTE_BYTES <= room) {
                self.task.source.insert(file.clone(), Some(text));
                self.task.source_preloaded.insert(file);
            } else {
                blocks.pop();
                skipped.push(file);
            }
        }
        let order = |file: &String| self.scope.files.iter().position(|scoped| scoped == file);
        skipped.sort_by_key(order);
        self.scope.skipped = skipped;
        if (&self.task.source_preloaded, &self.scope.skipped) != (&before.0, &before.1) {
            let list = |files: Vec<&String>| {
                if files.is_empty() {
                    "none".to_string()
                } else {
                    files
                        .iter()
                        .map(|file| file.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            };
            let detail = format!(
                "{} preloaded: {}; {} not loaded for space: {}",
                self.task.source_preloaded.len(),
                list(self.task.source_preloaded.iter().collect()),
                self.scope.skipped.len(),
                list(self.scope.skipped.iter().collect()),
            );
            self.intent_event("scope_preload", &detail);
        }
    }

    /// Drop every unread preload for the rest of this step, when the prompt
    /// overflowed with the rules the scope brought.
    pub(super) fn withdraw_scope_preload(&mut self) {
        let withdrawn = self.preloaded_unread();
        for file in &withdrawn {
            self.task.source.remove(file);
        }
        self.task.source_preloaded.clear();
        self.scope.skipped.clear();
        let unloaded = if withdrawn.is_empty() {
            String::new()
        } else {
            format!("; unloaded {}", withdrawn.join(", "))
        };
        self.intent_event(
            "scope_preload",
            &format!("withdrawn: the prompt overflowed with the scope's rules, so it is built without them{unloaded}"),
        );
    }

    /// The source section's line naming the scope files not loaded for
    /// space, within [`SCOPE_NOTE_BYTES`]; empty when there are none.
    pub(super) fn scope_note(&self) -> String {
        let skipped = &self.scope.skipped;
        if skipped.is_empty() {
            return String::new();
        }
        let (head, tail) = (
            "Scope files not loaded for space: ",
            "; read one to load it.\n",
        );
        let reserve = ", and 99999 more".len();
        let mut names = String::new();
        let mut shown = 0;
        for file in skipped {
            let separator = if names.is_empty() { "" } else { ", " };
            if head.len() + names.len() + separator.len() + file.len() + reserve + tail.len()
                > SCOPE_NOTE_BYTES
            {
                break;
            }
            names.push_str(separator);
            names.push_str(file);
            shown += 1;
        }
        match (shown, skipped.len() - shown) {
            (_, 0) => {}
            (0, rest) => names = format!("{rest} files"),
            (_, rest) => names.push_str(&format!(", and {rest} more")),
        }
        format!("{head}{names}{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{context_router, serve, test_config, Project};
    use super::super::{ApprovedPlan, Plan};
    use super::*;

    #[test]
    fn a_covered_directory_takes_what_is_under_it_and_the_project_nothing() {
        assert!(covered("badciv-map/", "badciv-map/src/lib.rs"));
        assert!(!covered("badciv-map/", "badciv-mapper/src/lib.rs"));
        assert!(covered("docs/map.md", "docs/map.md"));
        assert!(!covered("docs/map.md", "docs/map.md.bak"));
        for whole in ["", ".", "./"] {
            assert!(!covered(whole, "src/lib.rs"));
        }
    }

    #[test]
    fn a_failed_runs_error_lines_are_grouped_by_the_file_they_point_at() {
        let repo: Vec<String> = ["a/src/lib.rs", "a/src/grid.rs"].map(String::from).to_vec();
        let output = "   Compiling a v0.1.0\n\
error[E0308]: mismatched types\n  --> a/src/grid.rs:12:5\n   |\n\
error[E0425]: cannot find value `x`\n  --> a/src/lib.rs:3:9\n\
error[E0599]: no method `rows`\n  --> a/src/grid.rs:40:1\n\
error: aborting due to 3 previous errors\n\
error: could not compile `a`\n";
        assert_eq!(
            errors_by_file(output, &repo, FAILURE_ERRORS_BYTES),
            "a/src/grid.rs:\n  error[E0308]: mismatched types\n  --> a/src/grid.rs:12:5\n  error[E0599]: no method `rows`\n  --> a/src/grid.rs:40:1\n\
a/src/lib.rs:\n  error[E0425]: cannot find value `x`\n  --> a/src/lib.rs:3:9\n\
(no project file named):\n  error: aborting due to 3 previous errors\n"
        );
        let bounded = errors_by_file(output, &repo, 80);
        assert!(bounded.len() <= 80 + 40, "{bounded}");
        assert!(
            bounded.ends_with("more error line(s) not shown.\n"),
            "{bounded}"
        );
        assert_eq!(errors_by_file("all good\n", &repo, 100), "");
        // A warning's location is not an error line.
        let output = "warning: unused import\n --> a/src/grid.rs:1:1\nerror[E0425]: cannot find value `x`\n  --> a/src/lib.rs:3:9\n";
        assert_eq!(
            errors_by_file(output, &repo, FAILURE_ERRORS_BYTES),
            "a/src/lib.rs:\n  error[E0425]: cannot find value `x`\n  --> a/src/lib.rs:3:9\n"
        );
    }

    #[test]
    fn only_files_whose_name_the_output_shows_are_searched() {
        let repo: Vec<String> = ["a/src/lib.rs", "b/src/main.rs", "c/README.md"]
            .map(String::from)
            .to_vec();
        let output = "error[E0308]: mismatched types\n --> a/src/lib.rs:12:5\n";
        let candidates: Vec<&str> = named_candidates(output, &repo)
            .into_iter()
            .map(String::as_str)
            .collect();
        assert_eq!(candidates, ["a/src/lib.rs"]);
    }

    fn plan(files: &[&str]) -> Plan {
        serde_json::from_value(serde_json::json!({
            "summary": "Work on the files", "files": files, "checks": ["true"]
        }))
        .unwrap()
    }

    fn spec(path: &str, covers: &[&str], open: bool) -> ApprovedSpecStatus {
        ApprovedSpecStatus {
            path: path.into(),
            stale: false,
            record_count: 3,
            open_rules: Some(if open {
                vec!["Parse the map".into()]
            } else {
                vec![]
            }),
            covers: covers.iter().map(|path| path.to_string()).collect(),
        }
    }

    /// Writes `files` (Rust source of `lines` functions each) under the
    /// project and returns the sorted repository listing.
    fn write(project: &Project, files: &[&str], lines: usize) -> Vec<String> {
        for file in files {
            let path = project.0.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let text: String = (0..lines)
                .map(|i| format!("fn item_{i}() -> u32 {{\n    {i}\n}}\n"))
                .collect();
            std::fs::write(path, text).unwrap();
        }
        let mut repo: Vec<String> = files.iter().map(|file| file.to_string()).collect();
        repo.sort();
        repo
    }

    async fn runner(project: &Project, objective: &str) -> (Runner, tokio::task::JoinHandle<()>) {
        let (daemon, server) = serve(context_router(), project).await;
        let mut runner = Runner::create(project.0.clone(), daemon, objective.into())
            .await
            .unwrap();
        runner.configure(test_config(), None);
        (runner, server)
    }

    #[tokio::test]
    async fn the_scope_is_the_plans_then_the_spec_in_play_in_plan_mode() {
        let project = Project::new("scope-plan");
        let repo = write(
            &project,
            &[
                "map.md",
                "spec.md",
                "map/src/lib.rs",
                "map/src/parse.rs",
                "sim/src/lib.rs",
                "tui/src/main.rs",
            ],
            1,
        );
        let (mut runner, server) = runner(&project, "Build what map.md describes").await;
        let context = runner.context.as_mut().unwrap();
        // The whole-project spec names no place; a stale one and one with
        // nothing open are not in play.
        context.approved_specs = vec![
            spec("spec.md", &[], true),
            spec("map.md", &["map/"], true),
            ApprovedSpecStatus {
                stale: true,
                ..spec("sim.md", &["sim/"], true)
            },
            spec("tui.md", &["tui/"], false),
        ];
        assert_eq!(
            runner.step_scope(&repo),
            ["map.md", "map/src/lib.rs", "map/src/parse.rs"]
        );

        // The plan's files first, then earlier approved plans', each once.
        runner.task.plan = Some(plan(&["sim/src/lib.rs", "map/src/lib.rs"]));
        runner.task.approved_plans.push(
            serde_json::from_value::<ApprovedPlan>(serde_json::json!({
                "summary": "earlier", "files": ["tui/src/main.rs", "sim/src/lib.rs"], "edit_start": 0
            }))
            .unwrap(),
        );
        assert_eq!(
            runner.step_scope(&repo),
            [
                "sim/src/lib.rs",
                "map/src/lib.rs",
                "tui/src/main.rs",
                "map.md",
                "map/src/parse.rs"
            ]
        );

        // Two specs in play and neither named: no spec scope.
        runner.task.plan = None;
        runner.task.approved_plans.clear();
        runner.task.objective = "Carry on".into();
        runner.context.as_mut().unwrap().approved_specs = vec![
            spec("map.md", &["map/"], true),
            spec("sim.md", &["sim/"], true),
        ];
        assert!(runner.step_scope(&repo).is_empty());
        // A read of one names it.
        runner.task.read_files.push("sim.md".into());
        assert_eq!(runner.step_scope(&repo), ["sim/src/lib.rs"]);
        server.abort();
    }

    #[tokio::test]
    async fn in_auto_the_scope_adds_the_files_the_current_errors_are_in() {
        let project = Project::new("scope-auto");
        let repo = write(
            &project,
            &["a/src/lib.rs", "b/src/lib.rs", "c/src/lib.rs"],
            1,
        );
        let (mut runner, server) = runner(&project, "Fix the build").await;
        runner.task.mode = Mode::Auto;
        runner.task.plan = Some(plan(&["a/src/lib.rs"]));
        runner.event(
            "Command: cargo build\nPermission grants: none\nSuccess: false\nerror: --> c/src/lib.rs:1:1\n --> a/src/lib.rs:2:1"
                .to_string(),
        );
        assert_eq!(
            runner.current_error_files(&repo),
            ["c/src/lib.rs", "a/src/lib.rs"]
        );
        assert_eq!(runner.step_scope(&repo), ["a/src/lib.rs", "c/src/lib.rs"]);

        // A path the output names outside its error lines is no error file.
        runner.event(
            "Command: cargo test\nPermission grants: none\nSuccess: false\n   Compiling c v0.1.0 (c/src/lib.rs)\nwarning: unused import\n --> b/src/lib.rs:3:5\nerror[E0308]: mismatched types\n --> a/src/lib.rs:2:1\nerror: could not compile `a`"
                .to_string(),
        );
        assert_eq!(runner.current_error_files(&repo), ["a/src/lib.rs"]);
        // With no error line at all, the whole output is read.
        runner.event(
            "Command: cargo test\nPermission grants: none\nSuccess: false\nthread 'main' panicked at b/src/lib.rs:4:9"
                .to_string(),
        );
        assert_eq!(runner.current_error_files(&repo), ["b/src/lib.rs"]);
        server.abort();
    }

    #[tokio::test]
    async fn in_auto_the_scope_adds_the_spec_in_play_but_not_its_other_files() {
        let project = Project::new("scope-auto-spec");
        let repo = write(
            &project,
            &[
                "map.md",
                "sim.md",
                "map/src/lib.rs",
                "map/src/parse.rs",
                "sim/src/lib.rs",
            ],
            1,
        );
        let (mut runner, server) = runner(&project, "Carry on").await;
        runner.task.mode = Mode::Auto;
        runner.context.as_mut().unwrap().approved_specs = vec![
            spec("map.md", &["map/"], true),
            spec("sim.md", &["sim/"], true),
        ];
        // Neither spec named and two in play: the one covering the plan's
        // files is the spec in play, and only the spec file joins.
        runner.task.plan = Some(plan(&["map/src/lib.rs"]));
        assert_eq!(runner.step_scope(&repo), ["map/src/lib.rs", "map.md"]);
        // The spec covering the most of the plan's files.
        runner.task.plan = Some(plan(&[
            "sim/src/lib.rs",
            "map/src/lib.rs",
            "map/src/parse.rs",
        ]));
        assert_eq!(
            runner.step_scope(&repo),
            [
                "sim/src/lib.rs",
                "map/src/lib.rs",
                "map/src/parse.rs",
                "map.md"
            ]
        );
        // A plan neither spec covers has no spec in play.
        runner.task.plan = Some(plan(&["Cargo.toml"]));
        assert_eq!(runner.step_scope(&repo), ["Cargo.toml"]);
        // A spec the objective names is in play whatever the plan covers,
        // after the plan's files and the current error files.
        runner.task.plan = Some(plan(&["map/src/lib.rs"]));
        runner.task.objective = "Build what sim.md describes".into();
        runner.event(
            "Command: cargo build\nPermission grants: none\nSuccess: false\nerror[E0425]: cannot find value `x`\n --> map/src/parse.rs:1:1"
                .to_string(),
        );
        assert_eq!(
            runner.step_scope(&repo),
            ["map/src/lib.rs", "map/src/parse.rs", "sim.md"]
        );
        // In Plan the covering spec brings the files it covers too.
        runner.task.mode = Mode::Plan;
        runner.task.objective = "Carry on".into();
        assert_eq!(
            runner.step_scope(&repo),
            ["map/src/lib.rs", "map.md", "map/src/parse.rs"]
        );
        server.abort();
    }

    #[tokio::test]
    async fn preloads_are_capped_and_the_rest_named_in_the_source_section() {
        let project = Project::new("scope-preload");
        let files: Vec<String> = (0..30).map(|i| format!("map/src/m{i:02}.rs")).collect();
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        let repo = write(&project, &names, 2);
        let (mut runner, server) = runner(&project, "Plan the map crate").await;
        runner.task.plan = Some(plan(&names));
        let context = runner.context.clone().unwrap();

        let found = runner.scope_candidates(&repo);
        assert_eq!(found.rule_files().len(), PRELOAD_MAX_FILES);
        runner.preload_scope(&context, found);
        assert_eq!(runner.task.source_preloaded.len(), PRELOAD_MAX_FILES);
        assert!(runner.task.read_files.is_empty());
        assert!(runner.task.source_recency.is_empty());
        assert!(runner
            .task
            .symbolic
            .as_ref()
            .is_none_or(|state| state.read_snapshots.is_empty()));
        assert_eq!(runner.scope.skipped, &files[24..]);
        let (prompt, view) = runner.prompt(&context, &repo).unwrap();
        assert!(
            prompt.contains("Scope files not loaded for space: map/src/m24.rs, map/src/m25.rs, map/src/m26.rs, map/src/m27.rs, map/src/m28.rs, map/src/m29.rs; read one to load it.\n"),
            "{prompt}"
        );
        // Preloaded files rank as the scope, shown in full while they fit.
        assert!(view
            .placed
            .iter()
            .any(|placed| placed.reason == "scope"
                && placed.tier == super::super::source::Tier::Full));
        assert!(view
            .placed
            .iter()
            .all(|placed| placed.reason == "scope" || placed.reason == "over_budget"));
        let journaled = runner
            .task
            .intent_events
            .iter()
            .filter(|event| event.kind == "scope_preload")
            .count();
        assert_eq!(journaled, 1);

        // Unchanged next step: nothing new is journaled. A file that leaves
        // the scope unread leaves the working set.
        runner.task.plan = Some(plan(&names[1..]));
        let found = runner.scope_candidates(&repo);
        runner.preload_scope(&context, found);
        assert!(!runner.task.source.contains_key("map/src/m00.rs"));
        assert_eq!(runner.task.source_preloaded.len(), PRELOAD_MAX_FILES);

        // A prompt with no room for more outlines preloads nothing.
        let mut crowded = context.clone();
        crowded.context = "k".repeat(60_000);
        let found = runner.scope_candidates(&repo);
        runner.preload_scope(&crowded, found);
        assert!(runner.task.source_preloaded.is_empty());
        assert!(runner.task.source.is_empty());
        assert_eq!(runner.scope.skipped.len(), 29);
        assert!(runner.scope_note().len() <= SCOPE_NOTE_BYTES);
        let note = runner.scope_note();
        assert!(note.starts_with("Scope files not loaded for space: map/src/m01.rs, "));
        assert!(
            note.ends_with("map/src/m20.rs, and 9 more; read one to load it.\n"),
            "{note}"
        );
        server.abort();
    }
}

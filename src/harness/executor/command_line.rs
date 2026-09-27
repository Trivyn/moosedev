//! Whether a verification command line can start under the confined shell.
//! Checks run verbatim through `/bin/sh -c` in the source snapshot with the
//! trusted PATH; prose written in place of a command fails there with exit 127.
use super::sandbox::trusted_path;
use crate::code::substrate::lang::{check_tools, CheckTool};
use std::path::{Component, Path, PathBuf};

/// POSIX reserved words and builtins that start a command without a file.
const SHELL_WORDS: &[&str] = &[
    "!", "{", "[", "[[", ".", ":", "alias", "break", "case", "cd", "command", "continue", "echo",
    "eval", "exec", "exit", "export", "false", "for", "getopts", "hash", "if", "kill", "local",
    "printf", "pwd", "read", "readonly", "return", "set", "shift", "source", "test", "times",
    "trap", "true", "type", "ulimit", "umask", "unalias", "unset", "until", "wait", "while",
];

/// `None` when the first word of `command` names a shell builtin or keyword,
/// an executable on the confined PATH, or a file in the project `root`;
/// otherwise why the shell would report the command as not found. Only the
/// first word is judged, so a real command line is never rejected for a later
/// word the shell resolves at run time.
pub fn unrunnable_reason(root: &Path, command: &str) -> Option<String> {
    let Some(word) = first_word(command) else {
        return Some("the check has no command".into());
    };
    if SHELL_WORDS.contains(&word) {
        return None;
    }
    if word.contains('/') {
        let path = Path::new(word);
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        return (!candidate.is_file()).then(|| format!("`{word}` is not a file in the project"));
    }
    // Without a trusted PATH the executor cannot run anything either; leave the
    // judgment to the run rather than rejecting every command.
    let Ok(paths) = trusted_path() else {
        return None;
    };
    let installed =
        std::env::split_paths(&paths).any(|directory| is_executable(&directory.join(word)));
    (!installed)
        .then(|| format!("`{word}` is not an installed program, a shell builtin or a project file"))
}

/// Why `command`, a plan check, could not find its project: it runs a check
/// tool (`cargo`) that finds its project by a manifest at or above the
/// directory it runs in, and none exists there, on disk or among the plan's
/// `planned` files. Names both fixes, so the model chooses; the architecture
/// is not the harness's to decide (badciv 4a6d9bed: a standalone crate
/// checked with `cargo test -p` from a root with no `Cargo.toml`).
///
/// Only a command this can read is judged: an optional leading
/// `cd <relative dir> &&`, then one check tool running a subcommand that
/// needs the project, with nothing after it but a pipe. Anything else
/// (further commands, an option that runs the tool elsewhere, quoting or
/// expansions) is `None`: a valid plan is never sent back on a guess.
pub fn missing_project_reason(root: &Path, command: &str, planned: &[String]) -> Option<String> {
    let mut words = command.split_whitespace().peekable();
    let mut directory = PathBuf::new();
    if words.peek() == Some(&"cd") {
        words.next();
        directory = relative(words.next()?)?;
        if words.next() != Some("&&") {
            return None;
        }
    }
    let program = words.find(|word| !is_assignment(word))?;
    let tool = check_tools().find(|tool| tool.program == program)?;
    let mut arguments = Vec::new();
    let mut piped = false;
    for word in words {
        if piped {
            // Only another command after the pipe; a later `&&` or `;`
            // runs something this does not judge.
            if matches!(word, "&&" | "||" | ";") || word.ends_with(';') {
                return None;
            }
            continue;
        }
        if word == "|" || word.starts_with('|') {
            piped = true;
            continue;
        }
        if matches!(word, "&&" | "||" | ";")
            || word.ends_with(';')
            || word.contains(['\'', '"', '$', '`'])
        {
            return None;
        }
        arguments.push(word);
    }
    // Arguments after `--` belong to the program being run, not the tool.
    if let Some(end) = arguments.iter().position(|argument| *argument == "--") {
        arguments.truncate(end);
    }
    let elsewhere = |argument: &&str| {
        tool.directory_options.iter().any(|option| {
            *argument == *option
                || argument.starts_with(&format!("{option}="))
                || (!option.starts_with("--") && argument.starts_with(option))
        })
    };
    if arguments.iter().any(elsewhere) {
        return None;
    }
    let subcommand = arguments
        .iter()
        .find(|argument| !argument.starts_with('-'))?;
    if !tool.subcommands.contains(subcommand) {
        return None;
    }
    let planned: Vec<String> = planned
        .iter()
        .filter_map(|file| relative(file).and_then(|path| slashed(&path)))
        .collect();
    let present = |path: &Path| {
        root.join(path).is_file() || slashed(path).is_some_and(|path| planned.contains(&path))
    };
    let example = planned.iter().find(|file| {
        Path::new(file)
            .file_name()
            .is_some_and(|name| name == tool.manifest)
    });
    if let Some(option) = tool.manifest_option {
        let named = arguments.iter().enumerate().find_map(|(index, argument)| {
            if *argument == option {
                arguments.get(index + 1).copied()
            } else {
                argument.strip_prefix(option)?.strip_prefix('=')
            }
        });
        if let Some(named) = named {
            let path = directory.join(relative(named)?);
            return (!present(&path)).then(|| {
                format!(
                    "`{}` is pointed at {named}, which does not exist and is not among the plan's files: add it to them, or point {option} at a {} the plan's files include{}",
                    tool.program,
                    tool.manifest,
                    example.map(|file| format!(" ({file})")).unwrap_or_default()
                )
            });
        }
    }
    // Up to the project root only: checks run in a snapshot of the project,
    // where a manifest above it is not part of what the tool can read.
    let mut at = directory.clone();
    loop {
        if present(&at.join(tool.manifest)) {
            return None;
        }
        if !at.pop() {
            break;
        }
    }
    Some(no_manifest(tool, &directory, example))
}

fn no_manifest(tool: &CheckTool, directory: &Path, example: Option<&String>) -> String {
    let (place, manifest) = match slashed(directory).filter(|dir| !dir.is_empty()) {
        Some(dir) => (dir.clone(), format!("{dir}/{}", tool.manifest)),
        None => ("the project root".to_owned(), tool.manifest.to_owned()),
    };
    let alternative = match (tool.manifest_option, example) {
        (Some(option), Some(file)) => {
            format!(", or point the check at the one among them ({option} {file})")
        }
        (Some(option), None) => {
            format!(", or point the check at a package's manifest with {option}")
        }
        (None, _) => String::new(),
    };
    format!(
        "`{}` runs in {place}, but there is no {} there or above it and none among the plan's files: add {manifest} to the plan's files{alternative}",
        tool.program, tool.manifest
    )
}

/// `path` as a plain relative path, `.` removed; `None` for an absolute path,
/// a `..` or anything the shell would expand.
fn relative(path: &str) -> Option<PathBuf> {
    if path.contains(['$', '`', '~', '*', '?', '\'', '"']) {
        return None;
    }
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn slashed(path: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = path
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect();
    Some(parts?.join("/"))
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The command word, skipping `NAME=value` assignments and grouping or
/// separator punctuation attached to it.
fn first_word(command: &str) -> Option<&str> {
    command.split_whitespace().find_map(|token| {
        let word = token
            .trim_start_matches('(')
            .trim_end_matches([';', '&', '|', ')'])
            .trim_matches(['\'', '"']);
        (!word.is_empty() && !is_assignment(word)).then_some(word)
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Project(std::path::PathBuf);
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prose_in_place_of_a_command_is_named() {
        let root = Path::new("/");
        let reason = unrunnable_reason(
            root,
            "The implementation must ensure that processing the same `request_id` returns the receipt.",
        )
        .unwrap();
        assert!(reason.contains("`The`"), "{reason}");
        assert!(unrunnable_reason(root, "   ")
            .unwrap()
            .contains("no command"));
        assert!(unrunnable_reason(root, "definitely-not-an-installed-tool --check").is_some());
    }

    #[test]
    fn installed_programs_builtins_and_shell_syntax_are_runnable() {
        let root = Path::new("/");
        for command in [
            "sh -c 'exit 0'",
            "env LANG=C sh -c true",
            "test -f code.txt",
            "true",
            "cd tests && definitely-not-an-installed-tool",
            "PYTHONPATH=src sh -c true",
            "(cd tests; sh -c true)",
            "{ sh -c true; }",
            "\"sh\" -c true",
            "sh; true",
            "/bin/sh -c true",
        ] {
            assert_eq!(unrunnable_reason(root, command), None, "{command}");
        }
        assert!(unrunnable_reason(root, "/definitely/not/a/tool").is_some());
    }

    #[test]
    fn project_scripts_resolve_against_the_project_root() {
        let project = Project(
            std::env::temp_dir().join(format!("moosedev-command-line-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(project.0.join("scripts")).unwrap();
        std::fs::write(project.0.join("scripts/check.sh"), "#!/bin/sh\n").unwrap();
        assert_eq!(
            unrunnable_reason(&project.0, "scripts/check.sh --fast"),
            None
        );
        assert_eq!(unrunnable_reason(&project.0, "./scripts/check.sh"), None);
        let missing = unrunnable_reason(&project.0, "./scripts/missing.sh").unwrap();
        assert!(missing.contains("not a file in the project"), "{missing}");
    }

    #[test]
    fn a_check_must_find_its_project_on_disk_or_in_the_plan() {
        let project = Project(
            std::env::temp_dir().join(format!("moosedev-check-project-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&project.0).unwrap();
        let root = project.0.as_path();
        let crate_only = |files: &[&str]| {
            files
                .iter()
                .map(|file| file.to_string())
                .collect::<Vec<_>>()
        };
        let planned = crate_only(&["badciv-map/Cargo.toml", "badciv-map/src/lib.rs"]);

        // badciv 4a6d9bed: a standalone crate, checked from a root with none.
        let reason = missing_project_reason(root, "cargo test -p badciv-map", &planned).unwrap();
        assert!(reason.contains("runs in the project root"), "{reason}");
        assert!(
            reason.contains("add Cargo.toml to the plan's files"),
            "{reason}"
        );
        assert!(
            reason.contains("--manifest-path badciv-map/Cargo.toml"),
            "{reason}"
        );
        let piped = missing_project_reason(root, "cargo test 2>&1 | tail -30", &planned);
        assert!(piped.is_some());

        // Either fix is enough: plan the root manifest, or point at the crate's.
        let with_root = crate_only(&["Cargo.toml", "badciv-map/Cargo.toml"]);
        assert_eq!(
            missing_project_reason(root, "cargo test -p badciv-map", &with_root),
            None
        );
        for command in [
            "cargo test --manifest-path badciv-map/Cargo.toml",
            "cargo test --manifest-path=./badciv-map/Cargo.toml",
            "cd badciv-map && cargo test",
            "RUST_BACKTRACE=1 cargo test --manifest-path badciv-map/Cargo.toml",
        ] {
            assert_eq!(
                missing_project_reason(root, command, &planned),
                None,
                "{command}"
            );
        }
        let unplanned = missing_project_reason(
            root,
            "cargo test --manifest-path other/Cargo.toml",
            &planned,
        )
        .unwrap();
        assert!(
            unplanned.contains("pointed at other/Cargo.toml"),
            "{unplanned}"
        );
        // After `--` the arguments are the program's, not cargo's.
        assert_eq!(
            missing_project_reason(
                root,
                "cargo run -- --manifest-path missing/Cargo.toml",
                &crate_only(&["Cargo.toml"])
            ),
            None
        );

        // A manifest already on disk counts.
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(
            missing_project_reason(root, "cargo test -p badciv-map", &planned),
            None
        );
        std::fs::remove_file(root.join("Cargo.toml")).unwrap();

        // npm by its package.json; commands this cannot read are not judged.
        assert!(missing_project_reason(root, "npm test", &planned).is_some());
        assert_eq!(
            missing_project_reason(root, "npm test", &crate_only(&["package.json"])),
            None
        );
        for command in [
            "python -m pytest",
            "sh -c 'cargo test'",
            "cd $DIR && cargo test",
            "cargo test --manifest-path \"a b/Cargo.toml\"",
            "cd ../x && cargo test",
            // Codex review: not the tool's own project, or not one command.
            "cargo --version && cd badciv-map && cargo test",
            "cargo --version",
            "cargo install ripgrep",
            "npm --prefix web test",
            "cargo -C badciv-map test",
            "cargo test; echo done",
        ] {
            assert_eq!(
                missing_project_reason(root, command, &planned),
                None,
                "{command}"
            );
        }
    }
}

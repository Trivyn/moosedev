//! Whether a verification command line can start under the confined shell.
//! Checks run verbatim through `/bin/sh -c` in the source snapshot with the
//! trusted PATH; prose written in place of a command fails there with exit 127.
use super::sandbox::trusted_path;
use std::path::Path;

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

/// The command word, skipping `NAME=value` assignments and grouping or
/// separator punctuation attached to it.
fn first_word(command: &str) -> Option<&str> {
    command.split_whitespace().find_map(|token| {
        let word = token
            .trim_start_matches('(')
            .trim_end_matches([';', '&', '|', ')'])
            .trim_matches(['\'', '"']);
        let assignment = word.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        (!word.is_empty() && !assignment).then_some(word)
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
}

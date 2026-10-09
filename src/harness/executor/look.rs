//! Look requests: a planning model's `command` is read as a question about
//! the project or its environment, and the harness answers it the most useful
//! way it can (James, 2026-10-08): a file view becomes the grounded read, a
//! listing or a literal grep is answered from the workspace's own file list,
//! a toolchain or dependency-source look runs in the read-only sandbox, git
//! runs host-side read-only (the sandbox snapshot omits `.git`), and anything
//! that writes or needs the network is declined with the reason. Nothing here
//! changes the project.
//!
//! Evidence: in simL1 qwen3.8-27b, offered only literal search in Plan mode,
//! searched 250+ times a prompt without planning; OpenCode's plan agent (simO2)
//! left planning in minutes with `ls`, globs, `cargo build` and a look in the
//! local cargo registry.

mod git;

use std::path::Path;

pub use git::run_git;

use crate::code::substrate::lang::{look_tools, LookTool};

/// What a look request asks, and so how the harness answers it.
#[derive(Debug, Clone)]
pub enum Look {
    /// One file's text (`cat`, `head`, `sed -n 'a,bp'`): the grounded read.
    /// `lines` names a range the model asked for, if any.
    FileView { path: String, lines: Option<String> },
    /// A listing (`ls`, `find`, `tree`, `fd`) of `dir`, recursive or not,
    /// filtered by a `-name`-style glob.
    Listing {
        dir: String,
        name: Option<String>,
        recursive: bool,
    },
    /// A literal search over files (`grep -rn`, `rg`).
    Grep {
        pattern: String,
        paths: Vec<String>,
        ignore_case: bool,
        files_only: bool,
        word: bool,
    },
    /// A toolchain look a language defines (`cargo metadata`).
    Toolchain {
        tool: &'static LookTool,
        args: Vec<String>,
    },
    /// Read-only git (`git log`, `git diff`), run host-side.
    Git { args: Vec<String> },
    /// Read-only, but not one the harness answers itself (a pipeline, a
    /// regex grep, dependency source): run as asked in the sandbox.
    Sandbox,
    /// Not a look: the reason, and the way forward.
    Decline(String),
}

/// Read-only programs allowed as a later stage of a pipeline or chain.
const FILTERS: &[&str] = &[
    "head", "tail", "wc", "sort", "uniq", "cut", "tr", "grep", "rg", "nl", "cat", "echo", "column",
];
/// Programs that change files.
const WRITERS: &[&str] = &[
    "rm", "mv", "cp", "mkdir", "touch", "chmod", "chown", "ln", "tee", "dd", "truncate", "install",
    "rmdir", "patch",
];
/// Programs that need the network.
const NETWORK: &[&str] = &["curl", "wget", "ssh", "scp", "nc", "rsync", "ftp", "telnet"];
/// Harmless programs run as asked.
const HARMLESS: &[&str] = &["echo", "pwd", "which", "true", "date", "uname", "printenv"];
const READ_ONLY: &str = "Plan mode looks are read-only: put this in the plan's steps, or its checks, and it runs once the plan is approved.";

/// Splits a command into words and the operators between them, honouring
/// single and double quotes and backslashes. None when quotes do not close.
fn tokens(command: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => word.push(chars.next()?),
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next()?);
            }
            '\n' | '\r' => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
                out.push("\u{0};".to_owned());
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '|' | '&' | ';' | '>' | '<' | '(' | ')' | '`' | '$' if !in_word || c != '$' => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
                let mut op = c.to_string();
                while let Some(&next) = chars.peek() {
                    if matches!((c, next), ('|', '|') | ('&', '&') | ('>', '>') | ('>', '&')) {
                        op.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push(format!("\u{0}{op}"));
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        out.push(word);
    }
    Some(out)
}

fn operator(token: &str) -> Option<&str> {
    token.strip_prefix('\u{0}')
}

/// Classifies a look request.
pub fn classify(command: &str) -> Look {
    // Substitution runs a command inside another, even within double quotes.
    if command.contains('`') || command.contains("$(") || command.contains("${") {
        return Look::Decline(
            "command substitution is not a look. Ask one plain command at a time.".into(),
        );
    }
    let Some(tokens) = tokens(command) else {
        return Look::Decline("the command's quotes do not close.".into());
    };
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    let mut compound = false;
    let mut iter = tokens.into_iter().peekable();
    while let Some(token) = iter.next() {
        match operator(&token) {
            None => segments.last_mut().unwrap().push(token),
            Some("|" | "||" | "&&" | ";") => {
                compound = true;
                segments.push(Vec::new());
            }
            // A redirect to /dev/null or of stderr into stdout is harmless.
            Some(">" | ">&" | ">>") => {
                let target = iter.next().unwrap_or_default();
                let into_stdout = token == "\u{0}>&" && target == "1";
                let discarded = target == "/dev/null";
                let stderr = segments
                    .last_mut()
                    .and_then(|segment| segment.last())
                    .is_some_and(|word| word == "2");
                if into_stdout || discarded {
                    if stderr {
                        segments.last_mut().unwrap().pop();
                    }
                    continue;
                }
                return Look::Decline(format!("it writes a file (`> {target}`). {READ_ONLY}"));
            }
            Some("<") => {
                iter.next();
            }
            Some(op) => {
                return Look::Decline(format!(
                    "`{op}` (subshells, substitution or background jobs) is not a look. Ask one plain command at a time."
                ))
            }
        }
    }
    segments.retain(|segment| !segment.is_empty());
    let Some(first) = segments.first() else {
        return Look::Decline("the command is empty.".into());
    };
    if !compound {
        return simple(first);
    }
    // A pipeline or chain: read-only throughout runs as asked in the
    // sandbox; git cannot run there.
    for (index, segment) in segments.iter().enumerate() {
        let look = simple(segment);
        match look {
            Look::Decline(_) => return look,
            Look::Git { .. } => {
                return Look::Decline(
                    "git runs on its own, not in a pipeline or chain: ask `git ...` by itself."
                        .into(),
                )
            }
            _ if index > 0 && !is_filter(segment) && !is_look_program(segment) => {
                return Look::Decline(format!(
                    "`{}` in a pipeline is not a look. {READ_ONLY}",
                    segment.join(" ")
                ))
            }
            _ => {}
        }
    }
    Look::Sandbox
}

fn program(segment: &[String]) -> (&str, &[String]) {
    let mut start = 0;
    while start < segment.len()
        && segment[start].contains('=')
        && !segment[start].starts_with('-')
        && segment[start].split('=').next().is_some_and(|name| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    {
        start += 1;
    }
    match segment.get(start) {
        Some(program) => (
            program.rsplit('/').next().unwrap_or(program),
            &segment[start + 1..],
        ),
        None => ("", &[]),
    }
}

fn is_filter(segment: &[String]) -> bool {
    FILTERS.contains(&program(segment).0)
}

fn is_look_program(segment: &[String]) -> bool {
    !matches!(simple(segment), Look::Decline(_))
}

fn operands(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect()
}

/// One plain command.
fn simple(segment: &[String]) -> Look {
    let (program, args) = program(segment);
    match program {
        "" => Look::Decline("the command is empty.".into()),
        "cat" | "less" | "more" | "bat" | "nl" => match operands(args).as_slice() {
            [path] => Look::FileView {
                path: path.clone(),
                lines: None,
            },
            _ => Look::Sandbox,
        },
        "head" | "tail" => {
            let mut count = None;
            let mut paths = Vec::new();
            let mut iter = args.iter();
            while let Some(arg) = iter.next() {
                if arg == "-n" {
                    count = iter.next().cloned();
                } else if let Some(n) = arg.strip_prefix("-n") {
                    count = Some(n.to_owned());
                } else if arg.len() > 1
                    && arg.starts_with('-')
                    && arg[1..].chars().all(|c| c.is_ascii_digit())
                {
                    count = Some(arg[1..].to_owned());
                } else if arg.starts_with('-') {
                    // Bytes, follow, quiet...: run as asked.
                    return Look::Sandbox;
                } else {
                    paths.push(arg.clone());
                }
            }
            let count = count.unwrap_or_else(|| "10".into());
            let numeric = count
                .strip_prefix('+')
                .filter(|_| program == "tail")
                .unwrap_or(&count);
            if numeric.is_empty() || !numeric.chars().all(|c| c.is_ascii_digit()) {
                return Look::Sandbox;
            }
            match paths.as_slice() {
                [path] => Look::FileView {
                    path: path.clone(),
                    lines: Some(format!("{program} {count}")),
                },
                _ => Look::Sandbox,
            }
        }
        "sed" => {
            let in_place = args.iter().any(|arg| {
                arg.starts_with("--in-place")
                    || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('i'))
            });
            if in_place {
                return Look::Decline(format!("`sed -i` edits a file. {READ_ONLY}"));
            }
            let quiet = args.iter().any(|arg| arg == "-n");
            let rest = operands(args);
            // Only a numeric range is a file view; any other script runs in
            // the sandbox as asked.
            match (quiet, rest.as_slice()) {
                (true, [script, path]) if numeric_range(script) => Look::FileView {
                    path: path.clone(),
                    lines: Some(format!("lines {}", script.trim_end_matches('p'))),
                },
                _ => Look::Sandbox,
            }
        }
        "ls" => Look::Listing {
            dir: operands(args)
                .first()
                .cloned()
                .unwrap_or_else(|| ".".into()),
            name: None,
            recursive: args
                .iter()
                .any(|arg| arg.starts_with('-') && arg.contains('R')),
        },
        "tree" => Look::Listing {
            dir: operands(args)
                .first()
                .cloned()
                .unwrap_or_else(|| ".".into()),
            name: None,
            recursive: true,
        },
        "find" => {
            if args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-exec"
                        | "-execdir"
                        | "-delete"
                        | "-ok"
                        | "-okdir"
                        | "-fprint"
                        | "-fprint0"
                        | "-fprintf"
                        | "-fls"
                )
            }) {
                return Look::Decline(format!(
                    "`find` with `-exec` or `-delete` runs or changes things. {READ_ONLY}"
                ));
            }
            let dir = args
                .first()
                .filter(|arg| !arg.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| ".".into());
            let name = args
                .iter()
                .position(|arg| arg == "-name" || arg == "-iname")
                .and_then(|at| args.get(at + 1))
                .cloned();
            Look::Listing {
                dir,
                name,
                recursive: true,
            }
        }
        "fd" => {
            let rest = operands(args);
            Look::Listing {
                dir: rest.get(1).cloned().unwrap_or_else(|| ".".into()),
                name: rest.first().map(|pattern| format!("*{pattern}*")),
                recursive: true,
            }
        }
        "grep" | "rg" | "ag" => grep(args),
        "git" => git::classify(args),
        _ if WRITERS.contains(&program) => {
            Look::Decline(format!("`{program}` changes files. {READ_ONLY}"))
        }
        _ if NETWORK.contains(&program) => Look::Decline(format!(
            "`{program}` needs the network, which looks do not have."
        )),
        "sudo" | "su" => Look::Decline(format!("`{program}` is not a look.")),
        _ if HARMLESS.contains(&program) => Look::Sandbox,
        _ => toolchain(program, args),
    }
}

/// `N`, `N,M` or `N,$` followed by `p`: a sed script that only prints lines.
fn numeric_range(script: &str) -> bool {
    let Some(range) = script.strip_suffix('p') else {
        return false;
    };
    let (from, to) = range.split_once(',').unwrap_or((range, range));
    let number = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
    number(from) && (number(to) || to == "$")
}

/// A grep the harness answers itself when it is a literal search with plain
/// flags; anything else (a regex, context lines) runs in the sandbox.
fn grep(args: &[String]) -> Look {
    let mut ignore_case = false;
    let mut files_only = false;
    let mut word = false;
    let mut fixed = false;
    let mut pattern = None;
    let mut paths = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-e" | "--regexp" => {
                if pattern.is_some() {
                    // Several patterns: run as asked.
                    return Look::Sandbox;
                }
                pattern = iter.next().cloned();
            }
            "-F" | "--fixed-strings" => fixed = true,
            "--" => {}
            arg if arg.starts_with("--include")
                || arg.starts_with("--exclude")
                || arg.starts_with("-g") =>
            {
                return Look::Sandbox
            }
            arg if arg.starts_with("--") => {
                // --line-number, --recursive, --ignore-case...
                match arg {
                    "--ignore-case" => ignore_case = true,
                    "--files-with-matches" => files_only = true,
                    "--word-regexp" => word = true,
                    "--line-number" | "--recursive" | "--no-heading" | "--color=never" => {}
                    _ => return Look::Sandbox,
                }
            }
            arg if arg.starts_with('-') && arg.len() > 1 => {
                for flag in arg[1..].chars() {
                    match flag {
                        'r' | 'R' | 'n' | 'H' | 's' => {}
                        'i' => ignore_case = true,
                        'l' => files_only = true,
                        'w' => word = true,
                        'F' => fixed = true,
                        _ => return Look::Sandbox,
                    }
                }
            }
            arg => {
                if pattern.is_none() {
                    pattern = Some(arg.to_owned());
                } else {
                    paths.push(arg.to_owned());
                }
            }
        }
    }
    let Some(pattern) = pattern else {
        return Look::Sandbox;
    };
    let regex = pattern.chars().any(|c| "\\^$.[]|()*+?{}".contains(c));
    if regex && !fixed {
        return Look::Sandbox;
    }
    Look::Grep {
        pattern,
        paths,
        ignore_case,
        files_only,
        word,
    }
}

/// A program a language's look tools know.
fn toolchain(program: &str, args: &[String]) -> Look {
    let Some(tool) = look_tools().find(|tool| tool.program == program) else {
        return Look::Decline(format!(
            "`{program}` is not a look the harness runs while planning. {READ_ONLY}"
        ));
    };
    let first = args.first().map(String::as_str).unwrap_or("");
    if let Some((_, reason)) = tool.declined.iter().find(|(arg, _)| *arg == first) {
        return Look::Decline(format!("`{program} {first}`: {reason}."));
    }
    if tool.allowed.contains(&first) {
        return Look::Toolchain {
            tool,
            args: args.to_vec(),
        };
    }
    Look::Decline(format!(
        "`{program} {first}` is not a look the harness runs while planning (it runs {}). {READ_ONLY}",
        tool.allowed.join(", ")
    ))
}

/// Whether `path` lies under one of the languages' dependency roots in
/// `home` (a file view or grep of dependency source runs in the sandbox,
/// which can read them).
pub fn in_dependency_root(path: &str, home: &Path) -> bool {
    let path = Path::new(path);
    look_tools().any(|tool| {
        tool.dependency_roots
            .iter()
            .any(|root| path.starts_with(home.join(root)))
    })
}

/// A listing answered from the workspace's file list: the entries under
/// `dir` (directories derived from file paths), recursive or one level,
/// filtered by a simple `*` glob on the entry name.
pub fn listing(files: &[String], dir: &str, name: Option<&str>, recursive: bool) -> String {
    let dir = dir.trim_start_matches("./").trim_end_matches('/');
    let dir = if dir == "." { "" } else { dir };
    if files.iter().any(|file| file == dir) {
        return format!("Listing of `{dir}` (from the harness's file list): the file `{dir}`.");
    }
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let mut entries = std::collections::BTreeSet::new();
    for file in files {
        let Some(rest) = file.strip_prefix(&prefix) else {
            continue;
        };
        let parts: Vec<&str> = rest.split('/').collect();
        let shown: Vec<String> = if recursive {
            (1..=parts.len())
                .map(|depth| {
                    let entry = parts[..depth].join("/");
                    if depth < parts.len() {
                        format!("{prefix}{entry}/")
                    } else {
                        format!("{prefix}{entry}")
                    }
                })
                .collect()
        } else if parts.len() > 1 {
            vec![format!("{}/", parts[0])]
        } else {
            vec![parts[0].to_owned()]
        };
        for entry in shown {
            let leaf = entry
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&entry);
            if name.is_none_or(|pattern| glob(pattern, leaf)) {
                entries.insert(entry);
            }
        }
    }
    const MAX: usize = 400;
    let count = entries.len();
    let mut out: Vec<String> = entries.into_iter().take(MAX).collect();
    if count > MAX {
        out.push(format!("[{} more not listed]", count - MAX));
    }
    let shown = if dir.is_empty() { "." } else { dir };
    if out.is_empty() {
        return format!("Listing of `{shown}` (from the harness's file list): nothing matched.");
    }
    format!(
        "Listing of `{shown}` (from the harness's file list; build output, `.git`, binaries and protected files are not in it):\n{}",
        out.join("\n")
    )
}

/// `*` matches any run of characters; everything else literally.
fn glob(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            match rest.strip_prefix(part) {
                Some(after) => rest = after,
                None => return false,
            }
        } else if index == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(at) = rest.find(part) {
            rest = &rest[at + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// A literal grep answered over the workspace's files, read by `read`.
#[allow(clippy::too_many_arguments)]
pub fn grep_files(
    files: &[String],
    read: impl Fn(&str) -> Option<String>,
    pattern: &str,
    paths: &[String],
    ignore_case: bool,
    files_only: bool,
    word: bool,
    budget: usize,
) -> String {
    let needle = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_owned()
    };
    let under = |file: &str| {
        paths.is_empty()
            || paths.iter().any(|path| {
                let path = path.trim_start_matches("./").trim_end_matches('/');
                path == "."
                    || path.is_empty()
                    || file == path
                    || file.starts_with(&format!("{path}/"))
            })
    };
    let matches_line = |line: &str| {
        let hay = if ignore_case {
            line.to_lowercase()
        } else {
            line.to_owned()
        };
        if !word {
            return hay.contains(&needle);
        }
        hay.match_indices(&needle).any(|(at, _)| {
            let before = hay[..at].chars().next_back();
            let after = hay[at + needle.len()..].chars().next();
            let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            boundary(before) && boundary(after)
        })
    };
    let mut out = String::new();
    let mut count = 0usize;
    let mut omitted = 0usize;
    for file in files.iter().filter(|file| under(file)) {
        let Some(text) = read(file) else { continue };
        for (number, line) in text.lines().enumerate() {
            if !matches_line(line) {
                continue;
            }
            count += 1;
            let entry = if files_only {
                format!("{file}\n")
            } else {
                format!(
                    "{file}:{}:{}\n",
                    number + 1,
                    line.chars().take(300).collect::<String>()
                )
            };
            if out.len() + entry.len() > budget {
                omitted += 1;
            } else {
                out.push_str(&entry);
            }
            if files_only {
                break;
            }
        }
    }
    if count == 0 {
        return format!(
            "No line in the project's files matches `{pattern}`{} (searched by the harness, literally).",
            if paths.is_empty() { String::new() } else { format!(" under {}", paths.join(", ")) }
        );
    }
    if omitted > 0 {
        out.push_str(&format!(
            "[{omitted} more matches not shown; narrow the pattern or the path]\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(command: &str) -> String {
        match classify(command) {
            Look::FileView { .. } => "view",
            Look::Listing { .. } => "list",
            Look::Grep { .. } => "grep",
            Look::Toolchain { .. } => "tool",
            Look::Git { .. } => "git",
            Look::Sandbox => "sandbox",
            Look::Decline(_) => "decline",
        }
        .into()
    }

    #[test]
    fn looks_are_classified_by_what_they_ask() {
        for (command, expected) in [
            ("cat src/lib.rs", "view"),
            ("head -n 40 src/lib.rs", "view"),
            ("sed -n '10,40p' src/lib.rs", "view"),
            ("ls -la badciv-sim", "list"),
            ("find . -name '*.rs' -type f", "list"),
            ("tree badciv-map", "list"),
            ("grep -rn rusqlite .", "grep"),
            ("rg -i 'open game' src", "grep"),
            ("grep -rn 'fn (open|new)' src", "sandbox"),
            ("grep -rn rusqlite src | head -20", "sandbox"),
            ("cargo --version", "tool"),
            ("cargo metadata --format-version 1", "tool"),
            ("cargo build 2>&1 | tail -20", "sandbox"),
            ("rustc --version && cargo --version", "sandbox"),
            ("git log --oneline -5", "git"),
            ("git diff HEAD~1", "git"),
            ("RUST_LOG=debug cargo test -p badciv-sim", "tool"),
            ("rm -rf target", "decline"),
            ("cargo add rusqlite", "decline"),
            ("echo x > notes.txt", "decline"),
            ("sed -i 's/a/b/' src/lib.rs", "decline"),
            ("curl https://crates.io", "decline"),
            ("git commit -am wip", "decline"),
            ("git -c core.pager=less log", "decline"),
            ("git log | head", "decline"),
            ("find . -name '*.tmp' -delete", "decline"),
            ("python3 -m pytest", "decline"),
            ("$(rm -rf /)", "decline"),
            ("frobnicate --all", "decline"),
            ("env rm x", "decline"),
            ("cat a\nrm -rf src", "decline"),
            ("echo \"$(cargo publish)\"", "decline"),
            ("sed -ni 's/a/b/' f", "decline"),
            ("sed --in-place=.bak 's/a/b/' f", "decline"),
            ("sed -n '/fn main/,/^}/p' f", "sandbox"),
            ("sed -n p f", "sandbox"),
            ("sed -n '5,$p' f", "view"),
            ("tail -n +100 f", "view"),
            ("head -c 100 f", "sandbox"),
            ("find . -fprintf out x", "decline"),
            ("find . -fls out", "decline"),
            ("grep -e a -e b src", "sandbox"),
            ("git grep -Otouch x", "decline"),
            ("git log -c", "decline"),
        ] {
            assert_eq!(kind(command), expected, "{command}");
        }
    }

    #[test]
    fn a_decline_names_the_reason_and_the_way_forward() {
        let Look::Decline(reason) = classify("cargo add rusqlite") else {
            panic!("not declined")
        };
        assert!(reason.contains("Cargo.toml"), "{reason}");
        let Look::Decline(reason) = classify("rm -rf target") else {
            panic!("not declined")
        };
        assert!(reason.contains("put this in the plan"), "{reason}");
    }

    #[test]
    fn a_listing_comes_from_the_file_list() {
        let files = vec![
            "Cargo.toml".to_owned(),
            "badciv-map/src/lib.rs".to_owned(),
            "badciv-map/src/parse.rs".to_owned(),
            "badciv-map/tests/a.rs".to_owned(),
        ];
        let top = listing(&files, ".", None, false);
        assert!(top.contains("Cargo.toml\nbadciv-map/"), "{top}");
        let rust = listing(&files, "badciv-map", Some("*.rs"), true);
        assert!(
            rust.contains("badciv-map/src/lib.rs") && rust.contains("badciv-map/tests/a.rs"),
            "{rust}"
        );
        assert!(!rust.contains("Cargo.toml"));
    }

    #[test]
    fn a_literal_grep_is_answered_over_the_files() {
        let files = vec!["a.rs".to_owned(), "b/c.rs".to_owned()];
        let read = |file: &str| {
            Some(match file {
                "a.rs" => "use rusqlite::Connection;\nfn main() {}\n".to_owned(),
                _ => "// Rusqlite here\nlet sqlite = 1;\n".to_owned(),
            })
        };
        let out = grep_files(&files, read, "rusqlite", &[], true, false, false, 4000);
        assert!(
            out.contains("a.rs:1:use rusqlite::Connection;")
                && out.contains("b/c.rs:1:// Rusqlite here"),
            "{out}"
        );
        let words = grep_files(&files, read, "sqlite", &[], false, false, true, 4000);
        assert!(
            words.contains("b/c.rs:2:") && !words.contains("a.rs"),
            "{words}"
        );
        let none = grep_files(
            &files,
            read,
            "postgres",
            &["b".into()],
            false,
            false,
            false,
            4000,
        );
        assert!(none.starts_with("No line"), "{none}");
    }

    #[test]
    fn cargo_search_is_answered_from_the_local_registry() {
        let home = std::env::temp_dir().join(format!("look-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join(".cargo/registry/src/index.crates.io-x/rusqlite-0.31.0"))
            .unwrap();
        let Look::Toolchain { tool, args } = classify("cargo search rusqlite") else {
            panic!("not a toolchain look")
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let answer = (tool.answer.unwrap())(&args, &home).unwrap();
        assert!(
            answer.contains("rusqlite` at 0.31.0") && answer.contains("available offline"),
            "{answer}"
        );
        let missing = (tool.answer.unwrap())(&["search", "diesel"], &home).unwrap();
        assert!(missing.contains("not available offline"), "{missing}");
        assert!(in_dependency_root(
            &home
                .join(".cargo/registry/src/index.crates.io-x/rusqlite-0.31.0/src/lib.rs")
                .to_string_lossy(),
            &home
        ));
        std::fs::remove_dir_all(home).unwrap();
    }
}

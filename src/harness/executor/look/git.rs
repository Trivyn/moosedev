//! Read-only git for looks, run host-side: the sandbox snapshot omits
//! `.git`, so git is the one look that runs outside it, and so the one whose
//! arguments must be allowed, not screened. Each subcommand takes only the
//! options listed for it, spelled out in full (git accepts abbreviations
//! and clustered short flags, so a denylist misses `--open=` and `-iO`, and
//! `git grep -O<cmd>` runs a program). Nothing may come before the
//! subcommand. Positional paths and `rev:path` names may not reach what the
//! workspace protects; patches, greps and listings exclude it by pathspec;
//! and every revision must name a commit, tag or tree, never a blob a hash
//! could otherwise print.

use std::path::Path;
use std::time::Duration;

use super::super::workspace::{protected_path, PROTECTED_GLOBS};
use super::{Look, READ_ONLY};

/// How a git option takes its value.
#[derive(Clone, Copy)]
enum Opt {
    /// No value.
    Flag(&'static str),
    /// A value, as the next word, after `=` (long), or attached (short).
    Value(&'static str),
}
use Opt::{Flag, Value};

/// A read-only subcommand and the options it may take.
struct Sub {
    name: &'static str,
    options: &'static [Opt],
    /// Whether `-<n>` (a count, `git log -5`) is accepted.
    count: bool,
    /// Whether its answer can hold file contents, so protected paths are
    /// excluded by pathspec.
    contents: bool,
}

const SUBS: &[Sub] = &[
    Sub {
        name: "status",
        options: &[
            Flag("-s"),
            Flag("--short"),
            Flag("-b"),
            Flag("--branch"),
            Flag("--porcelain"),
            Flag("--long"),
        ],
        count: false,
        contents: false,
    },
    Sub {
        name: "log",
        options: &[
            Flag("--oneline"),
            Flag("--graph"),
            Flag("--decorate"),
            Flag("--all"),
            Flag("--reverse"),
            Flag("--no-merges"),
            Flag("--merges"),
            Flag("--first-parent"),
            Flag("--abbrev-commit"),
            Flag("-p"),
            Flag("--patch"),
            Flag("--stat"),
            Flag("--shortstat"),
            Flag("--name-only"),
            Flag("--name-status"),
            Value("-n"),
            Value("--max-count"),
            Value("--skip"),
            Value("--since"),
            Value("--until"),
            Value("--after"),
            Value("--before"),
            Value("--author"),
            Value("--grep"),
            Value("--format"),
            Value("--pretty"),
            Value("--date"),
            Value("-S"),
            Value("-G"),
            Value("-U"),
            Value("--unified"),
        ],
        count: true,
        contents: true,
    },
    Sub {
        name: "diff",
        options: &[
            Flag("--cached"),
            Flag("--staged"),
            Flag("--patience"),
            Flag("--histogram"),
            Flag("--stat"),
            Flag("--shortstat"),
            Flag("--numstat"),
            Flag("--name-only"),
            Flag("--name-status"),
            Flag("-w"),
            Flag("--ignore-all-space"),
            Flag("--word-diff"),
            Value("-U"),
            Value("--unified"),
        ],
        count: false,
        contents: true,
    },
    Sub {
        name: "show",
        options: &[
            Flag("--oneline"),
            Flag("--no-patch"),
            Flag("-s"),
            Flag("--abbrev-commit"),
            Flag("--stat"),
            Flag("--shortstat"),
            Flag("--numstat"),
            Flag("--name-only"),
            Flag("--name-status"),
            Flag("-w"),
            Flag("--ignore-all-space"),
            Flag("--word-diff"),
            Value("-U"),
            Value("--unified"),
            Value("--format"),
            Value("--pretty"),
            Value("--date"),
        ],
        count: false,
        contents: true,
    },
    Sub {
        name: "blame",
        options: &[Flag("-w"), Flag("-s"), Flag("-e"), Value("-L")],
        count: false,
        contents: true,
    },
    Sub {
        name: "ls-files",
        options: &[
            Flag("--cached"),
            Flag("-c"),
            Flag("--modified"),
            Flag("-m"),
            Flag("--deleted"),
            Flag("-d"),
        ],
        count: false,
        contents: false,
    },
    Sub {
        name: "ls-tree",
        options: &[Flag("-r"), Flag("-d"), Flag("-t"), Flag("--name-only")],
        count: false,
        contents: false,
    },
    Sub {
        name: "rev-parse",
        options: &[
            Flag("--abbrev-ref"),
            Flag("--short"),
            Flag("--verify"),
            Flag("--show-toplevel"),
            Flag("--is-inside-work-tree"),
        ],
        count: false,
        contents: false,
    },
    Sub {
        name: "branch",
        options: &[
            Flag("--list"),
            Flag("-l"),
            Flag("-a"),
            Flag("--all"),
            Flag("-r"),
            Flag("--remotes"),
            Flag("-v"),
            Flag("-vv"),
            Flag("--verbose"),
            Flag("--show-current"),
            Value("--contains"),
            Value("--no-contains"),
            Value("--merged"),
            Value("--no-merged"),
            Value("--sort"),
            Value("--format"),
        ],
        count: false,
        contents: false,
    },
    Sub {
        name: "grep",
        options: &[
            Flag("-n"),
            Flag("--line-number"),
            Flag("-i"),
            Flag("--ignore-case"),
            Flag("-w"),
            Flag("--word-regexp"),
            Flag("-l"),
            Flag("--files-with-matches"),
            Flag("-L"),
            Flag("--files-without-match"),
            Flag("-c"),
            Flag("--count"),
            Flag("-F"),
            Flag("--fixed-strings"),
            Flag("-E"),
            Flag("--extended-regexp"),
            Flag("-I"),
            Flag("-h"),
            Flag("-H"),
            Flag("--full-name"),
            Value("-e"),
            Value("-C"),
            Value("--context"),
            Value("-A"),
            Value("-B"),
            Value("--max-depth"),
        ],
        count: false,
        contents: true,
    },
    Sub {
        name: "shortlog",
        options: &[
            Flag("-s"),
            Flag("--summary"),
            Flag("-n"),
            Flag("--numbered"),
            Flag("-e"),
            Flag("--email"),
            Value("--since"),
            Value("--until"),
        ],
        count: false,
        contents: false,
    },
    Sub {
        name: "describe",
        options: &[
            Flag("--tags"),
            Flag("--always"),
            Flag("--long"),
            Value("--abbrev"),
            Value("--match"),
        ],
        count: false,
        contents: false,
    },
];

/// The subcommands, for answers that name them.
fn names() -> String {
    SUBS.iter()
        .map(|sub| sub.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What one argument list asks: options checked, the positional words.
struct Parsed {
    sub: &'static Sub,
    /// The options with their values, in order.
    options: Vec<String>,
    positionals: Vec<String>,
    /// Positionals after `--`: paths.
    paths: Vec<String>,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let Some(first) = args.first() else {
        return Err(format!("`git` needs a subcommand; looks run {}.", names()));
    };
    if first.starts_with('-') {
        return Err(format!(
            "git options before the subcommand (`{first}`) are not looks: ask `git <subcommand> ...`."
        ));
    }
    let Some(sub) = SUBS.iter().find(|sub| sub.name == first) else {
        return Err(format!(
            "`git {first}` changes the repository or is not a look. Looks run {}.",
            names()
        ));
    };
    let mut parsed = Parsed {
        sub,
        options: Vec::new(),
        positionals: Vec::new(),
        paths: Vec::new(),
    };
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            parsed.paths.extend(rest.by_ref().cloned());
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            parsed.positionals.push(arg.clone());
            continue;
        }
        if sub.count && arg.len() > 1 && arg[1..].chars().all(|c| c.is_ascii_digit()) {
            parsed.options.push(arg.clone());
            continue;
        }
        let matched = sub.options.iter().find(|option| match **option {
            Flag(name) => arg == name,
            Value(name) => {
                arg == name
                    || if name.starts_with("--") {
                        arg.starts_with(&format!("{name}="))
                    } else {
                        arg.starts_with(name)
                    }
            }
        });
        match matched {
            Some(Value(name)) if arg == name => {
                // The value is the next word, whatever it looks like.
                let Some(value) = rest.next() else {
                    return Err(format!("`git {} {arg}` needs its value.", sub.name));
                };
                // A value git might read as another option is refused; a
                // grep pattern may start with `-`, which `-e` always takes.
                if value.starts_with('-') && *name != "-e" {
                    return Err(format!(
                        "`git {} {arg} {value}`: attach the value (`{arg}{value}` or `{arg}=...`).",
                        sub.name
                    ));
                }
                parsed.options.push(arg.clone());
                parsed.options.push(value.clone());
            }
            Some(_) => parsed.options.push(arg.clone()),
            None => {
                return Err(format!(
                "`git {} {arg}`: looks pass only plain read-only options, spelled in full ({}).",
                sub.name,
                sub.options
                    .iter()
                    .map(|option| match option {
                        Flag(name) | Value(name) => *name,
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
            }
        }
    }
    if sub.name == "branch"
        && !parsed.positionals.is_empty()
        && !args.iter().any(|arg| arg == "--list" || arg == "-l")
    {
        return Err(format!(
            "`git branch <name>` creates a branch. Looks list branches (`git branch --list`). {READ_ONLY}"
        ));
    }
    // Every `:`-separated piece is checked as a path: `rev:path`, `:0:path`
    // and `HEAD^{tree}:path` all name a file by what follows a colon.
    // grep's pattern, when no `-e` gives it, is the first positional and
    // names no file.
    let pattern = usize::from(
        sub.name == "grep" && !parsed.options.iter().any(|option| option.starts_with("-e")),
    );
    for word in parsed.positionals.iter().skip(pattern).chain(&parsed.paths) {
        for path in std::iter::once(word.as_str()).chain(word.split(':')) {
            if path.starts_with('/') || path.split('/').any(|part| part == "..") {
                return Err(format!(
                    "`{word}` reaches outside the project; git looks read the project's own history."
                ));
            }
            if protected_path(path) {
                return Err(format!(
                    "`{word}` names a path the harness keeps out of reach (secrets, its own state, other tools' state)."
                ));
            }
        }
    }
    Ok(parsed)
}

/// Classifies `git <args>`.
pub(super) fn classify(args: &[String]) -> Look {
    match parse(args) {
        Ok(_) => Look::Git {
            args: args.to_vec(),
        },
        Err(reason) => Look::Decline(reason),
    }
}

/// Runs git host-side with fixed hardening: no pager, external diff, text
/// converter or fsmonitor, no optional locks, no prompts.
fn git(root: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=",
            "-c",
            "core.pager=cat",
            "-c",
            "diff.external=",
            // A signature check runs gpg.program, which config can name.
            "-c",
            "log.showSignature=false",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_EDITOR", "true")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

/// The object type a revision names (`commit`, `tree`, `blob`, `tag`).
async fn object_type(root: &Path, revision: &str) -> Option<String> {
    let output = git(root)
        .args(["cat-file", "-t", revision])
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Runs a look `classify` accepted. Returns whether git succeeded, and its
/// output, or why it was not run.
pub async fn run_git(root: &Path, args: &[String]) -> Result<(bool, String), String> {
    let parsed = parse(args)?;
    let sub = parsed.sub;
    // Positionals that name a project path are paths; the rest are
    // revisions (or grep's pattern), and a revision must not be a blob.
    let mut revisions = Vec::new();
    let mut paths = parsed.paths.clone();
    let pattern_first =
        sub.name == "grep" && !parsed.options.iter().any(|option| option.starts_with("-e"));
    for (index, word) in parsed.positionals.iter().enumerate() {
        if (pattern_first && index == 0) || word.contains(':') {
            // grep's pattern; or `rev:path`, whose path `parse` checked.
            revisions.push(word.clone());
            continue;
        }
        if root.join(word).exists() {
            paths.push(word.clone());
            continue;
        }
        // Each side of a range (`a..b`, `a...b`) is a revision.
        for side in word.split("...").flat_map(|side| side.split("..")) {
            if side.is_empty() {
                continue;
            }
            if let Some(kind) = object_type(root, side).await {
                if !matches!(kind.as_str(), "commit" | "tag" | "tree") {
                    return Err(format!(
                        "`{side}` names a {kind}; git looks show commits, trees and project paths."
                    ));
                }
            }
        }
        revisions.push(word.clone());
    }
    let mut command = git(root);
    command.arg(sub.name);
    if matches!(sub.name, "diff" | "log" | "show" | "blame" | "grep") {
        command.arg("--no-textconv");
    }
    if matches!(sub.name, "diff" | "log" | "show") {
        command.arg("--no-ext-diff");
    }
    // The options, the revisions, then `--` and the paths, with the
    // protected names excluded wherever contents can show.
    command
        .args(&parsed.options)
        .args(&revisions)
        .arg("--")
        .args(&paths);
    if sub.contents && sub.name != "blame" {
        if paths.is_empty() {
            command.arg(".");
        }
        command.args(
            PROTECTED_GLOBS
                .iter()
                .map(|glob| format!(":(exclude,glob,icase){glob}")),
        );
    }
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| "git did not finish within 30 s".to_owned())?
        .map_err(|error| format!("git could not run: {error}"))?;
    const CAP: usize = 64 * 1024;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let errors = String::from_utf8_lossy(&output.stderr);
    if !errors.trim().is_empty() {
        text.push_str(&errors);
    }
    if text.len() > CAP {
        let mut end = CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[output truncated]");
    }
    Ok((output.status.success(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(command: &str) -> Vec<String> {
        command.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn only_listed_options_spelled_in_full_pass() {
        for allowed in [
            "log --oneline -5",
            "log -n 3 --format=%h",
            "log -p -- src/lib.rs",
            "diff --stat",
            "diff HEAD~1..HEAD",
            "show --stat HEAD",
            "blame -L 1,20 src/lib.rs",
            "grep -n -e TODO",
            "grep -n /usr/bin",
            "grep -n .env.example HEAD",
            "grep -c fn",
            "branch --list",
            "branch -vv",
            "status -s",
            "ls-files",
        ] {
            assert!(parse(&words(allowed)).is_ok(), "{allowed}");
        }
        for refused in [
            "grep -Otouch x",
            "grep --open-files-in-pager=touch x",
            "grep --open=touch x",
            "grep -iOtouch x",
            "grep --no-index -e KEY",
            "grep --untracked KEY",
            "grep --no-exclude-standard KEY",
            "diff --no-index /etc/hosts /dev/null",
            "diff --output=x",
            "diff --ext-diff",
            "log --ext-diff",
            "log --textconv",
            "blame --contents=/etc/hosts a.txt",
            "--git-dir=/other/.git log",
            "-C /tmp log",
            "-p log",
            "branch foo",
            "branch --del foo",
            "branch -qD foo",
            "branch -f x",
            "branch --edit-description",
            "show HEAD:.env",
            "log -p -- .env",
            "log -- ../other",
            "log -- /etc",
            "push",
            "checkout .",
            "config core.pager x",
            "-c core.pager=x log",
            "show HEAD:./.env",
            "show HEAD:.ENV",
            "show :0:.env",
            "show :/.env",
            "show HEAD^{tree}:.env",
            "show HEAD@{0}:.env",
            "show HEAD:.ssh/id",
            "rev-parse HEAD:.env",
            "log -n -Otouch",
            "grep -C -Otouch x",
            "grep -f patterns.txt",
            "log --show-signature",
            "log --output=x",
            "describe --dirty",
            "ls-files --others",
            "ls-files -o",
        ] {
            assert!(parse(&words(refused)).is_err(), "{refused}");
        }
    }

    #[tokio::test]
    async fn git_answers_from_history_without_the_protected_files() {
        let repo = std::env::temp_dir().join(format!("look-git-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        std::fs::write(repo.join(".env"), "API_KEY=secret-value\n").unwrap();
        git(&["add", "-f", "a.txt", ".env"]);
        git(&["commit", "-q", "-m", "first look"]);
        std::fs::write(repo.join("a.txt"), "two\n").unwrap();
        let run = |command: &str| {
            let repo = repo.clone();
            let args = words(command);
            async move { run_git(&repo, &args).await }
        };
        let (success, log) = run("log --oneline").await.unwrap();
        assert!(success && log.contains("first look"), "{log}");
        let (_, diff) = run("diff").await.unwrap();
        assert!(diff.contains("-one") && diff.contains("+two"), "{diff}");
        // Committed secrets stay out of every answer that shows contents.
        for command in [
            "log -p",
            "show HEAD",
            "grep -n API_KEY HEAD",
            "grep -n API_KEY",
        ] {
            let (_, out) = run(command).await.unwrap();
            assert!(!out.contains("secret-value"), "{command}: {out}");
        }
        // Normal use is unaffected by the exclusions.
        let (_, patch) = run("log -p -- a.txt").await.unwrap();
        assert!(patch.contains("+one"), "{patch}");
        let (_, grep) = run("grep -n one HEAD").await.unwrap();
        assert!(grep.contains("HEAD:a.txt:1:one"), "{grep}");
        let (_, blame) = run("blame -L 1,1 a.txt").await.unwrap();
        assert!(
            blame.contains("two") || blame.contains("Not Committed"),
            "{blame}"
        );
        let (_, staged) = run("diff HEAD -- a.txt").await.unwrap();
        assert!(staged.contains("+two"), "{staged}");
        let (_, pickaxe) = run("log -Ssecret-value --oneline").await.unwrap();
        assert!(!pickaxe.contains("first look"), "{pickaxe}");
        let (_, stat) = run("log --stat").await.unwrap();
        assert!(!stat.contains(".env"), "{stat}");
        // A blob's hash cannot print it.
        let blob = git(&["rev-parse", "HEAD:.env"]);
        let refused = run(&format!("show {blob}")).await.unwrap_err();
        assert!(refused.contains("names a blob"), "{refused}");
        let other = git(&["rev-parse", "HEAD:a.txt"]);
        for refused in [
            format!("diff {blob}..{other}"),
            format!("diff {blob} {other}"),
            format!("log {blob}"),
            format!("blame {blob} -- a.txt"),
        ] {
            assert!(run(&refused).await.is_err(), "{refused}");
        }
        // A peel after a path is part of the path's name to git.
        let (_, peeled) = run("show HEAD:.env^{blob}").await.unwrap_or_default();
        assert!(!peeled.contains("secret-value"), "{peeled}");
        let (_, tree) = run("show HEAD:").await.unwrap();
        assert!(tree.contains("a.txt"), "{tree}");
        std::fs::remove_dir_all(repo).unwrap();
    }
}

//! Which project an invocation belongs to.
//!
//! Every repository-scoped operation — indexing, minting, classification,
//! position resolution, LSP path mapping, the reindex scheduler — needs a repo
//! root, and configuration discovery needs one too. Deriving them separately is
//! how they come to disagree: anchoring `.env` to the project while indexing the
//! working directory lets a run from `project/src` load `project/.moosedev` and
//! publish a substrate covering only `project/src` into it. This module is the
//! single answer both sides ask.

use std::path::{Path, PathBuf};

/// Files whose presence marks a directory as a project root.
///
/// `.git` covers the normal case — and is a *file* in worktrees and submodules,
/// so this tests existence rather than file-ness; the manifests cover a checkout
/// without git, across every language the substrate producers support.
///
/// Deliberately NOT shared with those producers' language detection, which
/// answers "can I run an indexer over this directory" — TypeScript, for one,
/// demands `tsconfig.json` *and* `package.json` first. That is a stricter and
/// unrelated question from "where does this project keep its configuration".
pub const PROJECT_ROOT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
];

/// True when `dir` carries any [`PROJECT_ROOT_MARKERS`] entry.
pub fn is_project_root(dir: &Path) -> bool {
    PROJECT_ROOT_MARKERS
        .iter()
        .any(|marker| dir.join(marker).exists())
}

/// The nearest ancestor of `start` (including itself) that looks like a project
/// root, if any.
pub fn project_root_from(start: &Path) -> Option<&Path> {
    start.ancestors().find(|dir| is_project_root(dir))
}

/// The project this invocation belongs to: the working directory's nearest
/// project root, else the working directory itself.
pub fn project_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match project_root_from(&cwd) {
        Some(root) => root.to_path_buf(),
        None => cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "moosedev-project-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn every_supported_language_marks_a_root() {
        for marker in PROJECT_ROOT_MARKERS {
            let root = scratch("markers");
            std::fs::write(root.join(marker), "").expect("write marker");
            assert!(is_project_root(&root), "{marker} should mark a root");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn nested_directories_resolve_to_the_enclosing_root() {
        let root = scratch("nested");
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(root.join("pyproject.toml"), "").expect("write marker");

        assert_eq!(project_root_from(&nested), Some(root.as_path()));
        // The root is its own answer, not its parent's.
        assert_eq!(project_root_from(&root), Some(root.as_path()));

        let _ = std::fs::remove_dir_all(&root);
    }
}

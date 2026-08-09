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

/// Language manifests that mark a project root when there is no repository.
///
/// A FALLBACK only — see [`project_root_from`]. Deliberately NOT shared with the
/// substrate producers' language detection, which answers "can I run an indexer
/// over this directory" — TypeScript, for one, demands `tsconfig.json` *and*
/// `package.json` first. That is a stricter and unrelated question from "where
/// does this project keep its configuration".
pub const PROJECT_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
];

/// True when `dir` is a git root. `.git` is a *file* in worktrees and
/// submodules, so this tests existence rather than file-ness.
pub fn is_git_root(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// True when `dir` carries a language manifest.
pub fn has_project_manifest(dir: &Path) -> bool {
    PROJECT_MANIFESTS
        .iter()
        .any(|marker| dir.join(marker).exists())
}

/// True when `dir` could be a project root by either test.
pub fn is_project_root(dir: &Path) -> bool {
    is_git_root(dir) || has_project_manifest(dir)
}

/// The nearest ancestor of `start` (including itself) that is a project root.
///
/// THE REPOSITORY WINS. A repository is one project even when it contains many
/// package manifests, so the git root is searched first and manifests are only
/// consulted when there is no `.git` anywhere above — a tarball, a vendored
/// subtree. Taking the nearest marker of either kind instead made every nested
/// manifest its own project: in this repo alone `ui/`, `bench/`,
/// `clients/vscode/`, and `clients/zed/` each became a separate root, so a
/// command run from one of them skipped the repository's `.env`, opened a store
/// under that subdirectory, and indexed only that subtree — the split store this
/// whole notion of a project root exists to prevent.
pub fn project_root_from(start: &Path) -> Option<&Path> {
    start
        .ancestors()
        .find(|dir| is_git_root(dir))
        .or_else(|| start.ancestors().find(|dir| has_project_manifest(dir)))
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
        for marker in PROJECT_MANIFESTS.iter().chain([".git"].iter()) {
            let root = scratch("markers");
            std::fs::write(root.join(marker), "").expect("write marker");
            assert!(is_project_root(&root), "{marker} should mark a root");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// A repository is ONE project. Nested package manifests — `ui/package.json`,
    /// `bench/requirements.txt`, `clients/*` — must not each become their own
    /// root, or a command run from one skips the repository's `.env` and opens a
    /// store under that subdirectory.
    #[test]
    fn a_nested_manifest_never_overrides_the_enclosing_repository() {
        let root = scratch("nested-manifest");
        std::fs::write(root.join(".git"), "gitdir: elsewhere").expect("git marker");
        let ui = root.join("ui");
        std::fs::create_dir_all(ui.join("src")).expect("create ui/src");
        std::fs::write(ui.join("package.json"), "{}").expect("nested manifest");

        assert_eq!(project_root_from(&ui), Some(root.as_path()));
        assert_eq!(project_root_from(&ui.join("src")), Some(root.as_path()));
        assert_eq!(project_root_from(&root), Some(root.as_path()));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Outside a repository the manifests are still what marks a project, so a
    /// tarball or vendored subtree is not left rootless.
    #[test]
    fn manifests_still_apply_when_there_is_no_repository() {
        let root = scratch("no-git");
        let nested = root.join("pkg");
        std::fs::create_dir_all(nested.join("src")).expect("create pkg/src");
        std::fs::write(nested.join("Cargo.toml"), "").expect("manifest");

        assert_eq!(
            project_root_from(&nested.join("src")),
            Some(nested.as_path())
        );

        let _ = std::fs::remove_dir_all(&root);
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

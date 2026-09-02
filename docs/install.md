# Installing MOOSEDev

MOOSEDev is distributed as a **pre-built binary**. The core MOOSE engine is
closed-source, so building from source isn't an option for most users. It also
isn't needed: every release ships a self-contained binary bundled with its
`ontologies/`, `skills/`, `templates/`, and the Arctic-Embed-S embedding weights
(`models/`), all resolved relative to the executable. Nothing is downloaded on
first run and embeddings are computed locally, so MOOSEDev works fully offline.

**Supported platforms:** macOS (Apple Silicon / arm64) and Linux (x86-64). Other
targets aren't built yet; the installer errors clearly rather than installing the
wrong artifact.

## Option A: install script

```sh
curl -fsSL https://raw.githubusercontent.com/Trivyn/moosedev/main/scripts/install.sh | sh
```

It detects your OS/arch, downloads the matching release tarball from GitHub,
verifies its SHA-256 checksum, unpacks it, and symlinks the `moosedev` binary
onto your PATH.

Environment overrides:

| Variable | Default | Purpose |
| --- | --- | --- |
| `MOOSEDEV_VERSION` | latest release | Install a specific version (`0.4.0` or `v0.4.0`). |
| `MOOSEDEV_INSTALL_DIR` | `$HOME/.local/share/moosedev` | Where versioned installs live. |
| `BIN_DIR` | `$HOME/.local/bin` | Where the `moosedev` symlink is created. |

If `BIN_DIR` isn't already on your `PATH`, the script tells you the line to add
to your shell profile (e.g. `export PATH="$HOME/.local/bin:$PATH"`).

## Option B: Homebrew

```sh
brew install Trivyn/moosedev/moosedev
```

This taps `Trivyn/homebrew-moosedev` and installs the binary formula. Upgrades
come through `brew upgrade` like any other formula.

## macOS: no notarization prompt

The binary is unsigned by Apple's Developer ID program and **not notarized**, yet
neither install path triggers Gatekeeper's "unidentified developer" block:

- Gatekeeper only quarantines files carrying the `com.apple.quarantine`
  attribute, which browsers set but `curl` (and Homebrew's downloader) do not,
  so the downloaded binary isn't quarantined and runs directly.
- On Apple Silicon the binary still needs *a* signature to execute at all; the
  Rust toolchain applies an **ad-hoc** signature during the native build, which
  satisfies the kernel.

If you download a release tarball manually through a web browser instead, macOS
*will* quarantine it; clear it with `xattr -dr com.apple.quarantine <dir>`.

## Verify

```sh
moosedev --help      # usage
moosedev --status    # backend + web UI status for the current data dir
```

Then configure a project with [`moosedev init`](quickstart.md). The quickstart
covers project setup and enabling the code layer + editor integration
(`index`/`mint`, Knowledge-LSP clients).

## Build from source

Source builds require Rust 1.89 or newer, Node.js 20, and access to the private MOOSE engine.
Check out MOOSE as a sibling at `../moose`; MOOSEDev uses it as a path dependency
and shares its patched `oxigraph` fork. The release workflow currently pins MOOSE
commit `2f88dd5e4cd9615b77f0fbdf43654437082e6e09` (`v0.9.2`). Use that revision to
reproduce release builds.

```sh
git clone https://github.com/Trivyn/moosedev.git
cd moosedev
# Check out the private MOOSE repository at ../moose and use the pinned revision.
scripts/build-release.sh
```

The script installs the UI dependencies, builds `ui/dist/`, and then runs
`cargo build --release --locked`. The default `embedded-frontend` feature embeds
the generated UI in the Rust binary. `ui/dist/` is generated and is not tracked
in Git.

For a backend-only build that does not need `ui/dist/`, select the headless
feature explicitly:

```sh
cargo build --release --locked --no-default-features --features headless
```

Normal and headless builds use the CPU Arctic-Embed-S backend. Model weights are
not compiled into the binary. Release bundles include them at
`models/snowflake-arctic-embed-s/` beside the executable, so installed releases
do not download a model on first use. A source build downloads the weights from
Hugging Face when no bundle is available. To stay offline or share one copy
across checkouts, set `MOOSEDEV_MODEL_DIR` to a directory that contains
`models/snowflake-arctic-embed-s/`.

## Upgrade

- **Script:** re-run it (optionally with `MOOSEDEV_VERSION`); the symlink is
  repointed to the new version. Because generated `.mcp.json` configs use the bare
  `moosedev` command on your PATH, they keep working across upgrades.
- **Homebrew:** `brew upgrade moosedev`.

## Uninstall

- **Script:** `rm ~/.local/bin/moosedev` and `rm -rf ~/.local/share/moosedev`
  (adjust for custom `BIN_DIR` / `MOOSEDEV_INSTALL_DIR`).
- **Homebrew:** `brew uninstall moosedev` (and `brew untap Trivyn/moosedev`).

Per-project files (`.mcp.json`, `.gitignore`, `CLAUDE.md`, `.moosedev/`) are left
untouched; remove them from a project by hand if you no longer want its memory.

---

## Maintainer: standing up the Homebrew tap

The `brew install` path requires a one-time setup of the tap repository. This is
for MOOSEDev maintainers, not users.

1. Create a public repo **`Trivyn/homebrew-moosedev`** with a `Formula/`
   directory. Seed it with the current formula:

   ```sh
   # from a moosedev checkout
   cp packaging/homebrew/moosedev.rb /path/to/homebrew-moosedev/Formula/moosedev.rb
   ```

2. Create a token that can push to that repo (a fine-grained PAT with
   *Contents: read/write* on `Trivyn/homebrew-moosedev`, or a classic `repo`
   token) and add it to the **`moosedev`** repo's Actions secrets as
   **`HOMEBREW_TAP_TOKEN`**.

Once the secret exists, the `homebrew` job in
[`.github/workflows/release.yml`](../.github/workflows/release.yml) regenerates
the formula (via `packaging/homebrew/render-formula.sh`) with each release's
version and checksums and pushes it to the tap. Without the secret the job skips
cleanly, so it never blocks a release. The formula text has a single source of
truth. Edit `render-formula.sh`, not the generated `moosedev.rb`.

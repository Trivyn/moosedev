# Installing MOOSEDev

MOOSEDev is distributed as a **pre-built binary**. The core MOOSE engine is
closed-source, so building from source isn't an option for most users — but it
also isn't needed: every release ships a self-contained binary bundled with its
`ontologies/`, `skills/`, and `templates/`, resolved relative to the executable.

**Supported platforms:** macOS (Apple Silicon / arm64) and Linux (x86-64). Other
targets aren't built yet; the installer errors clearly rather than installing the
wrong artifact.

## Option A — install script

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

## Option B — Homebrew

```sh
brew install Trivyn/moosedev/moosedev
```

This taps `Trivyn/homebrew-moosedev` and installs the binary formula. Upgrades
come through `brew upgrade` like any other formula.

## macOS: no notarization prompt

The binary is unsigned by Apple's Developer ID program and **not notarized**, yet
neither install path triggers Gatekeeper's "unidentified developer" block:

- Gatekeeper only quarantines files carrying the `com.apple.quarantine`
  attribute, which browsers set but `curl` (and Homebrew's downloader) do not —
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

Then configure a project with [`moosedev init`](quickstart.md) — the quickstart
covers project setup and enabling the code layer + editor integration
(`index`/`mint`, Knowledge-LSP clients).

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
truth — edit `render-formula.sh`, not the generated `moosedev.rb`.

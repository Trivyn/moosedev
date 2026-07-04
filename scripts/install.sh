#!/bin/sh
# MOOSEDev installer — download a pre-built release binary and put `moosedev` on
# your PATH. No compilation: the MOOSE engine is closed-source, so releases ship
# a self-contained binary bundled with its ontologies/skills/templates. This
# script just fetches that tarball, verifies its checksum, and links the binary.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Trivyn/moosedev/main/scripts/install.sh | sh
#
# Environment overrides:
#   MOOSEDEV_VERSION       release tag to install (default: latest)
#   MOOSEDEV_INSTALL_DIR   install prefix   (default: $HOME/.local/share/moosedev)
#   BIN_DIR                symlink location (default: $HOME/.local/bin)
#
# Supported platforms: macOS (Apple Silicon), Linux (x86_64). Other targets are
# not yet built — the script errors clearly rather than installing the wrong one.

set -eu

REPO="Trivyn/moosedev"
INSTALL_DIR="${MOOSEDEV_INSTALL_DIR:-$HOME/.local/share/moosedev}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

info() { printf '\033[1;34m==>\033[0m %s\n' "$1" >&2; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
err()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

command -v uname >/dev/null 2>&1 || err "required tool not found: uname"
command -v tar   >/dev/null 2>&1 || err "required tool not found: tar"

# Prefer curl, fall back to wget. dl STREAMS to stdout; dl_to writes URL -> FILE.
if command -v curl >/dev/null 2>&1; then
  dl()    { curl -fsSL "$1"; }
  dl_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  dl()    { wget -qO- "$1"; }
  dl_to() { wget -qO "$2" "$1"; }
else
  err "need curl or wget to download"
fi

# --- resolve target triple -------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64 | aarch64) target="aarch64-apple-darwin" ;;
      *) err "unsupported macOS arch: $arch (only Apple Silicon has prebuilt binaries)" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
      *) err "unsupported Linux arch: $arch (only x86_64 has prebuilt binaries)" ;;
    esac ;;
  *) err "unsupported OS: $os (prebuilt binaries exist for macOS arm64 and Linux x86_64)" ;;
esac

# --- resolve version -------------------------------------------------------
version="${MOOSEDEV_VERSION:-}"
if [ -z "$version" ]; then
  info "Resolving latest release"
  version="$(dl "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' | head -1 \
    | sed -E 's/.*"tag_name" *: *"([^"]+)".*/\1/')"
  [ -n "$version" ] || err "could not resolve the latest release version"
fi
# Accept both "0.4.0" and "v0.4.0".
case "$version" in
  v*) tag="$version";  ver="${version#v}" ;;
  *)  tag="v$version"; ver="$version" ;;
esac

name="moosedev-${tag}-${target}"
url="https://github.com/$REPO/releases/download/$tag/${name}.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

info "Downloading $name"
dl_to "$url" "$tmp/${name}.tar.gz" || err "download failed: $url"

# --- verify checksum (if the sidecar asset exists) -------------------------
if dl_to "${url}.sha256" "$tmp/${name}.tar.gz.sha256" 2>/dev/null; then
  expected="$(awk '{print $1}' "$tmp/${name}.tar.gz.sha256")"
  if command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/${name}.tar.gz" | awk '{print $1}')"
  elif command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/${name}.tar.gz" | awk '{print $1}')"
  else
    actual=""
    warn "no shasum/sha256sum available; skipping checksum verification"
  fi
  if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
    err "checksum mismatch: expected $expected, got $actual"
  fi
  [ -n "$actual" ] && info "Checksum verified"
else
  warn "no .sha256 asset for $tag; skipping checksum verification"
fi

# --- install ---------------------------------------------------------------
info "Extracting"
tar -xzf "$tmp/${name}.tar.gz" -C "$tmp"

dest="$INSTALL_DIR/$ver"
rm -rf "$dest"
mkdir -p "$dest"
# The tarball holds a single top dir named "$name"; install its contents so the
# binary keeps ontologies/skills/templates as siblings (exe-relative resolution).
cp -R "$tmp/$name/." "$dest/"
[ -f "$dest/moosedev" ] || err "binary not found in tarball ($name)"
chmod +x "$dest/moosedev"

mkdir -p "$BIN_DIR"
ln -sf "$dest/moosedev" "$BIN_DIR/moosedev"

# Sanity-check that the binary actually runs on this machine (catches an
# arch/signature mismatch before the user hits it).
if ! "$dest/moosedev" --help >/dev/null 2>&1; then
  warn "the installed binary did not run cleanly — check platform compatibility"
fi

info "Installed moosedev $ver -> $dest"
info "Linked $BIN_DIR/moosedev"

case ":$PATH:" in
  *":$BIN_DIR:"*) : ;;
  *)
    warn "$BIN_DIR is not on your PATH. Add it, e.g.:"
    # $PATH is intentionally literal — the user's shell expands it when they run this.
    # shellcheck disable=SC2016
    printf '  export PATH="%s:$PATH"\n' "$BIN_DIR" >&2 ;;
esac

cat >&2 <<'EOF'

Next: in your project directory, run
  moosedev init
to wire up the MCP config and project memory, then reload your MCP client.
EOF

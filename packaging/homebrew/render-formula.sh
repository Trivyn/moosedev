#!/bin/sh
# Emit the Homebrew formula for MOOSEDev to stdout — the single source of truth
# for the formula text. `moosedev.rb` (committed) and the tap copy pushed by
# .github/workflows/release.yml are both produced from here, so edit this script,
# never the generated output.
#
# Usage: render-formula.sh <version> <macos_arm64_sha256> <linux_x86_64_sha256>
#   version: release version WITHOUT the leading "v" (e.g. 0.4.0)
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <version> <macos_arm64_sha256> <linux_x86_64_sha256>" >&2
  exit 2
fi

version="$1"
macos_sha="$2"
linux_sha="$3"
base="https://github.com/Trivyn/moosedev/releases/download/v${version}"

cat <<EOF
# MOOSEDev — neurosymbolic MCP memory server for coding agents.
#
# Binary formula: the MOOSE engine is closed-source, so this downloads the
# pre-built release tarball rather than compiling from source. It lives in a
# custom tap (Trivyn/homebrew-moosedev), not homebrew/core. Regenerated on each
# release — edit packaging/homebrew/render-formula.sh, not this output.
class Moosedev < Formula
  desc "Neurosymbolic MCP server giving coding agents structured long-term memory"
  homepage "https://github.com/Trivyn/moosedev"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "${base}/moosedev-v${version}-aarch64-apple-darwin.tar.gz"
      sha256 "${macos_sha}"
    end
  end

  on_linux do
    on_intel do
      url "${base}/moosedev-v${version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${linux_sha}"
    end
  end

  def install
    # Keep the binary's companion resources (ontologies/, skills/, templates/)
    # beside it so the exe-relative resolution finds them at runtime.
    libexec.install Dir["*"]
    bin.install_symlink libexec/"moosedev"
  end

  test do
    assert_match "MCP", shell_output("#{bin}/moosedev --help")
  end
end
EOF

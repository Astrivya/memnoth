# memnoth — Homebrew formula (release-template draft).
#
# CHANGE_ON_RELEASE: every `sha256` below is a deliberately non-hex placeholder.
# No release archive digest is committed in this repository. The published
# manifest is generated at release time (`docs/operations/install-manifest.md`
# §2) and the winget template carries the same unfilled placeholder
# (`CLI/packaging/winget/memnoth.yaml:53`). Replace each placeholder with the
# matching `targets[].sha256` from
#   https://github.com/astrivya/memnoth/releases/download/v0.1.1/manifest.json
# before opening a tap PR. A placeholder that reaches a user is a hard failure,
# not a soft warning: `brew` rejects a non-64-hex digest.
#
# URLs and version are real: 0.1.1 is the published stable version, and the
# five target archive names come from `CLI/.github/workflows/release.yml:82-99`
# and `CLI/scripts/dist.sh:6-7`. The archives contain `memnoth` and
# `memnoth-mcp` at their root (`release.yml:396`).
#
# Publishing: keep this in a tap such as `astrivya/tap`; do not submit to
# homebrew/core until the project meets core's criteria. Verify with
# `brew audit --strict --online` and `brew install --build-from-source` in a
# disposable container before tagging the tap release.

class Memnoth < Formula
  desc "Local-first terminal agent with a bi-temporal vault and MCP server"
  homepage "https://memnoth.astrivya.ai"
  version "0.1.1"
  license "Apache-2.0"

  on_macos do
    on_intel do
      url "https://github.com/astrivya/memnoth/releases/download/v0.1.1/memnoth-x86_64-apple-darwin.tar.gz"
      # CHANGE_ON_RELEASE: sha256 of x86_64-apple-darwin from the release manifest.
      sha256 "CHANGE_ON_RELEASE_X86_64_APPLE_DARWIN"
    end
    on_arm do
      url "https://github.com/astrivya/memnoth/releases/download/v0.1.1/memnoth-aarch64-apple-darwin.tar.gz"
      # CHANGE_ON_RELEASE: sha256 of aarch64-apple-darwin from the release manifest.
      sha256 "CHANGE_ON_RELEASE_AARCH64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/astrivya/memnoth/releases/download/v0.1.1/memnoth-x86_64-unknown-linux-gnu.tar.gz"
      # CHANGE_ON_RELEASE: sha256 of x86_64-unknown-linux-gnu from the release manifest.
      sha256 "CHANGE_ON_RELEASE_X86_64_LINUX_GNU"
    end
    on_arm do
      url "https://github.com/astrivya/memnoth/releases/download/v0.1.1/memnoth-aarch64-unknown-linux-gnu.tar.gz"
      # CHANGE_ON_RELEASE: sha256 of aarch64-unknown-linux-gnu from the release manifest.
      sha256 "CHANGE_ON_RELEASE_AARCH64_LINUX_GNU"
    end
  end

  def install
    bin.install "memnoth"
    bin.install "memnoth-mcp" if File.exist?("memnoth-mcp")
    # Completions are intentionally not generated here: `mn completion` needs
    # a shell argument, and a guessed invocation would be a second source of
    # truth. Add it when the CLI exposes a completion writer with a fixed CLI.
  end

  test do
    assert_match(/\d+\.\d+\.\d+/, shell_output("#{bin}/memnoth version"))
  end
end

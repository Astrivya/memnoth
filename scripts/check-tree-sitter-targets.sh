#!/usr/bin/env bash
# Verify that every tree-sitter crate used by memnoth-codegraph compiles for a
# single release target. The eight crates below are the tree-sitter runtime
# plus the seven grammars declared in crates/memnoth-codegraph/Cargo.toml;
# they are C/C++ sources built by each crate's build.rs, so a workspace
# `cargo check` only ever exercises the host C compiler. This script checks
# each crate directly against one target, using `cross` for the target that
# cannot build on the host (aarch64-unknown-linux-gnu).
#
# CI calls this once per release target; the target list and runner mapping
# live in the `tree-sitter-targets` matrix in .github/workflows/ci.yml and
# mirror the release build matrix in .github/workflows/release.yml. Keep the
# CRATES array here in sync with memnoth-codegraph's manifest.
#
# Usage:  TS_USE_CROSS=0|1 bash scripts/check-tree-sitter-targets.sh <triple>
set -euo pipefail

TARGET="${1:-}"
if [[ -z "$TARGET" ]]; then
  echo "usage: $0 <target-triple>" >&2
  exit 2
fi

CRATES=(
  tree-sitter          # runtime
  tree-sitter-rust
  tree-sitter-python
  tree-sitter-typescript
  tree-sitter-go
  tree-sitter-java
  tree-sitter-cpp
  tree-sitter-ruby
)

case "${TS_USE_CROSS:-0}" in
  1 | true | TRUE | yes | YES) DRIVER=cross ;;
  *) DRIVER=cargo ;;
esac

echo "tree-sitter target check: ${#CRATES[@]} crates -> ${TARGET} (driver: ${DRIVER})"

# Collect every failure instead of stopping at the first so one run reports
# all crates that break on the target. Compiler diagnostics stay in the log;
# the crate names are mirrored into the step summary for a loud, scannable
# failure.
failed=()
for crate in "${CRATES[@]}"; do
  echo "::group::${DRIVER} check -p ${crate} --target ${TARGET}"
  if "$DRIVER" check --locked -p "$crate" --target "$TARGET"; then
    :
  else
    failed+=("$crate")
    echo "::error::${crate} failed to check for ${TARGET}"
  fi
  echo "::endgroup::"
done

if ((${#failed[@]})); then
  {
    echo "### tree-sitter target check FAILED for \`${TARGET}\`"
    echo
    echo "These crates did not compile for the target:"
    printf -- '- `%s`\n' "${failed[@]}"
  } >>"${GITHUB_STEP_SUMMARY:-/dev/stderr}"
  printf 'tree-sitter crates failed for %s: %s\n' "$TARGET" "${failed[*]}" >&2
  exit 1
fi

echo "OK: all ${#CRATES[@]} tree-sitter crates check cleanly for ${TARGET}"

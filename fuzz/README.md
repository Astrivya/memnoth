# cargo-fuzz targets for memnoth

This crate holds libFuzzer targets for the protocol-adjacent code that parses
untrusted bytes. Targets live in `fuzz_targets/` and are declared as `[[bin]]`
entries in `Cargo.toml` with `test = false` — their `main` comes from
`fuzz_target!`, so a libtest harness cannot link them.

## Prerequisites

    cargo install cargo-fuzz
    rustup toolchain install nightly

cargo-fuzz selects nightly itself for the fuzz build; `CLI/rust-toolchain.toml`
pins `stable` for the workspace and does not need to change.

## Run a target

From `CLI/fuzz/` (or add `--manifest-path fuzz/Cargo.toml` from `CLI/`):

    cargo fuzz list
    cargo fuzz run config
    cargo fuzz run mcp_framing
    cargo fuzz run diff_parsing
    cargo fuzz run provider_url
    cargo fuzz run sync_manifest
    cargo fuzz run frontmatter

Extra targets also present: `security`, `child_event`, `install_manifest`.

Short one-seed replay instead of a campaign:

    cargo fuzz run config -- -runs=1 corpus/config/real_default.json

To reduce and inspect a crash:

    cargo fuzz tmin <target> <crash-file>
    cargo fuzz fmt <target> <crash-file>

## Deterministic seeds

Every target is a pure function of its input bytes: no clock, no randomness,
no network, and no filesystem access. The same bytes always take the same
path. libFuzzer's own PRNG is pinned with `-seed`:

    cargo fuzz run config -- -seed=1 -runs=10000

Each file in `corpus/<target>/` is an input seed that starts its own replay
branch. The corpus directories contain three kinds of seed:

- `seed.*` — the original minimal smoke seed;
- `real_*` — a shape copied from a real source file or type definition;
- `adv_*` — adversarial/edge-case input (malformed JSON, wrong field types,
  integer overflow, SSRF hosts, unterminated delimiters, empty input).

A `real_*` seed being rejected is still useful: reject paths are part of the
contract, and fuzzing exercises both accepted and refused inputs.

## A crash is a bug, not a flake

These surfaces are meant to be total: arbitrary bytes must return a verdict,
never panic, abort, overflow, or hang. If a target fails, treat it as a
reproducible defect — do not rerun it until it goes away. Minimize with
`cargo fuzz tmin`, add the reduced bytes to `corpus/<target>/` as a regression
seed, and fix the underlying parser.

libFuzzer writes new corpus units to `fuzz/corpus/<target>/` and crash
artifacts to `fuzz/artifacts/<target>/`. Both are inside this directory; no
target writes anywhere else.

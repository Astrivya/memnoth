# G17 reproduction kit

A self-contained kit that reproduces a **recorded, deterministic benchmark
artifact byte-for-byte** and refuses to run when the environment differs from
the pin.

> **Read this first.** This kit exercises the model-free fixture lane
> (`mn bench trs-verify`). It is a **fixture diagnostic**, not an official
> benchmark score, not a model run, and not an external or third-party
> reproduction. The model/effort/seed fields describe the configuration of the
> official lane this fixture stands in for; the fixture itself performs no
> inference and no network access. When the publisher runs the kit, the report
> says `NOT a third-party claim` explicitly.

## What is pinned

`manifest.json` records everything needed to decide whether a run is the run
that was published:

| Pin | Value |
|---|---|
| commit | `b1c463a3283491b6d2e753606992205008bfba33` |
| dataset | `ci/trs-cases.json` @ sha256 `3f67ebcbb2c053bcb2e453705018974b0d3414a3a7ebc42a274b45449da844ef` |
| model / effort | `claude-sonnet-4-5` / none (fixture is model-free) |
| temperature / max_output | `0` / `8192` |
| trials (N) | `3` |
| seeds | `1, 2, 3` (paired; official default seed `20261110`) |
| toolchain | rustc `1.98.0`, cargo `1.98.0` |
| recorded artifact | `run-receipt.json` @ sha256 pinned in the manifest |

The manifest bytes themselves are pinned by a sha256 constant inside both
scripts, so editing the pin file without updating the scripts is detected and
refused.

## Requirements

- Bash 4+, git, `awk`, `sed`, `cmp`, `date`, `mktemp`
- `sha256sum` (GNU) or `shasum -a 256` (macOS)
- Rust toolchain `1.98.0` (from `rust-toolchain.toml`), i.e. `rustc` and `cargo`
  on `PATH` (or under `$HOME/.cargo/bin`)
- The repository checked out at the pinned commit

No API key, no network, no model is used.

## Run

```bash
git clone <repo-url> memnoth && cd memnoth/CLI
git checkout b1c463a3283491b6d2e753606992205008bfba33
bash scripts/repro/reproduce.sh
```

The script prints a dated report with one `ok`/`FAIL` line per check and a final
`RESULT: PASS/FAIL`. Exit status is `0` on pass, `1` on refusal or failure.
Set `REPRO_RUN_BY_AUTHOR=1` to explicitly mark the run as the publisher's own;
otherwise the author is detected by comparing `git config user.email` with the
HEAD commit's author email.

## What it checks

1. **Manifest integrity** — the pin file still hashes to the committed value.
2. **Required tools** — every tool the verification needs is present.
3. **Commit pin** — `git rev-parse HEAD` equals the pinned commit.
4. **Dataset sha256** — `ci/trs-cases.json` hashes to the pinned digest.
5. **Toolchain pin** — `rustc --version` and `cargo --version` equal `1.98.0`.
6. **Recorded artifact** — the receipt regenerated from the pins is checked
   against the recorded sha256 **and** compared to the recorded bytes with
   `cmp` (a literal byte-for-byte comparison).

Any difference in steps 1–5 is a refusal (`REFUSED: environment differs ...`)
with no report produced — the kit will not attest to an environment it cannot
pin.

## For an official run (re-record protocol)

This kit ships a fixture. To publish an official number, do **not** edit these
files by hand:

1. Run the real harness at the pinned commit (e.g.
   `mn bench run --suite trs --n 3 --seed 20261110 --json -o ci/trs-result.json`)
   with the model/effort recorded in the manifest, on a clean checkout.
2. Put the resulting JSON under `scripts/repro/`, add its sha256 and exact
   canonical bytes to `manifest.json`, and update the `MANIFEST_SHA256`
   constant in both scripts to the new manifest digest.
3. Re-run `bash scripts/repro/reproduce.sh` and commit the updated pins and
   artifact together. A number may be published only from that committed JSON,
   never typed into marketing, README or release notes.

Until that is done, the only claim this kit supports is: *the recorded fixture
artifact reproduces byte-for-byte from the pinned environment.* It does not
support a score, a ranking, or a third-party reproduction.

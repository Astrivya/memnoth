//! KG-01/02/03 + Gate G13 acceptance plan — precision, LSP, and fixture gate.
//!
//! This file is a design scaffold, NOT a passing acceptance. It does four
//! things and nothing else:
//!
//! 1. Inventories the LSP/precision machinery that already exists in source,
//!    with exact `file:line` (section 1).
//! 2. Specifies what a real fixture repo must contain for the G13 comparison
//!    (section 2).
//! 3. Names the exact external tool/crate needed, its build cost on the five
//!    release targets, and the honest interim precision claim (sections 3-4).
//! 4. Declares ignored tests named for every missing case (section 5). Each
//!    one panics loudly with "NOT RUN" if invoked with `--ignored`, so a future
//!    run cannot silently pass by treating the scaffold as evidence.
//!
//! Everything here is `#[ignore]`d. The file compiles under
//! `cargo test --workspace` and contributes zero executed assertions. No file
//! outside this scaffold is edited by this change.

//! ## 1. Inventory — what exists today
//!
//! ### 1a. Precision provenance (the wire contract already exists)
//! - `crates/memnoth-codegraph/src/precision.rs:34-48` — `Precision` enum with
//!   exactly the three wire labels: `lexical`, `tree-sitter`, `lsp`.
//! - `precision.rs:59-65` `label()`, `:67-75` `parse()`, `:82-98` edge
//!   confidence (0.90 / 0.85 / 0.99) and `LIKELY_EDGE_FACTOR = 0.5`.
//! - `precision.rs:115-178` — `Capabilities`; `capabilities()` returns
//!   `{ lexical: true, tree_sitter: true, lsp: false }`, so `best_available()`
//!   is `TreeSitter`. No build here can honestly claim `lsp`.
//! - `precision.rs:184-192` — `GraphAnswer` trait every answer implements.
//!
//! ### 1b. Auto-start policy and position encoding (types only)
//! - `crates/memnoth-codegraph/src/lsp.rs:57-138` — `SERVER_TABLE` of ten
//!   servers; the Rust row is `rust-analyzer` at `:58-65` with root marker
//!   `Cargo.toml`.
//! - `lsp.rs:211-256` — `AutoStartPolicy`; `default()` is `auto_lsp: false`
//!   and `should_start()` requires four gates (opt-in, extension, allowlist,
//!   root marker). This is the `auto_lsp=false` escape, already specified.
//! - `lsp.rs:258-310` — `PositionEncoding::{Utf8,Utf16}`,
//!   `from_capabilities()`, `to_character()` / `from_character()`. The UTF-8
//!   byte <-> UTF-16 unit conversion helpers exist and are unit-tested at
//!   `:399-417` with a non-ASCII `let 😀 = 'é';` line.
//! - `lsp.rs:312-344` — `encode_frame` / `decode_frame` (`Content-Length`).
//!
//! ### 1c. LSP client crate (spawns a process, but incomplete)
//! - `crates/memnoth-lsp/Cargo.toml:9-14` — only anyhow, memnoth-codegraph,
//!   serde, serde_json, tokio. No heavy dependency; no new crate is needed to
//!   *drive* a server.
//! - `crates/memnoth-lsp/src/client.rs:47-61` — `start()` consults the policy.
//! - `client.rs:63-112` — `spawn()` starts the child and reads stdout on a
//!   thread. **No `initialize` / `initialized` handshake is ever sent.**
//! - `client.rs:27` — `encoding: Mutex<Option<PositionEncoding>>` is declared
//!   but never assigned from `ServerCapabilities.position_encoding`
//!   (`protocol.rs:348-369`). **No position-encoding negotiation is wired.**
//! - `client.rs:116-141` — `request()` uses a 30 s timeout, not the 2 s soft /
//!   10 s hard policy in `plans/10a-deep-design.md:1068-1071`.
//! - `client.rs:264-288` — `did_open()` exists; `did_change()` hard-codes
//!   `contentChanges: [{"text": ""}]` and takes **no text argument**, so it
//!   wipes the document. **Diagnostics-after-edit cannot be correct against a
//!   real server** until this carries the edited text.
//! - `client.rs:203-218` — `wait_for_diagnostics()` polls for up to a timeout;
//!   `client.rs:157-190` stores `publishDiagnostics` by URI.
//! - `crates/memnoth-lsp/src/server.rs:98-152` — the in-process `LspServer`
//!   answers definition/references/hover/rename from a `Store`, but
//!   `server.rs:350-352` `diagnostics()` always returns an empty vec (no engine).
//!   `server.rs:382-429` `offset_at`/`position_at` count Unicode **chars**
//!   (neither UTF-8 bytes nor UTF-16 units), inconsistent with
//!   `PositionEncoding::to_character`.
//! - `crates/memnoth-lsp/src/apply.rs:19-70` — safe `WorkspaceEdit` apply.
//!
//! ### 1d. CLI integration and the honesty gap
//! - `src/lsp_diag.rs:22-36` — `after_write()` starts a server only under
//!   opt-in, opens the doc, waits 2 s, kills the process.
//! - `lsp_diag.rs:38-56` — `MEMNOTH_AUTO_LSP=1` opt-in and
//!   `MEMNOTH_LSP_ALLOW=rust,python` allowlist; absent either, default refuses.
//! - `lsp_diag.rs:70-89` — `summarize()` caps at five rows, counts severities,
//!   and prints file + line. **It never prints the diagnostic code**, which the
//!   G13 bullet (`plans/10-agentic-parity-and-moat.md:966`) requires.
//! - `src/kg/mod.rs:1-23` — the CLI graph is a regex **line scanner**; the
//!   tree-sitter production graph is called out as the real thing.
//! - `src/kg/mod.rs:615-694` — `kg index` calls `extract::extract` (the line
//!   scanner, `src/kg/extract.rs:1-16`), not tree-sitter.
//! - `src/kg/mod.rs:1062-1074` — `impact()` stamps `precision:
//!   best_available()` = `tree-sitter` on those lexical edges. The unresolved
//!   branches at `src/kg/mod.rs:923-935` and `:2147-2154` correctly stamp
//!   `Precision::Lexical`. **The stamp at `:1065` overstates the evidence.**
//! - `crates/memnoth-codegraph/src/tree_sitter.rs:62-143` — the real
//!   tree-sitter `symbols()` / `references()` that honestly earn
//!   `Precision::TreeSitter`.
//! - `src/bench_kg.rs:291-306` — the `precision` probe case;
//!   `ci/kg-verify-cases.json` case `kg-verify-011` already asserts
//!   `{ best: "tree-sitter", answer_precision: "tree-sitter", lsp: false }`.
//!   `tests/bench_kg_verify.rs:19-35` is a real, passing, deterministic gate —
//!   but it grades a *hand-seeded in-memory store*, not a fixture repo, and
//!   measures no precision/recall against rust-analyzer.
//!
//! ### 1e. Environment fact
//! - The only `rust-analyzer` on PATH is a **rustup proxy shim**:
//!   `rustup which rust-analyzer` reports "unknown binary ... in toolchain
//!   1.98.0", and `rustup component list --installed` has no `rust-analyzer`.
//!   There is no functioning LSP server binary here. (`pyright-langserver` is
//!   installed, but it is not the Rust fixture oracle.) This is why every test
//!   below is ignored rather than executed.

//! ## 2. What a real G13 fixture repo must contain
//!
//! A committed, pinned, offline-indexable fixture — not a hand-seeded SQLite
//! store. Concretely:
//!
//! - A real Rust crate with `Cargo.toml` (the `rust-analyzer` root marker in
//!   `SERVER_TABLE`), a `Cargo.lock`, and no network-only dependencies, so a
//!   language server can run `cargo metadata` offline in CI.
//! - Multiple modules with a target such as `MemoryEngine` (the exact symbol
//!   named in Gate G13) plus a hand-verified expected set of declarations and
//!   references: trait-method dispatch, `pub use` re-exports, a macro, two
//!   same-named symbols in different modules, a `#[cfg]`-gated item, a
//!   cross-file call, and a non-ASCII identifier to exercise UTF-16 positions.
//! - A **rust-analyzer ground-truth dump** produced by the real server's
//!   `textDocument/references` / `textDocument/definition` on each query
//!   symbol, stored as a versioned fixture. Per the bench rule it must be
//!   written by the oracle, never derived from the graph under test.
//! - A manifest pinning dataset sha256, rust-analyzer version, fixture commit,
//!   and hardware profile (plan 22 P22-007) so the run is reproducible.
//! - A second, larger corpus for the 100k-file index-latency leg of G13; the
//!   correctness fixture stays small enough to hand-verify.
//! - Gate math: precision = |returned ∩ truth| / |returned| ≥ 0.95 and recall
//!   = |returned ∩ truth| / |truth| ≥ 0.90, computed on the `(uri, range)`
//!   set, not on symbol names. The `MemoryEngine` references case is the
//!   named acceptance bullet; the rest generalize it.

//! ## 3. Exact external tool/crate and five-target build cost
//!
//! - **Oracle + backend:** `rust-analyzer`. Install with
//!   `rustup component add rust-analyzer` (a prebuilt binary, ~30-60 MB
//!   download, no C compilation) pinned to the 1.98.0 toolchain, or a pinned
//!   GitHub release binary. The *same* binary produces the ground-truth dump
//!   and is what `SERVER_TABLE` spawns. It is absent here (section 1e).
//! - **Client:** `memnoth-lsp` already exists and is pure Rust
//!   (anyhow/serde/serde_json/tokio) — negligible build cost, no new crate.
//!   The work to earn G13 is *implementation*, not dependency addition: wire
//!   the `initialize` handshake, set `encoding` from `position_encoding`,
//!   pass edited text through `did_change`, and adopt the 2 s/10 s timeouts.
//! - **tree-sitter C surface:** `crates/memnoth-codegraph/Cargo.toml:20-28`
//!   already lists `tree-sitter 0.25` plus eight grammar crates (rust, python,
//!   typescript, go, java, cpp, ruby). Each compiles a C parser via the `cc`
//!   crate on every target:
//!   - `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`,
//!     `x86_64-pc-windows-msvc` — native C compilers on their runners; first
//!     clean build compiles all eight grammars, incremental rebuilds hit the
//!     `cc` cache (`.github/workflows/release.yml:158-173`).
//!   - `aarch64-unknown-linux-gnu` — cross-built with the `cross` Docker tool
//!     (`release.yml:216`), which supplies the cross C compiler. The plan's
//!     "musl aarch64" wording (`KG-03`) is stale: the shipping leg is gnu.
//!
//! No build was run for this scaffold: the build cache at
//! `D:\memnoth-builds-cache` measured 54.21 GB against the hard 100 GB cap,
//! and all build output must stay on D:. Cost figures above are from the
//! manifest and CI workflow, not a fresh local measurement.

//! ## 4. Honest interim claim
//!
//! Until a real server and a ground-truth dump exist:
//!
//! - **No answer may claim `lsp`.** `capabilities().lsp == false`
//!   (`precision.rs:161`) and no server binary is installed here.
//! - CLI `kg` answers are produced by the regex line scanner
//!   (`src/kg/mod.rs:1-23`, `:615-694`), so their truthful tier is `lexical`.
//!   `impact()`'s `best_available()` stamp at `src/kg/mod.rs:1065` currently
//!   says `tree-sitter` — that overstates the evidence and is the first thing
//!   to fix (use `Precision::Lexical`, matching `:926`/`:2151`). Only answers
//!   from `tree_sitter.rs:84-143` have earned `tree-sitter` today.
//! - The committed bench fixture is internally honest (`lsp: false`), but
//!   precision/recall vs rust-analyzer is **unmeasured**. Do not claim
//!   0.95 / 0.90. The interim sentence is: "Lexical graph; tier labelled;
//!   LSP parity not measured."

//! ## 5. Missing-case scaffolds
//!
//! Each test is ignored so the workspace suite stays green. Run them
//! explicitly (`cargo test --test kg_lsp_acceptance -- --ignored`) and each
//! fails with a clear "NOT RUN" message naming the gate and the step that
//! closes it — a guard against treating the scaffold itself as evidence.

#[test]
#[ignore = "KG-01 / G-01 / G13: precision tier must be present and truthful on every answer in TUI and JSON; CLI graph edges are lexical but impact() stamps tree-sitter at src/kg/mod.rs:1065"]
fn every_answer_states_a_truthful_precision_tier() {
    panic!(
        "NOT RUN / KG-01, G-01, G13: every KG answer must carry \
         precision: lsp|tree-sitter|lexical in TUI and JSON, and the tier must \
         match the edge source. The CLI line scanner (src/kg/mod.rs:1-23, \
         :615-694) produces lexical edges, but impact() at :1065 stamps \
         best_available() = tree-sitter. Fix the stamp, then assert the label \
         on kg def/refs/impact/search/repomap in both renderers."
    );
}

#[test]
#[ignore = "G13: precision >=0.95 / recall >=0.90 vs rust-analyzer on a fixture repo is unmeasured; needs a real server, a fixture crate and a ground-truth dump"]
fn references_match_rust_analyzer_within_g13_thresholds() {
    panic!(
        "NOT RUN / G13: precision >=0.95 and recall >=0.90 against a \
         rust-analyzer ground-truth dump has not been measured. Build the \
         fixture repo in section 2, produce the oracle dump with a real \
         rust-analyzer, compute the (uri, range) set overlap, and record the \
         numbers in Plan 10/18 before claiming this gate."
    );
}

#[test]
#[ignore = "G13 named bullet: references for MemoryEngine must match rust-analyzer ground truth at >=0.95/>=0.90"]
fn memory_engine_references_match_ground_truth() {
    panic!(
        "NOT RUN / G13 named acceptance: `references` for `MemoryEngine` \
         (plans/10-agentic-parity-and-moat.md:960) must match a rust-analyzer \
         dump at >=0.95 precision / >=0.90 recall. No fixture containing \
         MemoryEngine and no oracle dump exists in this tree."
    );
}

#[test]
#[ignore = "KG-02 / P13-05: auto_lsp=false escape must be proven — no process starts even when a server is installed and MEMNOTH_AUTO_LSP is set"]
fn auto_lsp_false_is_a_real_escape_hatch() {
    panic!(
        "NOT RUN / KG-02, P13-05, G-05: prove the escape. With \
         auto_lsp=false, a present rust-analyzer, a matching extension and a \
         root marker, LspClient::start must return None and no child process \
         may exist. AutoStartPolicy::default() (lsp.rs:211-217) specifies this, \
         but no end-to-end test observes the process table; the env gate in \
         src/lsp_diag.rs:40-56 is only asserted on its default shape at :131-138."
    );
}

#[test]
#[ignore = "KG-02 / P13-03: capability probe — the LSP backend must be used only when the server is installed and answers, else fall back without failing"]
fn capability_probe_falls_back_when_no_server_answers() {
    panic!(
        "NOT RUN / KG-02, P13-03: there is no capability probe that starts a \
         server, checks the initialize reply, and falls back to tree-sitter or \
         lexical when the binary is missing or silent. LspClient::spawn \
         (client.rs:63-112) never sends initialize at all. Add the probe and \
         assert the fallback keeps a truthful precision label."
    );
}

#[test]
#[ignore = "KG-02 / G-02: utf-8/utf-16 position negotiation — convert helpers exist but are never wired from ServerCapabilities into the client"]
fn position_encoding_is_negotiated_end_to_end() {
    panic!(
        "NOT RUN / KG-02, G-02: PositionEncoding::from_capabilities and the \
         byte<->UTF-16 conversions (lsp.rs:268-310) exist and are unit-tested, \
         but LspClient.encoding (client.rs:27) is never set from \
         ServerCapabilities.position_encoding (protocol.rs:352) and no \
         initialize request is sent (client.rs:63-112). Prove a real server \
         that offers utf-8 and one that defaults to utf-16 both resolve a \
         non-ASCII identifier to the correct (uri, range)."
    );
}

#[test]
#[ignore = "KG-02 / P13-06: did_change hard-codes an empty document (client.rs:280-288); diagnostics-after-edit cannot work until the edited text is sent"]
fn diagnostics_after_edit_carry_the_real_edited_text() {
    panic!(
        "NOT RUN / KG-02, P13-06: LspClientHandle::did_change sends \
         contentChanges: [{{\"text\": \"\"}}] and takes no text argument, so a \
         real server sees an empty file. Wire the edited text through did_change \
         (or full sync on didOpen), then assert the server reports the error the \
         edit introduced."
    );
}

#[test]
#[ignore = "G13 named bullet: diagnostics after a deliberately broken edit must name file, line and code; summarize() prints no code (src/lsp_diag.rs:70-89)"]
fn broken_edit_diagnostics_name_file_line_and_code() {
    panic!(
        "NOT RUN / G13: after a deliberately broken edit, the summary must \
         name the file, the line and the diagnostic code, and the next turn \
         must fix it without being told the line. src/lsp_diag.rs:70-89 counts \
         severities and prints file:line but never diagnostic[\"code\"]. Add \
         the code, run against a real server, and record the result."
    );
}

#[test]
#[ignore = "KG-03 / G-05: tree-sitter C grammars must build on all five shipping targets including the cross-built aarch64 leg"]
fn tree_sitter_builds_on_all_five_release_targets() {
    panic!(
        "NOT RUN / KG-03, G-05: the eight tree-sitter grammars \
         (crates/memnoth-codegraph/Cargo.toml:20-28) are C build surfaces. \
         Prove `cargo build --release --locked` on x86_64-linux-gnu, \
         aarch64-linux-gnu (via cross), x86_64-apple-darwin, \
         aarch64-apple-darwin and x86_64-pc-windows-msvc. The plan's 'musl \
         aarch64' wording is stale: the shipping leg is gnu \
         (.github/workflows/release.yml:158-173, :216)."
    );
}

#[test]
#[ignore = "KG-03 / G13: 100k-file index under 60 s and incremental under 200 ms are unmeasured"]
fn hundred_k_file_index_meets_latency_budget() {
    panic!(
        "NOT RUN / KG-03, G13, P13-15: index a generated 100k-file fixture \
         and fail if full index >60 s or incremental >200 ms. The current \
         bench gate skips this (tests/bench_kg_verify.rs:34 asserts \
         index_budget.gate == \"skipped\"). Generate the fixture at test time, \
         do not commit it, and keep build output under D:\\memnoth-builds-cache."
    );
}

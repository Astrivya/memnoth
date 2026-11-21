//! KI-04 acceptance plan — terminal and accessibility gate.
//!
//! This file is a design scaffold, not a passing acceptance. It does three
//! things and nothing else:
//!
//! 1. Catalogues the existing terminal/accessibility tests that are ignored on
//!    Windows, with their exact names and `file:line` (section 1 below).
//! 2. Declares four ignored tests named for the cases that are still missing or
//!    unmeasured (section 4). Each one fails loudly with a "requires Linux tty /
//!    measurement not run" message if someone runs it with `--ignored` before the
//!    gate is closed.
//! 3. Records the exact Linux command a human must run to close the gate
//!    (section 2) and the honest boundary between that run and a green Windows
//!    CI job (section 3).
//!
//! Everything here is `#[ignore]`d. The file compiles under
//! `cargo test --workspace` and contributes zero executed assertions. No file
//! outside this scaffold is edited by this change.

//! ## 1. Catalogue — ignored terminal/accessibility tests
//!
//! Source of truth: `CLI/tests/tui_pty.rs`. All ten cases below carry
//! `#[cfg_attr(windows, ignore = "...")]` — they are skipped ONLY on Windows,
//! where ConPTY needs an interactive console the build agent does not give the
//! child (it dies in DLL initialisation with `0xC0000142`). On Linux the same
//! tests use a `forkpty` pair and do execute; the gate nonetheless requires a
//! deliberate, recorded Linux run with transcripts, not merely a green CI job.
//!
//! Two ignore strings are in use. "short" =
//! "ConPTY needs an interactive console; this host starts processes without one";
//! "long" is that sentence plus the docker command and "(see the module header)".
//! The reason is identical in substance for every row: no console on Windows.
//!
//! | ignore attr | fn line | test name | case | reason |
//! | --- | --- | --- | --- | --- |
//! | `tui_pty.rs:231` | 233 | `the_pty_captures_the_binarys_output` | PTY captures real binary output | short |
//! | `tui_pty.rs:242` | 246 | `the_screen_opens_at_eighty_columns_and_can_be_left` | 80x24 opens, fits, leaves | long |
//! | `tui_pty.rs:305` | 309 | `the_deck_focus_round_trip_at_eighty_columns` | 80x24 deck focus round trip | long |
//! | `tui_pty.rs:410` | 414 | `alt_t_opens_the_trace_at_eighty_columns` | 80x24 trace overlay | long |
//! | `tui_pty.rs:448` | 450 | `question_mark_opens_help_at_eighty_columns` | 80x24 help overlay | short |
//! | `tui_pty.rs:516` | 520 | `usage_renders_measured_text_at_eighty_columns` | 80x24 /usage | long |
//! | `tui_pty.rs:527` | 531 | `usage_renders_measured_text_at_forty_columns` | 40-column /usage | long |
//! | `tui_pty.rs:538` | 542 | `org_overview_is_read_only_at_eighty_columns` | 80x24 /org read-only | long |
//! | `tui_pty.rs:600` | 604 | `deck_opens_with_a_seeded_child_at_eighty_columns` | 80x24 seeded deck | long |
//! | `tui_pty.rs:649` | 653 | `help_fits_forty_columns_without_mid_word_clipping` | 40-column help, no mid-word clip | long |
//!
//! There is no automated screen-reader test and no 100-run responsiveness test
//! anywhere in the CLI. The screen-reader plumbing exists (`ScreenReaderMode`,
//! `mn set-screen-reader`, plain-text formatting), and the design docs record
//! the pass as unproven because it needs a real reader; the 100-run measurement
//! is likewise unmeasured. Those two gaps are the reason for the first and
//! fourth scaffolds below. Other ignored tests in the CLI (MCP eval arenas in
//! `eval.rs`, a `server.json` rewrite test) are unrelated to this gate.

//! ## 2. Exact Linux command to close the gate
//!
//! From the `CLI/tests/tui_pty.rs` module header (lines 19-23), verbatim:
//!
//! ```text
//! docker run --rm -v E:/memnoth-prod:/w -v mn-target-linux:/target \
//!   -e CARGO_TARGET_DIR=/target -w /w/CLI rust:1-bookworm \
//!   cargo test --test tui_pty -- --nocapture
//! ```
//!
//! Set `MEMNOTH_PTY_DUMP=1` on that command to print the parsed-screen
//! transcripts (the harness only dumps with that variable set). The human must
//! save the transcripts plus the host/terminal/backend, fixture size, and the
//! measured p50/p95 event-to-render numbers into Plan 22 (P22-096/097/102)
//! before claiming the gate. The 100-run and screen-reader cases need fixtures
//! and manual steps beyond this single command; the scaffolds below name them.

//! ## 3. Honest statement
//!
//! A passing Windows CI run is NOT this acceptance. On Windows these cases are
//! ignored, so `cargo test --workspace` going green says nothing about the
//! terminal matrix. Even a green Ubuntu CI job is repository evidence, not the
//! deliberately recorded Linux-tty run plus 100-run measurement the gate
//! demands. Until the four cases below are replaced with real implementations
//! and their results recorded in Plan 22, KI-04 stays open.

//! ## 4. Missing-case scaffolds
//!
//! Each test is ignored so the workspace suite stays green. Run them explicitly
//! (`cargo test --test tui_acceptance_plan -- --ignored`) and each fails with a
//! clear "requires Linux tty / measurement not run" message — a guard against
//! treating the scaffold itself as evidence.

#[test]
#[ignore = "KI-04 / P22-097: 100-run responsiveness is unmeasured; requires a real Linux tty and a recorded result"]
fn hundred_run_responsiveness_measurement_requires_linux_tty() {
    panic!(
        "REQUIRES LINUX TTY / MEASUREMENT NOT RUN: 100-run responsiveness \
         (KI-04, P22-097/099/102) has no measurement. Seed 100 runs behind a \
         real Linux tty, record p50/p95 event-to-render in Plan 22, then \
         replace this scaffold with the measured test."
    );
}

#[test]
#[ignore = "KI-04 / P22-102: 80x24 PTY acceptance needs a deliberate, recorded Linux run (not Windows CI)"]
fn eighty_by_twentyfour_pty_acceptance_requires_linux_tty() {
    panic!(
        "REQUIRES LINUX TTY / MEASUREMENT NOT RUN: the 80x24 PTY cases in \
         CLI/tests/tui_pty.rs are ignored on Windows. Run the docker command in \
         section 2 with MEMNOTH_PTY_DUMP=1, save the transcript, then replace \
         this scaffold with that recorded acceptance."
    );
}

#[test]
#[ignore = "KI-04 / P22-102: 40-column PTY acceptance needs a deliberate, recorded Linux run (not Windows CI)"]
fn forty_column_pty_acceptance_requires_linux_tty() {
    panic!(
        "REQUIRES LINUX TTY / MEASUREMENT NOT RUN: the 40-column cases \
         (usage_renders_measured_text_at_forty_columns, \
         help_fits_forty_columns_without_mid_word_clipping) are ignored on \
         Windows. Run them on a real Linux tty, save the transcript, then \
         replace this scaffold with that recorded acceptance."
    );
}

#[test]
#[ignore = "KI-04 / P22-102: screen-reader acceptance needs a real reader; the PTY harness does not provide one"]
fn screen_reader_acceptance_requires_real_reader() {
    panic!(
        "REQUIRES A REAL SCREEN READER / MEASUREMENT NOT RUN: no automated \
         screen-reader test exists; the pass is unproven. Exercise the 80x24 \
         and 40-column keyboard-only flows with an actual reader, record the \
         accessibility findings in Plan 22, then replace this scaffold."
    );
}

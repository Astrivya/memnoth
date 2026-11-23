//! Motion and feedback policy — pure rules, no clock, no I/O, no global state.
//!
//! This module encodes the feedback half of `docs/research/cli-theme-and-design-2026-11-23.md`
//! (§3 Charm braille spinners at 7–12 fps, §4 rules 5 and 10, §7 "never
//! full-screen-redraw a spinner"). It is a *policy*: given a few signals it
//! decides whether animation is allowed, when a spinner may appear, which
//! frame to draw, and what to print instead when motion is off. It never
//! sleeps, prints, or reads a terminal on its own — the caller supplies
//! `is_tty` and `screen_reader` once and owns the clock.
//!
//! The rules, in one place:
//!
//! * First output <100 ms, meaningful result <400 ms. A spinner is therefore
//!   withheld until [`DEFAULT_REVEAL_DELAY`] (140 ms) — work that finishes
//!   before then never flickers a spinner at all.
//! * Once revealed, a spinner is held for at least [`DEFAULT_MIN_HOLD`]
//!   (500 ms). [`MotionPolicy::spinner_window`] gives the exact visible window.
//! * Frames advance every [`DEFAULT_FRAME_INTERVAL`] (90 ms ≈ 11 fps, inside
//!   the 75–120 ms / 8–13 fps band). Frames are plain braille characters:
//!   no colour, no SGR blink, no escape sequences of any kind.
//! * Progress is determinate — [`progress_label`] says "X of Y", never a
//!   percentage. When the total is unknown, [`progress_unknown`] reports the
//!   stage and honest elapsed time instead of inventing a fraction.
//! * Reduced motion is a single boolean resolved once at construction. Any
//!   of: not a TTY, `CI`, `TERM=dumb`, `NO_COLOR`, one of
//!   `MEMNOTH_NO_MOTION` / `NO_ANIMATION` / `REDUCE_MOTION`, or screen-reader
//!   mode turns all motion off. The fallback is a plain state word from
//!   [`MotionPolicy::static_status`] that changes only when real progress
//!   changes — never on a timer.
//! * Decorative animation is a separate, strictly opt-in flag
//!   (`MEMNOTH_MOTION`), lasts at most [`DECORATION_BUDGET`] (3 s), is computed
//!   purely from elapsed time (non-blocking), and is always off in
//!   screen-reader mode.
//!
//! Pure (`std` only) so it can be graded by a path-included integration test
//! before it is declared in `lib.rs`.

use std::time::Duration;

/// Earliest point at which a spinner may be revealed. Quick work never shows
/// one, which avoids the flicker of a spinner that appears and vanishes inside
/// one human perception window.
pub const DEFAULT_REVEAL_DELAY: Duration = Duration::from_millis(140);

/// Minimum time a revealed spinner stays on screen, even if the work finishes
/// sooner. Prevents a one-frame flash.
pub const DEFAULT_MIN_HOLD: Duration = Duration::from_millis(500);

/// Spinner frame interval: 90 ms ≈ 11 fps, inside the 75–120 ms (8–13 fps)
/// band. Fast enough to read as motion, slow enough not to strobe.
pub const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_millis(90);

/// Hard ceiling for optional decorative animation. Decoration must be over
/// before this; it must never block output.
pub const DECORATION_BUDGET: Duration = Duration::from_secs(3);

/// Budget for the first byte of output. Informational: callers should emit
/// something by now, which is why no spinner is justified before it.
pub const FIRST_OUTPUT_BUDGET: Duration = Duration::from_millis(100);

/// Budget for a meaningful result. Informational target for the work itself,
/// not a promise that every command can meet it.
pub const MEANINGFUL_RESULT_BUDGET: Duration = Duration::from_millis(400);

/// Braille "dots rotating" sequence (the familiar `cli-spinners` dots set).
/// Plain characters only — no ANSI, no colour, no SGR blink.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Very slow plain-glyph shimmer for opt-in decoration. Plain characters only.
const DECORATION_FRAMES: &[&str] = &["·", "✧", "✦", "✧"];

/// The environment/terminal signals that decide the policy. Split out from
/// [`MotionPolicy`] so the variable-to-signal mapping is testable without
/// touching the process environment (which is global and would make parallel
/// tests flaky). `is_tty` and `screen_reader` are supplied by the caller
/// because only the caller can answer them correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionSignals {
    pub is_tty: bool,
    pub screen_reader: bool,
    pub ci: bool,
    pub term_dumb: bool,
    pub no_color: bool,
    /// Any of `MEMNOTH_NO_MOTION`, `NO_ANIMATION`, `REDUCE_MOTION` was set.
    pub no_motion: bool,
    /// `MEMNOTH_MOTION` truthily set: opt-in for decoration only.
    pub motion_opt_in: bool,
}

fn non_empty(value: &str) -> bool {
    !value.is_empty()
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl MotionSignals {
    /// Build signals from an iterable of `(key, value)` pairs. Key matching is
    /// ASCII case-insensitive so the same rules hold on Windows, where
    /// environment names are case-insensitive. A variable with an empty value
    /// counts as "not set" (the `NO_COLOR` convention is presence of a
    /// non-empty value). Duplicate keys accumulate.
    pub fn from_vars<K, V>(
        is_tty: bool,
        screen_reader: bool,
        vars: impl IntoIterator<Item = (K, V)>,
    ) -> Self
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut signals = Self {
            is_tty,
            screen_reader,
            ci: false,
            term_dumb: false,
            no_color: false,
            no_motion: false,
            motion_opt_in: false,
        };
        for (key, value) in vars {
            let key = key.as_ref();
            let value = value.as_ref();
            if key.eq_ignore_ascii_case("CI") {
                signals.ci |= non_empty(value);
            } else if key.eq_ignore_ascii_case("TERM") {
                signals.term_dumb |= value.eq_ignore_ascii_case("dumb");
            } else if key.eq_ignore_ascii_case("NO_COLOR") {
                signals.no_color |= non_empty(value);
            } else if key.eq_ignore_ascii_case("MEMNOTH_NO_MOTION")
                || key.eq_ignore_ascii_case("NO_ANIMATION")
                || key.eq_ignore_ascii_case("REDUCE_MOTION")
            {
                signals.no_motion |= non_empty(value);
            } else if key.eq_ignore_ascii_case("MEMNOTH_MOTION") {
                signals.motion_opt_in |= truthy(value);
            }
        }
        signals
    }

    /// Read signals from the process environment. `vars_os` plus lossy
    /// conversion is used deliberately: `std::env::vars()` panics on a
    /// non-Unicode value, and a stray byte must not take down the CLI.
    pub fn from_env(is_tty: bool, screen_reader: bool) -> Self {
        let vars: Vec<(String, String)> = std::env::vars_os()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect();
        Self::from_vars(is_tty, screen_reader, vars)
    }

    /// The single reduced-motion gate, evaluated from the signals.
    pub fn motion_disabled(&self) -> bool {
        !self.is_tty
            || self.screen_reader
            || self.ci
            || self.term_dumb
            || self.no_color
            || self.no_motion
    }
}

/// Resolved motion policy. `enabled` is the one motion boolean: it is fixed
/// once at construction and every method below reads it, so the answer cannot
/// drift mid-command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionPolicy {
    pub enabled: bool,
    pub reveal_delay: Duration,
    pub min_hold: Duration,
    pub frame_interval: Duration,
    pub decoration_budget: Duration,
    /// Decoration is strictly opt-in and only meaningful when `enabled`.
    pub decorations_enabled: bool,
}

impl Default for MotionPolicy {
    /// Static by default: no animation, default timings retained so the
    /// policy still describes what *would* happen if it were enabled.
    fn default() -> Self {
        Self {
            enabled: false,
            reveal_delay: DEFAULT_REVEAL_DELAY,
            min_hold: DEFAULT_MIN_HOLD,
            frame_interval: DEFAULT_FRAME_INTERVAL,
            decoration_budget: DECORATION_BUDGET,
            decorations_enabled: false,
        }
    }
}

impl MotionPolicy {
    /// Resolve from explicit signals. Kept pure so callers can supply their
    /// own terminal detection in tests.
    pub fn from_signals(signals: MotionSignals) -> Self {
        let enabled = !signals.motion_disabled();
        Self {
            enabled,
            decorations_enabled: enabled && signals.motion_opt_in && !signals.screen_reader,
            ..Self::default()
        }
    }

    /// Format a determinate progress label: "X of Y". Never a percentage — an
    /// invented percentage is the exact failure mode this policy rules out.
    pub fn progress_label(&self, done: usize, total: usize) -> String {
        progress_label(done, total)
    }

    /// Honest fallback when the total is unknown: a stage word and real
    /// elapsed time, with no fraction and no percentage.
    pub fn progress_unknown(&self, stage: &str, elapsed: Duration) -> String {
        progress_unknown(stage, elapsed)
    }

    /// Plain static fallback for pipes, CI and reduced motion. It is a pure
    /// function of `stage`, `done` and `total`: identical inputs always give
    /// identical text, so it changes only on real progress, never on a timer.
    pub fn static_status(&self, stage: &str, done: usize, total: usize) -> String {
        format!("{stage}: {}", progress_label(done, total))
    }

    /// Whether a spinner may be revealed for work that has run `elapsed`.
    /// Always false under a static policy.
    pub fn should_show_spinner(&self, elapsed: Duration) -> bool {
        self.enabled && elapsed >= self.reveal_delay
    }

    /// The visible window for a spinner, measured from when the work started,
    /// given the time it finished. `None` means "never show one" — work that
    /// ends before the reveal delay does not flicker. When work ends after the
    /// reveal, the window ends at `max(finish, reveal + min_hold)` so a
    /// revealed spinner is never cut short of its minimum hold.
    pub fn spinner_window(&self, work_finished: Duration) -> Option<(Duration, Duration)> {
        if !self.enabled || work_finished < self.reveal_delay {
            return None;
        }
        let start = self.reveal_delay;
        let end = work_finished.max(start + self.min_hold);
        Some((start, end))
    }

    /// The braille frame for `elapsed` (measured from when the work started).
    /// Returns an empty string under a static policy, so a caller that
    /// ignores `enabled` still prints no animation. Plain characters only:
    /// no colour, no SGR blink, no escape sequences.
    pub fn spinner_frame(&self, elapsed: Duration) -> &'static str {
        if !self.enabled {
            return "";
        }
        let interval_ms = self.frame_interval.as_millis().max(1);
        let index = (elapsed.as_millis() / interval_ms) as usize % SPINNER_FRAMES.len();
        SPINNER_FRAMES[index]
    }

    /// An opt-in decorative frame, or "" once the 3 s budget is spent or
    /// motion is off/screen-reader mode. Pure and non-blocking: the caller
    /// advances `elapsed` and must stop at the budget, never wait on a frame.
    pub fn decoration_frame(&self, elapsed: Duration) -> &'static str {
        if !self.decorations_enabled || elapsed >= self.decoration_budget {
            return "";
        }
        let interval = self.frame_interval.saturating_mul(3).as_millis().max(1);
        let index = (elapsed.as_millis() / interval) as usize % DECORATION_FRAMES.len();
        DECORATION_FRAMES[index]
    }
}

/// Resolve the policy from the process environment. Call this once, after the
/// caller has answered `is_tty` (e.g. `stdout().is_terminal()`) and
/// `screen_reader`, then thread the resulting value through the command.
pub fn from_env(is_tty: bool, screen_reader: bool) -> MotionPolicy {
    MotionPolicy::from_signals(MotionSignals::from_env(is_tty, screen_reader))
}

/// Determinacy helper: determinate progress as "X of Y". No percent sign is
/// ever produced, and `0 of 0` is printed honestly rather than as 100%.
pub fn progress_label(done: usize, total: usize) -> String {
    format!("{done} of {total}")
}

fn format_elapsed(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", millis as f64 / 1_000.0)
    }
}

/// Honest unknown-total progress: stage plus elapsed time. No fake fraction.
pub fn progress_unknown(stage: &str, elapsed: Duration) -> String {
    format!("{stage}: {} elapsed", format_elapsed(elapsed))
}

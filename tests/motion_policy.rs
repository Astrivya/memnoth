//! Feedback/motion policy tests.
//!
//! The module is included by source path so these tests grade the same code the
//! CLI will use once `src/motion.rs` is declared in `lib.rs`. No global state
//! is mutated: environment gating is tested through `MotionSignals::from_vars`
//! with explicit variable pairs, so parallel tests cannot race on `std::env`.

#[path = "../src/motion.rs"]
mod motion;

use std::time::Duration;

use motion::{
    DECORATION_BUDGET, DEFAULT_FRAME_INTERVAL, DEFAULT_MIN_HOLD, DEFAULT_REVEAL_DELAY,
    FIRST_OUTPUT_BUDGET, MEANINGFUL_RESULT_BUDGET, MotionPolicy, MotionSignals, SPINNER_FRAMES,
    from_env, progress_label, progress_unknown,
};

/// A normal interactive terminal with the given environment variables set.
fn tty(vars: &[(&str, &str)]) -> MotionPolicy {
    MotionPolicy::from_signals(MotionSignals::from_vars(true, false, vars.iter().copied()))
}

/// A terminal with screen-reader mode on.
fn screen_reader(vars: &[(&str, &str)]) -> MotionPolicy {
    MotionPolicy::from_signals(MotionSignals::from_vars(true, true, vars.iter().copied()))
}

#[test]
fn non_tty_disables_all_motion() {
    let policy = MotionPolicy::from_signals(MotionSignals::from_vars(
        false,
        false,
        Vec::<(&str, &str)>::new(),
    ));
    assert!(!policy.enabled);
    assert!(!policy.should_show_spinner(Duration::from_secs(2)));
    assert_eq!(policy.spinner_frame(Duration::from_secs(2)), "");
    assert_eq!(policy.spinner_window(Duration::from_secs(2)), None);
    // The real entry point agrees for a non-TTY no matter what else is set.
    assert!(!from_env(false, false).enabled);
}

#[test]
fn a_clean_tty_enables_motion_but_not_decoration() {
    let policy = tty(&[]);
    assert!(policy.enabled);
    assert!(!policy.decorations_enabled, "decoration must be opt-in");
}

#[test]
fn ci_disables_motion() {
    for value in ["true", "1", "yes"] {
        assert!(!tty(&[("CI", value)]).enabled, "CI={value}");
    }
    assert!(!tty(&[("ci", "true")]).enabled, "keys are case-insensitive");
    assert!(tty(&[("CI", "")]).enabled, "an empty value is not set");
}

#[test]
fn no_color_disables_motion() {
    assert!(!tty(&[("NO_COLOR", "1")]).enabled);
    assert!(
        !tty(&[("NO_COLOR", "0")]).enabled,
        "presence disables, per no-color.org"
    );
    assert!(tty(&[("NO_COLOR", "")]).enabled);
}

#[test]
fn reduce_motion_aliases_disable_motion() {
    for key in ["MEMNOTH_NO_MOTION", "NO_ANIMATION", "REDUCE_MOTION"] {
        assert!(!tty(&[(key, "1")]).enabled, "{key} should disable motion");
        assert!(tty(&[(key, "")]).enabled, "{key}= empty is not set");
    }
}

#[test]
fn term_dumb_disables_motion() {
    assert!(!tty(&[("TERM", "dumb")]).enabled);
    assert!(tty(&[("TERM", "xterm-256color")]).enabled);
}

#[test]
fn screen_reader_disables_motion_and_decoration() {
    assert!(!screen_reader(&[]).enabled);
    // Even an explicit opt-in cannot re-enable decoration for a screen reader.
    let opted_in = screen_reader(&[("MEMNOTH_MOTION", "1")]);
    assert!(!opted_in.enabled);
    assert!(!opted_in.decorations_enabled);
    assert_eq!(opted_in.decoration_frame(Duration::ZERO), "");
}

#[test]
fn reveal_delay_suppresses_flicker_for_quick_work() {
    let policy = tty(&[]);
    assert_eq!(policy.reveal_delay, DEFAULT_REVEAL_DELAY);
    // Reveal before the meaningful-result budget, but after the first-output
    // budget: a spinner is never justified for output that arrives at 100 ms.
    assert!(FIRST_OUTPUT_BUDGET < DEFAULT_REVEAL_DELAY);
    assert!(DEFAULT_REVEAL_DELAY < MEANINGFUL_RESULT_BUDGET);
    assert!(!policy.should_show_spinner(FIRST_OUTPUT_BUDGET));
    assert!(!policy.should_show_spinner(DEFAULT_REVEAL_DELAY - Duration::from_millis(1)));
    assert!(policy.should_show_spinner(DEFAULT_REVEAL_DELAY));
    assert!(policy.should_show_spinner(Duration::from_secs(1)));
}

#[test]
fn min_hold_extends_a_revealed_spinner() {
    let policy = tty(&[]);
    assert_eq!(policy.min_hold, DEFAULT_MIN_HOLD);
    assert_eq!(policy.spinner_window(FIRST_OUTPUT_BUDGET), None);
    assert_eq!(
        policy.spinner_window(DEFAULT_REVEAL_DELAY - Duration::from_millis(1)),
        None
    );
    let (start, end) = policy
        .spinner_window(Duration::from_millis(200))
        .expect("work past the reveal delay shows a spinner");
    assert_eq!(start, DEFAULT_REVEAL_DELAY);
    assert_eq!(end - start, DEFAULT_MIN_HOLD, "hold the full minimum");
    // Longer work simply keeps the spinner until it finishes.
    let (_, end) = policy
        .spinner_window(Duration::from_millis(900))
        .expect("revealed");
    assert_eq!(end, Duration::from_millis(900));
}

#[test]
fn spinner_frames_are_plain_braille_and_cycle() {
    let policy = tty(&[]);
    assert_eq!(policy.frame_interval, DEFAULT_FRAME_INTERVAL);
    for frame in SPINNER_FRAMES {
        assert_eq!(frame.chars().count(), 1, "one cell per frame");
        let codepoint = frame.chars().next().expect("non-empty") as u32;
        assert!(
            (0x2800..=0x28FF).contains(&codepoint),
            "not a braille glyph: {frame}"
        );
        assert!(!frame.contains('\u{1b}'), "no ANSI/SGR blink");
    }
    for (index, expected) in SPINNER_FRAMES.iter().enumerate() {
        let frame = policy.spinner_frame(DEFAULT_FRAME_INTERVAL.saturating_mul(index as u32));
        assert_eq!(frame, *expected);
        assert!(!frame.contains('\u{1b}'));
    }
    // Wraps back to the first frame after a full cycle.
    let wrapped = DEFAULT_FRAME_INTERVAL.saturating_mul(SPINNER_FRAMES.len() as u32);
    assert_eq!(policy.spinner_frame(wrapped), SPINNER_FRAMES[0]);
}

#[test]
fn frame_interval_is_within_the_8_to_13_fps_band() {
    let millis = DEFAULT_FRAME_INTERVAL.as_millis();
    assert!(
        (75..=120).contains(&millis),
        "interval {millis}ms outside 75-120ms"
    );
    let fps = 1_000.0 / millis as f64;
    assert!((8.0..=13.0).contains(&fps), "{fps} fps outside 8-13 fps");
}

#[test]
fn progress_is_determinate_and_never_a_fake_percentage() {
    assert_eq!(progress_label(3, 10), "3 of 10");
    assert_eq!(progress_label(0, 0), "0 of 0");
    let label = tty(&[]).progress_label(7, 9);
    assert_eq!(label, "7 of 9");
    assert!(!label.contains('%'), "no invented percentage: {label}");
}

#[test]
fn unknown_progress_reports_stage_and_honest_elapsed() {
    let label = progress_unknown("indexing", Duration::from_millis(1_200));
    assert!(label.starts_with("indexing:"));
    assert!(label.contains("1.2s"));
    assert!(!label.contains('%'));
    let sub_second = progress_unknown("waiting", Duration::from_millis(250));
    assert!(sub_second.contains("250ms"));
    assert_eq!(
        tty(&[]).progress_unknown("kg", Duration::from_secs(2)),
        "kg: 2.0s elapsed"
    );
}

#[test]
fn static_fallback_changes_only_on_real_progress() {
    let policy = from_env(false, false);
    let before = policy.static_status("indexing", 3, 10);
    assert_eq!(before, policy.static_status("indexing", 3, 10));
    assert_ne!(before, policy.static_status("indexing", 4, 10));
    assert!(!before.contains('%'));
    assert!(!before.contains('\u{1b}'), "plain text only");
    assert!(before.starts_with("indexing: 3 of 10"));
}

#[test]
fn decoration_is_opt_in_bounded_and_non_blocking() {
    let on = tty(&[("MEMNOTH_MOTION", "1")]);
    assert!(on.decorations_enabled);
    assert_eq!(on.decoration_budget, DECORATION_BUDGET);
    assert!(DECORATION_BUDGET <= Duration::from_secs(3));
    assert!(!on.decoration_frame(Duration::ZERO).is_empty());
    assert_eq!(on.decoration_frame(DECORATION_BUDGET), "");
    assert_eq!(
        on.decoration_frame(DECORATION_BUDGET + Duration::from_secs(1)),
        ""
    );
    // A non-truthy opt-in is not an opt-in.
    assert!(!tty(&[("MEMNOTH_MOTION", "0")]).decorations_enabled);
    // Frames are plain glyphs; the API exposes no wait/sleep primitive.
    for index in 0..12 {
        let frame = on.decoration_frame(Duration::from_millis(index * 200));
        assert!(!frame.contains('\u{1b}'));
    }
}

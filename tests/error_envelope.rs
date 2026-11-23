//! The versioned JSON error envelope, stable code registry, human rendering
//! and typo hints. Integration tests: only the public `memnoth::output` API is
//! used, so these assertions hold for any caller of the library.

use memnoth::output::{
    ERROR_ENVELOPE_SCHEMA_VERSION, ErrorCode, ErrorEnvelope, ErrorOutcome, ExitCode, closest,
    did_you_mean, edit_distance, emit_error_envelope, error_envelope, exit_code_for,
    render_error_human,
};
use serde_json::{Value, json};

#[test]
fn envelope_has_version_nested_error_and_stable_fields() {
    let error = ErrorOutcome::auth("not signed in")
        .with_error_code(ErrorCode::Auth)
        .with_context(json!({ "attempt": 1 }))
        .with_run("memnoth auth login")
        .with_cause("token missing")
        .with_docs_url("https://example.test/cli#auth");

    let value: Value = error_envelope(&error).to_value();
    assert_eq!(value["schemaVersion"], ERROR_ENVELOPE_SCHEMA_VERSION);
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["error"]["code"], "AUTH");
    assert_eq!(value["error"]["message"], "not signed in");
    assert_eq!(value["error"]["retryable"], false);
    assert_eq!(value["error"]["context"]["attempt"], 1);
    assert_eq!(value["error"]["context"]["cause"], "token missing");
    assert_eq!(
        value["error"]["context"]["docs"],
        "https://example.test/cli#auth"
    );
    assert_eq!(value["error"]["run"], "memnoth auth login");
    // The legacy envelope's top-level `ok` / numeric `code` are not part of
    // the versioned shape.
    assert!(value.get("ok").is_none());
    assert!(value.get("command").is_none());
}

#[test]
fn envelope_derives_code_and_retryability_from_exit_code() {
    let transient = error_envelope(&ErrorOutcome::transient("server error")).to_value();
    assert_eq!(transient["error"]["code"], "TRANSIENT");
    assert_eq!(transient["error"]["retryable"], true);

    let usage = error_envelope(&ErrorOutcome::usage("bad flag")).to_value();
    assert_eq!(usage["error"]["code"], "USAGE");
    assert_eq!(usage["error"]["retryable"], false);

    let conflict = error_envelope(&ErrorOutcome::conflict("state moved")).to_value();
    assert_eq!(conflict["error"]["code"], "CONFLICT");
    assert_eq!(conflict["error"]["retryable"], true);
}

#[test]
fn code_registry_maps_to_the_shipped_exit_codes() {
    assert_eq!(ErrorCode::Runtime.as_str(), "RUNTIME");
    assert_eq!(ErrorCode::NotFound.as_str(), "NOT_FOUND");
    assert_eq!(ErrorCode::RateLimited.as_str(), "RATE_LIMITED");

    assert_eq!(exit_code_for(ErrorCode::Runtime), ExitCode::Unexpected);
    assert_eq!(exit_code_for(ErrorCode::Usage), ExitCode::Usage);
    assert_eq!(exit_code_for(ErrorCode::Auth), ExitCode::Auth);
    assert_eq!(exit_code_for(ErrorCode::NotFound), ExitCode::NotFound);
    assert_eq!(exit_code_for(ErrorCode::Transient), ExitCode::Transient);
    assert_eq!(exit_code_for(ErrorCode::Conflict), ExitCode::Conflict);
    assert_eq!(exit_code_for(ErrorCode::RateLimited), ExitCode::RateLimited);
    assert_eq!(exit_code_for(ErrorCode::Unsupported), ExitCode::Unsupported);
    assert_eq!(exit_code_for(ErrorCode::Cancelled), ExitCode::Cancelled);
}

#[test]
fn every_shipped_exit_code_round_trips_through_the_registry() {
    for code in [2u8, 3, 4, 5, 6, 7, 8, 130] {
        let kind = ErrorCode::from_exit_code(code);
        assert_eq!(kind.exit_code() as u8, code, "exit code {code}");
    }
    assert_eq!(ErrorCode::from_exit_code(1), ErrorCode::Runtime);
    assert_eq!(ErrorCode::from_exit_code(99), ErrorCode::Runtime);
}

#[test]
fn explicit_code_and_retryable_override_the_derived_values() {
    let error = ErrorOutcome::new(ExitCode::Unexpected, "state moved")
        .with_error_code(ErrorCode::Conflict)
        .with_retryable(true);
    let value = error_envelope(&error).to_value();
    assert_eq!(value["error"]["code"], "CONFLICT");
    assert_eq!(value["error"]["retryable"], true);
    // The process exit code is untouched by the finer machine code.
    assert_eq!(error.code, 1);
}

#[test]
fn envelope_context_is_allowlisted() {
    let error = ErrorOutcome::usage("bad flag").with_cause("unknown --frob");
    let context = error_envelope(&error).to_value()["error"]["context"]
        .as_object()
        .unwrap()
        .clone();
    for key in context.keys() {
        assert!(
            matches!(key.as_str(), "cause" | "docs" | "suggestion" | "detail"),
            "unexpected context key: {key}"
        );
    }
}

#[test]
fn human_output_is_headline_then_note_then_run() {
    let error = ErrorOutcome::new(ExitCode::Auth, "not signed in")
        .with_cause("token missing")
        .with_suggestion("did you mean 'auth login'?")
        .with_run("memnoth auth login")
        .with_docs_url("https://example.test");
    let rendered = render_error_human(&error);
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines[0], "memnoth: not signed in");
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("note:"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("run:"))
    );
    assert!(rendered.contains("memnoth auth login"));
    // No stack trace, no tabs, no backtrace dump.
    assert!(!rendered.to_lowercase().contains("stack"));
    assert!(!rendered.contains('\t'));
}

#[test]
fn typo_suggestions_use_edit_distance_and_never_execute() {
    assert_eq!(edit_distance("vault", "valut"), 2);
    assert_eq!(edit_distance("vault", "vault"), 0);
    assert_eq!(edit_distance("", "abc"), 3);

    let candidates = ["session", "vault", "kg", "auth"];
    assert_eq!(closest("valut", &candidates), Some("vault"));
    assert_eq!(
        did_you_mean("valut", &candidates).as_deref(),
        Some("did you mean 'vault'?")
    );
    assert!(did_you_mean("zzzz", &candidates).is_none());

    // The hint is text only; building it changes no state and runs nothing.
    let error = ErrorOutcome::not_found("no such verb").with_typo_suggestion("valut", &candidates);
    assert_eq!(error.typo_hint(), Some("did you mean 'vault'?"));
    assert_eq!(
        error_envelope(&error).to_value()["error"]["context"]["suggestion"],
        "did you mean 'vault'?"
    );
}

#[test]
fn envelope_round_trips_through_serde() {
    let error = ErrorOutcome::auth("nope").with_run("mn auth login");
    let text = error_envelope(&error).to_string_pretty();
    let back: ErrorEnvelope = serde_json::from_str(&text).expect("envelope deserializes");
    assert_eq!(back.schema_version, 1);
    assert_eq!(back.error.code, ErrorCode::Auth);
    assert_eq!(back.error.run.as_deref(), Some("mn auth login"));
}

#[test]
fn emitter_accepts_both_modes() {
    // Both branches write to stderr; neither may panic.
    let error = ErrorOutcome::usage("bad flag");
    emit_error_envelope(true, &error);
    emit_error_envelope(false, &error);
}

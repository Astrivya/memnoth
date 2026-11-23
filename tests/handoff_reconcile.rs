//! Declared `mn-handoff/1` bundles: P22-049 field round trips and P22-050
//! refusal when the branch, revision, worktree or a recorded check no longer
//! matches. Deterministic: a local git repository and a temp home, no model,
//! no network, no credential store.

use std::path::{Path, PathBuf};

use memnoth::context::{AppContext, Config};
use memnoth::goals::GoalStore;
use memnoth::session::SessionStore;
use memnoth::session_cmd::handoff_reconcile as hr;
use memnoth::session_cmd::{attach, handoff};
use memnoth_session::{ApprovalRecord, Session, TurnReceipt};
use serde_json::{Value, json};

fn git(root: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// A clean repo with one commit, so both revision and branch are capturable.
fn init_repo(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    assert!(git(root, &["init", "-q"]), "git init failed");
    assert!(git(root, &["config", "user.email", "test@example.invalid"]));
    assert!(git(root, &["config", "user.name", "Test"]));
    std::fs::write(root.join("seed.txt"), "one\n").unwrap();
    assert!(git(root, &["add", "seed.txt"]));
    assert!(git(root, &["commit", "-q", "-m", "seed"]));
}

fn context(root: &Path, state: &Path) -> AppContext {
    AppContext {
        root: root.to_path_buf(),
        home: state.to_path_buf(),
        vault_path: state.join("vault.db"),
        namespace: "default".to_string(),
        graph_path: state.join("graph.db"),
        sessions_dir: state.join("sessions"),
        config_path: state.join("config.toml"),
        config: Config::default(),
        json: false,
        quiet: true,
        exe_path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("memnoth")),
    }
}

fn receipt() -> TurnReceipt {
    TurnReceipt {
        at_ms: 1_000,
        total_tokens: 10,
        prefix_hash: "abcd1234".to_string(),
        layer_tokens: [1, 2, 3, 4],
        pieces: 1,
        input_tokens: 8,
        context_window_used: None,
        context_window_model: None,
        context_window_provider: None,
        provider_id: None,
        reasoning_effort: None,
        duration_ms: None,
        provider_requests: None,
        tool_calls: None,
        provider_reasoning_tokens: None,
        provider_cache_read_tokens: None,
        provider_cache_write_tokens: None,
        provider_usage_reported: None,
        provider_input_reported: None,
        provider_output_reported: None,
        output_tokens: 2,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        prompt: String::new(),
        cost_micros: Some(42),
        turn_id: Some(0),
    }
}

/// Seed one session with a receipt and an approval, under `ctx.home`.
fn seed(ctx: &AppContext, name: &str) {
    let mut session = Session::new("m");
    session.id = name.to_string();
    session.user("land the handoff");
    session.assistant("landed");
    session.record_receipt(receipt());
    session.record_approval(ApprovalRecord::user("edit_file", "src/a.rs", true, "asha"));
    SessionStore::new(&ctx.sessions_dir).save(&session).unwrap();
}

fn sample_manifest() -> hr::HandoffManifest {
    hr::HandoffManifest {
        schema: hr::HANDOFF_SCHEMA.to_string(),
        session: "origin".to_string(),
        captured_ms: 1_700_000_000_000,
        goals: vec![hr::GoalState {
            condition: "land it".to_string(),
            done: false,
            set_ms: 1_700_000_000_000,
        }],
        constraints: vec!["no network".to_string(), "keep api stable".to_string()],
        repository: hr::RepositoryState {
            branch: Some("main".to_string()),
            revision: Some("a".repeat(40)),
            dirty: false,
        },
        attempts: vec![hr::AttemptRecord {
            label: "turn 0".to_string(),
            at_ms: 1_000,
            outcome: "completed".to_string(),
            detail: "abcd1234".to_string(),
        }],
        verification: vec![hr::VerificationRecord {
            name: "cargo test".to_string(),
            ok: true,
            at_ms: 2_000,
            revision: Some("a".repeat(40)),
        }],
        pending_approvals: vec![hr::PendingApproval {
            tool: "edit_file".to_string(),
            subject: "src/a.rs".to_string(),
            approved: true,
            approved_by: "asha".to_string(),
            at_ms: 3_000,
        }],
    }
}

/// Attach a git-backed session and return the parsed bundle plus its path.
fn declared_bundle(root: &Path, state: &Path, bundle: &Path) -> (AppContext, Value) {
    let ctx = context(root, state);
    std::fs::create_dir_all(state).unwrap();
    seed(&ctx, "origin");
    let goals = GoalStore::new(&ctx.home);
    goals.set_goal("origin", "land it").unwrap();
    goals.add_step("origin", "keep api stable").unwrap();

    let path = bundle.to_string_lossy().to_string();
    attach(&ctx, "origin", Some(&path), false, false, false).unwrap();
    let text = std::fs::read_to_string(bundle).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    (ctx, value)
}

#[test]
fn manifest_round_trip_preserves_every_field() {
    let manifest = sample_manifest();
    let value = manifest.to_value().unwrap();
    let back: hr::HandoffManifest = serde_json::from_value(value).unwrap();

    assert_eq!(back, manifest);
    assert_eq!(back.schema, hr::HANDOFF_SCHEMA);
    assert_eq!(back.session, "origin");
    assert_eq!(back.goals[0].condition, "land it");
    assert_eq!(back.constraints, vec!["no network", "keep api stable"]);
    assert_eq!(
        back.repository.revision.as_deref(),
        Some("a".repeat(40).as_str())
    );
    assert_eq!(back.attempts.len(), 1);
    assert_eq!(back.verification[0].name, "cargo test");
    assert_eq!(back.pending_approvals[0].approved_by, "asha");
}

#[test]
fn attach_then_handoff_preserves_declared_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    let bundle = tmp.path().join("handoff.bundle.json");
    init_repo(&repo);

    let (ctx, value) = declared_bundle(&repo, &state, &bundle);
    assert_eq!(value["schema"], hr::HANDOFF_SCHEMA);
    assert!(hr::is_declared(&value));

    let manifest = hr::extract(&value).expect("declared bundle carries a manifest");
    assert_eq!(manifest.schema, hr::HANDOFF_SCHEMA);
    assert_eq!(manifest.session, "origin");
    assert_eq!(manifest.goals.len(), 1, "the /goal condition is exported");
    assert_eq!(manifest.goals[0].condition, "land it");
    assert_eq!(
        manifest.constraints,
        vec!["keep api stable"],
        "plan steps are constraints"
    );
    assert!(
        manifest.repository.revision.is_some(),
        "the revision is recorded"
    );
    assert_eq!(manifest.attempts.len(), 1, "the turn receipt is an attempt");
    assert_eq!(manifest.pending_approvals.len(), 1);
    // This build has no verification store, so it must not invent a pass.
    assert!(manifest.verification.is_empty());

    let path = bundle.to_string_lossy().to_string();
    let outcome = handoff(&ctx, &path, Some("landed"), None, false).unwrap();
    assert_eq!(outcome.data["reconciled"], true);
    assert_eq!(outcome.data["schema"], hr::HANDOFF_SCHEMA);

    let landed = SessionStore::new(&ctx.sessions_dir).load("landed").unwrap();
    assert_eq!(landed.messages.len(), 2);
    assert_eq!(landed.receipts.len(), 1);
    assert_eq!(landed.receipts[0].prefix_hash, "abcd1234");
    assert_eq!(landed.approvals.len(), 1, "pending approvals are restored");
    assert_eq!(landed.approvals[0].subject, "src/a.rs");
    assert_eq!(landed.approvals[0].rule, "handoff");
}

#[test]
fn changed_revision_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    let bundle = tmp.path().join("handoff.bundle.json");
    init_repo(&repo);

    let (ctx, _) = declared_bundle(&repo, &state, &bundle);

    std::fs::write(repo.join("seed.txt"), "two\n").unwrap();
    assert!(git(&repo, &["add", "seed.txt"]));
    assert!(git(&repo, &["commit", "-q", "-m", "advance"]));

    let path = bundle.to_string_lossy().to_string();
    let error = format!(
        "{:#}",
        handoff(&ctx, &path, Some("landed"), None, false).unwrap_err()
    );
    assert!(error.contains("revision changed"), "{error}");
    assert!(
        SessionStore::new(&ctx.sessions_dir).load("landed").is_err(),
        "a refused handoff must not write a session"
    );
}

#[test]
fn changed_branch_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    let bundle = tmp.path().join("handoff.bundle.json");
    init_repo(&repo);

    let (ctx, _) = declared_bundle(&repo, &state, &bundle);
    assert!(git(&repo, &["checkout", "-q", "-b", "other"]));

    let path = bundle.to_string_lossy().to_string();
    let error = format!(
        "{:#}",
        handoff(&ctx, &path, Some("landed"), None, false).unwrap_err()
    );
    assert!(error.contains("branch changed"), "{error}");
}

#[test]
fn dirty_tree_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    let bundle = tmp.path().join("handoff.bundle.json");
    init_repo(&repo);

    let (ctx, _) = declared_bundle(&repo, &state, &bundle);

    std::fs::write(repo.join("seed.txt"), "dirty\n").unwrap();
    assert!(hr::capture_repository(&repo).dirty, "the tree is now dirty");

    let path = bundle.to_string_lossy().to_string();
    let error = format!(
        "{:#}",
        handoff(&ctx, &path, Some("landed"), None, false).unwrap_err()
    );
    assert!(error.contains("dirty"), "{error}");
    assert!(SessionStore::new(&ctx.sessions_dir).load("landed").is_err());
}

#[test]
fn stale_verification_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    let bundle = tmp.path().join("handoff.bundle.json");
    init_repo(&repo);

    let (ctx, mut value) = declared_bundle(&repo, &state, &bundle);
    value["handoff"]["verification"] = json!([{
        "name": "cargo test",
        "ok": true,
        "at_ms": 2_000,
        "revision": "0000000000000000000000000000000000000000"
    }]);
    std::fs::write(&bundle, value.to_string()).unwrap();

    let path = bundle.to_string_lossy().to_string();
    let error = format!(
        "{:#}",
        handoff(&ctx, &path, Some("landed"), None, false).unwrap_err()
    );
    assert!(error.contains("stale"), "{error}");
}

#[test]
fn no_hidden_reasoning_in_bundle() {
    let mut value = sample_manifest().to_value().unwrap();
    value["reasoning"] = json!("private chain of thought");
    value["goals"][0]["thinking"] = json!("secret plan");
    value["pending_approvals"][0]["chain_of_thought"] = json!("secret reason");

    hr::strip_hidden_reasoning(&mut value);
    let text = value.to_string();
    for key in hr::HIDDEN_REASONING_KEYS {
        assert!(!text.contains(*key), "hidden reasoning key {key} survived");
    }
    assert!(!text.contains("private chain of thought"));
    assert!(!text.contains("secret plan"));
    assert!(!text.contains("secret reason"));
    // Stripping reasoning must not strip the declared fields.
    assert_eq!(value["schema"], hr::HANDOFF_SCHEMA);
    assert_eq!(value["goals"][0]["condition"], "land it");
    assert_eq!(value["pending_approvals"][0]["approved_by"], "asha");

    // The same holds for a real bundle written by `attach`: no reasoning key
    // is present anywhere in the document.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    let bundle = tmp.path().join("handoff.bundle.json");
    init_repo(&repo);
    let (_, written) = declared_bundle(&repo, &state, &bundle);
    let written_text = written.to_string();
    for key in hr::HIDDEN_REASONING_KEYS {
        assert!(!written_text.contains(*key), "bundle contains {key}");
    }
}

#[test]
fn legacy_bundle_without_revision_still_hands_off() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let bundle = tmp.path().join("legacy.bundle.json");
    // A non-git root has no revision, so `attach` writes the legacy shape.
    let ctx = context(&state, &state);
    std::fs::create_dir_all(&state).unwrap();
    seed(&ctx, "origin");
    let path = bundle.to_string_lossy().to_string();
    attach(&ctx, "origin", Some(&path), false, false, false).unwrap();

    let text = std::fs::read_to_string(&bundle).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["version"], 1);
    assert!(
        value.get("schema").is_none(),
        "legacy bundle has no declared schema"
    );
    assert!(
        hr::extract(&value).is_none(),
        "legacy bundle has no manifest"
    );

    let outcome = handoff(&ctx, &path, Some("landed"), None, false).unwrap();
    assert_eq!(outcome.data["reconciled"], false);
    assert_eq!(
        SessionStore::new(&ctx.sessions_dir)
            .load("landed")
            .unwrap()
            .messages
            .len(),
        2
    );
}

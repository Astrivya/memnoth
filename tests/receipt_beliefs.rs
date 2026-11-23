//! EV-02: a receipt belief carries a real predicate and an honest provenance.
//!
//! These are integrity checks, not claims of cryptographic authenticity: a
//! receipt says what the vault recorded and what `receipt verify` can still
//! check. A field nobody recorded stays `None` — the "not measured" value — and
//! a belief whose provenance is partial is a failed check, not a signed fact.

use std::path::Path;

use memnoth::receipt::{self, Belief};
use memnoth::{AppContext, Config};
use memnoth_engine::{EntityKind, MemoryEngine, now_ms};

fn fixture_receipt(commit: &str) -> receipt::Receipt {
    receipt::Receipt {
        schema: receipt::SCHEMA.to_string(),
        change: receipt::Change {
            commit: commit.to_string(),
            base: None,
            patch_sha256: "0".repeat(64),
            files: Vec::new(),
        },
        agent: receipt::Agent {
            session: None,
            run_id: None,
            agent: "fixture".to_string(),
            model: None,
        },
        authority: receipt::Authority {
            user: "asha".to_string(),
            org: None,
            policy_version: None,
            sandbox: "policy".to_string(),
            approvals: Vec::new(),
        },
        beliefs: Some(Vec::new()),
        beliefs_unavailable: None,
        impact: receipt::Impact {
            symbols: None,
            references: None,
            impacted_tests: None,
            unavailable: None,
        },
        policy: receipt::Policy {
            denied: Vec::new(),
            rule_that_would_allow: None,
        },
    }
}

fn test_context(root: &Path) -> AppContext {
    AppContext {
        root: root.to_path_buf(),
        home: root.to_path_buf(),
        vault_path: root.join("vault.db"),
        namespace: "default".to_string(),
        graph_path: root.join(".memnoth").join("graph.db"),
        sessions_dir: root.join("sessions"),
        config_path: root.join("config.toml"),
        config: Config::default(),
        json: false,
        quiet: true,
        progress_tx: None,
        exe_path: std::path::PathBuf::from("mn"),
    }
}

fn bare_belief() -> Belief {
    Belief {
        entity: "mystery".to_string(),
        predicate: None,
        text: "something was true".to_string(),
        kind: None,
        author: None,
        agent: None,
        source: None,
        recorded_at: None,
        valid_from: None,
        supersedes: None,
        unknown: vec![
            "author".to_string(),
            "source".to_string(),
            "date".to_string(),
        ],
    }
}

#[test]
fn predicate_is_present_from_a_seeded_belief() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let now = now_ms();
    let id = engine
        .add_entity(EntityKind::Concept, "retry-policy", 1.0, now)
        .expect("entity");

    // Schema v5 stores the predicate on the observation itself.
    engine
        .connection()
        .execute(
            "INSERT INTO observations \
                (entity_id, content, kind, valid_from, recorded_at, predicate, provenance) \
             VALUES (?1, 'cap is 5', 'decision', ?2, ?2, 'retry-cap', ?3)",
            rusqlite::params![id, now, r#"{"user":"asha","agent":"cli"}"#],
        )
        .expect("seed an observation with a predicate");

    let symbols = vec!["retry-policy".to_string()];
    let (beliefs, unavailable) = receipt::beliefs_from_engine(&engine, &symbols, now);
    assert!(unavailable.is_none(), "no query failures: {unavailable:?}");
    assert_eq!(beliefs.len(), 1, "one current fact for the symbol");
    assert_eq!(
        beliefs[0].predicate.as_deref(),
        Some("retry-cap"),
        "the predicate came from the observation, not from the fact text"
    );
    assert_eq!(beliefs[0].text, "cap is 5");
    assert_eq!(
        beliefs[0].author.as_deref(),
        Some("asha"),
        "a cited belief keeps its recorder"
    );

    // An attributed belief with provenance verifies; naming the predicate does
    // not make the check pass on its own.
    let mut receipt = fixture_receipt(&"a".repeat(40));
    receipt.beliefs = Some(beliefs);
    let check = receipt::beliefs_check(&receipt);
    assert!(check.ok, "attributed belief passes: {}", check.detail);
}

#[test]
fn missing_provenance_is_reported_as_unknown_not_failed() {
    let mut receipt = fixture_receipt(&"b".repeat(40));
    receipt.beliefs = Some(vec![bare_belief()]);
    let check = receipt::beliefs_check(&receipt);
    assert!(
        check.ok,
        "no provenance recorded is honest, not a failure: {}",
        check.detail
    );
    assert!(
        check.detail.contains("unknown"),
        "the absence must be said, not implied: {}",
        check.detail
    );

    // The other half of the contract: provenance that exists but omits the
    // person is an integrity failure, not a fact signed by the recorder.
    let mut partial = bare_belief();
    partial.agent = Some("cli".to_string());
    partial.recorded_at = Some(1_000);
    partial.unknown = vec!["author".to_string()];
    receipt.beliefs = Some(vec![partial]);
    let check = receipt::beliefs_check(&receipt);
    assert!(
        !check.ok,
        "provenance without an author must fail: {}",
        check.detail
    );
}

#[test]
fn a_moved_commit_renders_stale() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let ctx = test_context(dir.path());
    let commit = "c".repeat(40);
    let receipt = fixture_receipt(&commit);

    let path = receipt::path_for(&ctx, &commit);
    std::fs::create_dir_all(path.parent().expect("receipt dir")).expect("create receipt dir");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&receipt).expect("serialize"),
    )
    .expect("write receipt");

    // No repository knows this commit — the state after an amend or rebase.
    let verification = receipt::verify(&ctx, Some(&commit)).expect("verify");
    assert!(!verification.ok, "a stale receipt must not verify");
    assert_eq!(verification.bound_commit, commit);
    let exists = verification
        .checks
        .iter()
        .find(|check| check.name == "commit-exists")
        .expect("commit-exists check");
    assert!(!exists.ok);
    assert!(
        exists.detail.contains("moved"),
        "staleness must be named: {}",
        exists.detail
    );
    let not_checked = verification
        .checks
        .iter()
        .filter(|check| check.detail.starts_with("not checked:"))
        .count();
    assert_eq!(
        not_checked, 2,
        "patch and files are reported as uncheckable, not silently passed"
    );
}

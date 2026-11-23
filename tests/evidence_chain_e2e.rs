//! One hermetic end-to-end observation of the full evidence chain, driven
//! through the shipped binary.
//!
//! This is an **observation artifact** for the evidence-chain positioning. It
//! is not a team-memory claim, not a benchmark, and not an authenticity
//! attestation: every field is either measured from the real binary/real git or
//! named as a fixture seed. The JSON transcript (`mn-evidence-chain/1`) is
//! written next to the temp workspace and its path is printed at the end.
//!
//! Chain under test:
//!
//!   propose (provenance {run_id,agent,user,turn,file,line})
//!     -> human approve (named accepter)
//!     -> git commit
//!     -> receipt write (impact + beliefs measured)
//!     -> receipt verify
//!     -> vault asof (both clocks)
//!     -> contradictions (both sides)
//!     -> audit verify
//!     -> tampered audit field detected
//!
//! Every link is asserted: a missing link fails the test rather than being
//! recorded as "partial". The only seeded values are the rich provenance on
//! the proposal row (built with the product's own `VaultSlice` shape) and one
//! observation's `recorded_at`, both written directly into the isolated vault
//! exactly as other CLI fixtures seed temporal state.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use memnoth_engine::{VaultSlice, now_ms};
use rusqlite::params;
use serde_json::{Value, json};

const SCHEMA: &str = "mn-evidence-chain/1";
const ENTITY: &str = "evidence_chain_probe";
const BELIEF: &str = "retry cap is 5";
const RUN_ID: &str = "run-evidence-chain-1";
const AGENT: &str = "proposer-agent";
const TURN: u64 = 7;
const FILE: &str = "src/evidence_chain_probe.rs";
const LINE: usize = 42;
const PROPOSER: &str = "agent-runner";
const ACCEPTER: &str = "human-accepter";
const CORRECTION: &str = "superseded by reviewed evidence";
const DAY_MS: i64 = 86_400_000;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    transcript: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("workspace");
    let home = dir.path().join("state");
    std::fs::create_dir_all(&root).expect("workspace");
    std::fs::create_dir_all(&home).expect("state home");

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let default = std::env::temp_dir().join(format!(
        "mn-evidence-chain-{date}-{}.json",
        std::process::id()
    ));
    let transcript = std::env::var_os("MEMNOTH_EVIDENCE_TRANSCRIPT")
        .map(PathBuf::from)
        .unwrap_or(default);
    if let Some(parent) = transcript.parent() {
        std::fs::create_dir_all(parent).expect("transcript directory");
    }

    Fixture {
        _dir: dir,
        root,
        home,
        transcript,
    }
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

/// One invocation of the real binary, isolated from the developer's machine.
fn run(fx: &Fixture, user: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new(binary());
    cmd.arg("--json").arg("--trust-folder").arg("--root");
    cmd.arg(&fx.root);
    cmd.args(args);
    cmd.current_dir(&fx.root)
        .env("MEMNOTH_HOME", &fx.home)
        .env("MEMNOTH_USER", user)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        .env_remove("MEMNOTH_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("KILO_API_KEY")
        .env_remove("KILOCODE_API_KEY")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run `memnoth {}`: {e}", args.join(" ")))
}

/// Parse the JSON envelope. `audit verify` on a broken chain still prints a
/// success envelope with `data.ok == false` and exits 1, so the exit status is
/// deliberately not checked here; `data()` checks it for the healthy steps.
fn envelope(out: &Output, label: &str) -> Value {
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{label} did not print a JSON envelope: {e}\nstdout: {text}\nstderr: {err}")
    });
    assert_eq!(
        value["ok"].as_bool(),
        Some(true),
        "{label} error envelope on stderr: {err}\nstdout: {text}"
    );
    value
}

fn data(fx: &Fixture, user: &str, args: &[&str]) -> Value {
    let out = run(fx, user, args);
    assert!(
        out.status.success(),
        "`memnoth {} ` exited {:?}\nstderr: {}",
        args.join(" "),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let env = envelope(&out, &format!("memnoth {}", args.join(" ")));
    env["data"].clone()
}

/// A required piece of evidence. Missing or null = a broken link = test fail.
fn need<'a>(evidence: &'a Value, pointer: &str, step: &str) -> &'a Value {
    let found = evidence
        .pointer(pointer)
        .unwrap_or_else(|| panic!("step {step} is missing evidence at {pointer}: {evidence}"));
    assert!(
        !found.is_null(),
        "step {step} has null evidence at {pointer}"
    );
    found
}

fn save(fx: &Fixture, doc: &Value) {
    std::fs::write(
        &fx.transcript,
        serde_json::to_string_pretty(doc).expect("serialize transcript"),
    )
    .expect("write transcript");
}

fn add_step(doc: &mut Value, fx: &Fixture, id: &str, command: String, evidence: Value) {
    assert!(
        evidence.is_object(),
        "step {id} must carry an evidence object"
    );
    doc["steps"]
        .as_array_mut()
        .expect("steps array")
        .push(json!({ "id": id, "command": command, "ok": true, "evidence": evidence }));
    save(fx, doc);
}

fn git(fx: &Fixture, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(&fx.root)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run `git {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn open_vault(fx: &Fixture) -> rusqlite::Connection {
    rusqlite::Connection::open(fx.home.join("vault.db")).expect("open isolated vault")
}

#[test]
fn the_full_evidence_chain_is_observable_end_to_end() {
    let fx = fixture();
    let mut doc = json!({
        "schema": SCHEMA,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
        "artifact": "observation for evidence-chain positioning",
        "not_a_claim": [
            "not team memory",
            "not a benchmark",
            "not an authenticity or signing attestation"
        ],
        "fixture_seeds": [
            "rich provenance is seeded onto the proposal row with memnoth_engine::VaultSlice's own shape",
            "one observation's recorded_at is moved so the second clock has something to exclude"
        ],
        "paths": {
            "workspace": fx.root.display().to_string(),
            "vault": fx.home.join("vault.db").display().to_string(),
            "graph": fx.root.join(".memnoth").join("graph.db").display().to_string(),
            "audit": fx.home.join("audit").join("default.jsonl").display().to_string(),
        },
        "links": [],
        "steps": [],
    });
    save(&fx, &doc);

    // 1. Vault exists before any belief does.
    let init = data(&fx, "chain-runner", &["vault", "init"]);
    assert!(
        fx.home.join("vault.db").is_file(),
        "vault init made the vault"
    );
    add_step(
        &mut doc,
        &fx,
        "vault_init",
        "memnoth --json vault init".to_string(),
        json!({ "data": init, "vault_exists": true }),
    );

    // 2. Agent proposes the belief. The rich six-field provenance is the
    // product's own shape; it is seeded onto the row the real binary created.
    let mut slice = VaultSlice::spawn(RUN_ID, AGENT, TURN).with_user(PROPOSER);
    let index = slice.propose(ENTITY, BELIEF, "fact", 0.9, FILE, LINE);
    let provenance: Value = serde_json::from_str(&slice.proposals()[index].provenance.to_json())
        .expect("provenance JSON");
    for field in ["run_id", "agent", "user", "turn", "file", "line"] {
        assert!(!provenance[field].is_null(), "provenance has {field}");
    }
    let propose = data(
        &fx,
        PROPOSER,
        &["vault", "propose", ENTITY, BELIEF, "--author", PROPOSER],
    );
    let id = need(&propose, "/id", "propose")
        .as_i64()
        .expect("numeric proposal id");
    let id_text = id.to_string();
    {
        let conn = open_vault(&fx);
        conn.execute(
            "UPDATE proposals SET provenance = ?1 WHERE id = ?2",
            params![provenance.to_string(), id],
        )
        .expect("seed full provenance on the proposed belief");
    }
    let proposals = data(&fx, PROPOSER, &["vault", "proposals"]);
    let row = proposals["proposals"]
        .as_array()
        .expect("proposal list")
        .iter()
        .find(|p| p["id"] == id)
        .expect("the proposed row is still listed")
        .clone();
    let stored: Value =
        serde_json::from_str(row["provenance"].as_str().expect("provenance string"))
            .expect("stored provenance parses");
    assert_eq!(
        stored, provenance,
        "the proposal carries the full provenance"
    );
    add_step(
        &mut doc,
        &fx,
        "propose",
        format!(
            "memnoth --json vault propose {ENTITY} --author {PROPOSER} -- (provenance seeded from VaultSlice)"
        ),
        json!({
            "proposal_id": id,
            "entity": ENTITY,
            "belief": BELIEF,
            "provenance": provenance,
            "stored_provenance": stored,
            "proposals_view": row,
        }),
    );

    // 3. A named human accepts it; the proposer's attribution survives and the
    // accepter is added beside it.
    let approve = data(
        &fx,
        ACCEPTER,
        &["vault", "approve", &id_text, "--by", ACCEPTER],
    );
    assert_eq!(approve["status"], "accepted");
    assert_eq!(approve["approved_by"], ACCEPTER);
    let observation_id = need(&approve, "/observation_id", "approve")
        .as_i64()
        .expect("numeric observation id");
    let observed_provenance: Value = {
        let conn = open_vault(&fx);
        let raw: String = conn
            .query_row(
                "SELECT provenance FROM observations WHERE id = ?1",
                params![observation_id],
                |r| r.get(0),
            )
            .expect("accepted observation exists");
        serde_json::from_str(&raw).expect("observation provenance parses")
    };
    assert_eq!(observed_provenance["accepted_by"], ACCEPTER);
    for field in ["run_id", "agent", "user", "turn", "file", "line"] {
        assert_eq!(
            observed_provenance[field], provenance[field],
            "acceptance preserved {field}"
        );
    }
    add_step(
        &mut doc,
        &fx,
        "approve",
        format!("memnoth --json vault approve {id} --by {ACCEPTER}"),
        json!({
            "proposal_id": id,
            "observation_id": observation_id,
            "approved_by": ACCEPTER,
            "status": approve["status"],
            "observation_provenance": observed_provenance,
        }),
    );

    // 4. A real code change is committed in an isolated git repository.
    git(&fx, &["init", "-q"]);
    git(&fx, &["config", "user.email", "chain@example.invalid"]);
    git(&fx, &["config", "user.name", "Evidence Chain"]);
    std::fs::write(fx.root.join("README.md"), "evidence chain fixture\n").expect("README");
    // The base revision already mentions the symbol the next commit defines,
    // so the graph can measure real references and impacted tests, not zeros.
    std::fs::create_dir_all(fx.root.join("tests")).expect("tests dir");
    std::fs::write(
        fx.root.join("tests").join("probe_test.rs"),
        "#[test]\nfn probe_mentions_symbol() {\n    let _ = crate::evidence_chain_probe();\n}\n",
    )
    .expect("test file");
    git(&fx, &["add", "-A"]);
    git(&fx, &["commit", "-q", "-m", "base"]);
    let base = git(&fx, &["rev-parse", "HEAD"]);
    std::fs::create_dir_all(fx.root.join("src")).expect("src dir");
    let source = "pub fn evidence_chain_probe() -> u32 {\n    42\n}\n";
    std::fs::write(fx.root.join(FILE), source).expect("source file");
    git(&fx, &["add", "-A"]);
    git(&fx, &["commit", "-q", "-m", "add evidence chain probe"]);
    let head = git(&fx, &["rev-parse", "HEAD"]);
    assert_ne!(base, head, "the change produced a new commit");
    add_step(
        &mut doc,
        &fx,
        "code_change",
        "git commit -m 'add evidence chain probe'".to_string(),
        json!({
            "base_commit": base,
            "head_commit": head,
            "changed_file": FILE,
            "source": source,
        }),
    );

    // 5. Index the graph so impact can actually be measured at HEAD.
    let index_report = data(&fx, "chain-runner", &["kg", "index"]);
    let search = data(&fx, "chain-runner", &["kg", "search", ENTITY]);
    assert!(
        index_report.to_string().contains("symbols"),
        "index report names its symbol count: {index_report}"
    );
    assert!(
        search.to_string().contains(ENTITY),
        "the graph knows the changed symbol: {search}"
    );
    add_step(
        &mut doc,
        &fx,
        "kg_index",
        "memnoth --json kg index && memnoth --json kg search evidence_chain_probe".to_string(),
        json!({ "index": index_report, "symbol_search": search, "symbol": ENTITY }),
    );

    // 6. The receipt measures both the code impact and the approved belief.
    let receipt = data(&fx, "chain-runner", &["receipt", "write"]);
    assert_eq!(
        need(&receipt, "/change/commit", "receipt_write"),
        &json!(head)
    );
    assert_eq!(receipt["schema"], json!("mn-receipt/1"));
    assert!(
        receipt["impact"]["unavailable"].is_null(),
        "impact must be measured, not excused: {}",
        receipt["impact"]
    );
    assert!(
        receipt["impact"]["symbols"].as_u64().unwrap_or(0) > 0,
        "symbols were measured: {}",
        receipt["impact"]
    );
    assert!(
        receipt["impact"]["references"].as_u64().unwrap_or(0) > 0,
        "references were measured: {}",
        receipt["impact"]
    );
    let impacted_tests = receipt["impact"]["impacted_tests"]
        .as_array()
        .expect("impacted tests were measured");
    assert!(
        !impacted_tests.is_empty(),
        "the test that mentions the symbol was found: {}",
        receipt["impact"]
    );
    let beliefs = receipt["beliefs"]
        .as_array()
        .expect("beliefs were measured, not left unknown");
    let cited = beliefs
        .iter()
        .find(|b| b["entity"] == ENTITY && b["text"] == BELIEF)
        .unwrap_or_else(|| panic!("the approved belief is in the receipt: {beliefs:?}"));
    assert_eq!(cited["author"], PROPOSER);
    assert_eq!(cited["agent"], AGENT);
    assert_eq!(cited["source"], FILE);
    add_step(
        &mut doc,
        &fx,
        "receipt_write",
        "memnoth --json receipt write".to_string(),
        json!({
            "commit": head,
            "impact": receipt["impact"],
            "beliefs": receipt["beliefs"],
            "cited_belief": cited,
            "receipt": receipt,
        }),
    );

    // 7. The receipt verifies against the commit it describes.
    let verification = data(&fx, "chain-runner", &["receipt", "verify"]);
    assert_eq!(
        need(&verification, "/ok", "receipt_verify").as_bool(),
        Some(true)
    );
    assert_eq!(verification["bound_commit"], json!(head));
    let checks = verification["checks"]
        .as_array()
        .expect("receipt checks are reported");
    assert!(checks.len() >= 7, "every check is present: {checks:?}");
    for check in checks {
        assert_eq!(
            check["ok"].as_bool(),
            Some(true),
            "receipt check {} failed: {}",
            check["name"],
            check["detail"]
        );
    }
    add_step(
        &mut doc,
        &fx,
        "receipt_verify",
        "memnoth --json receipt verify".to_string(),
        json!({ "ok": verification["ok"], "bound_commit": verification["bound_commit"], "checks": checks, "verification": verification }),
    );

    // 8. `vault asof` answers with both clocks. A second fact is recorded after
    // `--knew` so the recorded-time filter has something real to exclude.
    let now = now_ms();
    let at = now + 20 * DAY_MS;
    let knew = now + 5 * DAY_MS;
    let later = now + 10 * DAY_MS;
    let at_text = at.to_string();
    let knew_text = knew.to_string();
    let remembered = data(
        &fx,
        ACCEPTER,
        &["vault", "remember", ENTITY, "later shadow fact"],
    );
    assert_eq!(remembered["content"], "later shadow fact");
    {
        let conn = open_vault(&fx);
        conn.execute(
            "UPDATE observations SET recorded_at = ?1 WHERE content = 'later shadow fact'",
            params![later],
        )
        .expect("move the shadow fact past --knew");
    }
    let asof = data(
        &fx,
        "chain-runner",
        &[
            "vault", "asof", "--name", ENTITY, "--at", &at_text, "--knew", &knew_text,
        ],
    );
    assert_eq!(need(&asof, "/knew", "vault_asof"), &json!(knew));
    assert!(
        need(&asof, "/knew_iso", "vault_asof").is_string(),
        "the second clock is rendered for a person too"
    );
    assert_eq!(asof["at"], json!(at));
    let facts = asof["facts"].as_array().expect("asof facts");
    assert!(
        facts.iter().any(|f| f["content"] == BELIEF),
        "the approved belief was known by --knew: {facts:?}"
    );
    assert!(
        facts.iter().all(|f| f["content"] != "later shadow fact"),
        "a fact recorded after --knew is excluded: {facts:?}"
    );
    let history = data(&fx, "chain-runner", &["vault", "history", ENTITY]);
    let history_text = history.to_string();
    assert!(history_text.contains(RUN_ID), "history keeps run_id");
    assert!(
        history_text.contains(ACCEPTER),
        "history keeps the accepter"
    );
    add_step(
        &mut doc,
        &fx,
        "vault_asof",
        format!("memnoth --json vault asof --name {ENTITY} --at {at} --knew {knew}"),
        json!({
            "at": at,
            "knew": knew,
            "knew_iso": asof["knew_iso"],
            "facts": facts,
            "shadow_fact_recorded_at": later,
            "provenance_in_history": history_text,
            "history": history,
        }),
    );

    // 9. A contradiction keeps both the closed and the current side.
    let correct = data(
        &fx,
        ACCEPTER,
        &["vault", "correct", ENTITY, "--reason", CORRECTION],
    );
    assert_eq!(correct["reason"], CORRECTION);
    let contradictions = data(
        &fx,
        "chain-runner",
        &["vault", "contradictions", "--name", ENTITY],
    );
    let sides = contradictions["contradictions"]
        .as_array()
        .expect("contradictions array");
    assert!(
        !sides.is_empty(),
        "a correction is a recorded contradiction"
    );
    let closed = &sides[0];
    assert!(
        closed["previous"].is_object() || !closed["previous_summary"].is_null(),
        "the previous side carries the closed belief: {closed}"
    );
    assert_eq!(closed["reason"], CORRECTION);
    let current = need(&contradictions, "/current_belief", "contradictions");
    assert!(
        current.is_object() && current["name"] == ENTITY,
        "the current side is returned too: {current}"
    );
    add_step(
        &mut doc,
        &fx,
        "contradictions",
        format!(
            "memnoth --json vault correct {ENTITY} --reason '{CORRECTION}' && memnoth --json vault contradictions --name {ENTITY}"
        ),
        json!({
            "reason": CORRECTION,
            "previous_side": closed["previous"],
            "previous_summary": closed["previous_summary"],
            "current_side": current,
            "report": contradictions,
        }),
    );

    // 10. The hash-chained audit trail verifies before any tampering.
    let audit_path = fx.home.join("audit").join("default.jsonl");
    assert!(audit_path.is_file(), "the correction wrote an audit event");
    let audit_pass = data(&fx, "chain-runner", &["audit", "verify"]);
    assert_eq!(
        need(&audit_pass, "/ok", "audit_verify_pass").as_bool(),
        Some(true)
    );
    assert!(
        audit_pass["events"].as_u64().unwrap_or(0) >= 1,
        "a real event was sealed: {audit_pass}"
    );
    add_step(
        &mut doc,
        &fx,
        "audit_verify_pass",
        "memnoth --json audit verify".to_string(),
        json!({
            "path": audit_path.display().to_string(),
            "events": audit_pass["events"],
            "verification": audit_pass,
        }),
    );

    // 11. Editing one sealed field is detected by the same verifier.
    let original = std::fs::read_to_string(&audit_path).expect("read audit log");
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    assert!(!lines.is_empty(), "there is an event to tamper with");
    let mut first: Value = serde_json::from_str(&lines[0]).expect("audit event is JSON");
    let before = first["target"].clone();
    first["target"] = json!("tampered-by-evidence-chain-test");
    let after = first["target"].clone();
    lines[0] = serde_json::to_string(&first).expect("serialize tampered event");
    std::fs::write(&audit_path, format!("{}\n", lines.join("\n"))).expect("write tampered log");
    let tampered_out = run(&fx, "chain-runner", &["audit", "verify"]);
    let tampered_env = envelope(&tampered_out, "memnoth --json audit verify (tampered)");
    let tampered = &tampered_env["data"];
    assert_eq!(
        tampered["ok"].as_bool(),
        Some(false),
        "the tamper must be detected: {tampered}"
    );
    assert_eq!(tampered["first_bad_seq"].as_u64(), Some(1));
    assert!(
        tampered["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("hash"),
        "the verifier names the broken seal: {}",
        tampered["reason"]
    );
    add_step(
        &mut doc,
        &fx,
        "audit_tamper_detected",
        "edit audit/default.jsonl target field; memnoth --json audit verify".to_string(),
        json!({
            "tampered_field": "target",
            "before": before,
            "after": after,
            "first_bad_seq": tampered["first_bad_seq"],
            "reason": tampered["reason"],
            "verification": tampered,
        }),
    );

    // Every link is now present. The transcript records the links explicitly,
    // and the completeness check fails the test if a step is absent.
    doc["links"] = json!([
        { "from": "propose", "to": "approve", "via": "proposal_id + full provenance", "state": "measured" },
        { "from": "approve", "to": "receipt_write", "via": "observation in the vault, cited belief", "state": "measured" },
        { "from": "code_change", "to": "receipt_write", "via": "HEAD commit", "state": "measured" },
        { "from": "receipt_write", "to": "receipt_verify", "via": "bound commit + patch + files", "state": "measured" },
        { "from": "approve", "to": "vault_asof", "via": "both clocks over the same observation", "state": "measured" },
        { "from": "vault_asof", "to": "contradictions", "via": "previous side vs current side", "state": "measured" },
        { "from": "contradictions", "to": "audit_verify_pass", "via": "sealed supersede event", "state": "measured" },
        { "from": "audit_verify_pass", "to": "audit_tamper_detected", "via": "changed field breaks the seal", "state": "measured" },
    ]);
    let expected = [
        "vault_init",
        "propose",
        "approve",
        "code_change",
        "kg_index",
        "receipt_write",
        "receipt_verify",
        "vault_asof",
        "contradictions",
        "audit_verify_pass",
        "audit_tamper_detected",
    ];
    let steps = doc["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), expected.len(), "every link produced a step");
    for id in expected {
        assert!(
            steps
                .iter()
                .any(|s| s["id"] == id && s["ok"].as_bool() == Some(true)),
            "link step {id} is missing from the transcript"
        );
    }
    assert_eq!(doc["schema"], json!(SCHEMA));
    save(&fx, &doc);
    eprintln!(
        "evidence chain transcript: {}",
        Path::new(&fx.transcript).display()
    );
}

//! **Multi Round-Trip Requests** — the in-spec confirmation path for writes
//! (SEP-2322, the 2026-07-28 revision).
//!
//! Before this revision, a server asked the user something by opening a
//! server-initiated request on a stream. That pattern is **gone** — this
//! revision made it a breaking change:
//!
//! > Servers MUST send server-to-client requests (such as `roots/list`,
//! > `sampling/createMessage`, or `elicitation/create`) using the MRTR pattern.
//! > The previous pattern of server-initiated requests is no longer supported.
//!
//! So confirmation works like this: the server answers the *original* request
//! with `resultType: "input_required"`, the client gathers the answer and
//! **retries the same call** carrying `inputResponses` plus the `requestState`
//! we handed it. There is no session and no pending-request table on our side —
//! everything needed on the retry travels through the client, signed (see
//! `memnoth_mcp_auth::state`, which is where the integrity requirement lives).
//!
//! # What this gates, and why this one
//!
//! `mem_ingest` — bulk ingestion into the shared vault. It is the one write
//! where a wrong argument is both easy to produce and expensive to undo: a model
//! that resolves a path badly does not add one wrong fact, it adds thousands.
//! Facts are invalidated rather than deleted, so "undo" means issuing a
//! compensating invalidation for every row.
//!
//! Gating *reads*, or every single `mem_add_fact`, would be security theatre
//! with an excellent chance of training hosts to auto-accept. The rule here is:
//! confirm when the write is bulk and hard to reverse, not when it is small and
//! cheap.
//!
//! Deliberately postponed as `L-MCP-22`:
//!
//! * **Per-tool scope enforcement.** `AuthContext.scopes` exists and the auth
//!   spec's `insufficient_scope` (403) challenge is not emitted, because no tool
//!   declares a required scope yet. Emitting a challenge for a scope nothing
//!   checks would be a lie.
//! * **Principal binding is only as good as the transport.** We bind the state
//!   to the authenticated `sub` when there is one (HTTP bearer). On stdio there
//!   is no principal to bind — the process *is* the user — so the claim is
//!   omitted rather than filled with a fake identity.

use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

pub use memnoth_mcp_auth::state::{self, STATE_TTL_SECS};

/// `resultType` value that marks an interim result needing more input.
pub const INPUT_REQUIRED: &str = "input_required";
/// The only request kind we send. `sampling/createMessage` and `roots/list` are
/// either deprecated (SEP-2577) or something we do not need.
pub const ELICIT_METHOD: &str = "elicitation/create";
/// Elicitation mode: a form the host renders, as opposed to a URL hand-off.
pub const FORM_MODE: &str = "form";
/// Key inside `inputRequests`/`inputResponses` for our approval question.
pub const CONFIRM_KEY: &str = "confirm_bulk_write";

/// Environment switch: `MEMNOTH_MCP_WRITE_APPROVAL=off` answers such writes
/// without asking. Anything else (including unset) enables the prompt for hosts
/// that can answer it.
pub const APPROVAL_ENV: &str = "MEMNOTH_MCP_WRITE_APPROVAL";

/// Writes that require an explicit confirmation when the host can give one.
///
/// One entry, and it is a fact rather than a policy knob: expanding this list
/// expands how many of a user's actions can be interrupted, so it should grow
/// only when a write is bulk and irreversible.
pub const CONFIRMED_WRITE_TOOLS: &[&str] =
    &["mem_ingest", "mem_merge", "mem_supersede", "mem_forget"];

/// How many nonces we remember for single-use enforcement. Bounded so a
/// long-lived server cannot grow without limit; at 15-minute TTLs and human
/// approval rates, 4096 outstanding confirmations is far beyond any real load.
const NONCE_LOG_CAP: usize = 4096;

pub fn enabled() -> bool {
    !matches!(
        std::env::var(APPROVAL_ENV).as_deref(),
        Ok("0") | Ok("off") | Ok("false") | Ok("no")
    )
}

/// What the caller should do with a `tools/call` that may need confirmation.
#[derive(Debug)]
pub enum Decision {
    /// Nothing to confirm, or confirmation already satisfied: run the tool.
    Proceed,
    /// Answer the call with this `InputRequiredResult` instead of running it.
    Ask(Value),
    /// Do not run the tool, and say why. Covers both a human declining and a
    /// `requestState` that failed verification — the outcome is the same (no
    /// write) but the message is not, because one is a stale/forged token and
    /// the other is a decision.
    Deny(String),
}

/// Is this the kind of write that gets a confirmation prompt at all?
pub fn is_confirmed_write(tool: &str) -> bool {
    CONFIRMED_WRITE_TOOLS.contains(&tool)
}

/// Does this request declare elicitation support?
///
/// The spec is a MUST: "Servers MUST NOT send an `inputRequests` that the client
/// has not declared support for in its capabilities." A client that says nothing
/// (every legacy client, and any modern one that did not opt in) simply gets the
/// old synchronous behaviour — it is never asked a question it cannot answer.
pub fn client_declares_elicitation(params: &Value) -> bool {
    params
        .get("_meta")
        .and_then(|meta| meta.get(crate::META_CLIENT_CAPABILITIES))
        .and_then(|caps| caps.get("elicitation"))
        .is_some()
}

/// Decide how to handle a possibly-confirmed write.
///
/// Called on every `tools/call`, including retries — which is the whole point:
/// the retry is an ordinary, self-contained request, and this function is
/// stateless apart from the nonce log.
pub fn decision(tool: &str, args: &Value, params: &Value, principal: Option<&str>) -> Decision {
    if !enabled() || !is_confirmed_write(tool) {
        return Decision::Proceed;
    }

    let Some(request_state) = params.get("requestState").and_then(Value::as_str) else {
        // First attempt. Ask only if the host can be asked.
        if !client_declares_elicitation(params) {
            return Decision::Proceed;
        }
        return ask(tool, args, principal);
    };

    let claims = match state::verify(state::process_secret(), request_state) {
        Ok(claims) => claims,
        Err(error) => {
            // The spec: state that fails verification MUST be rejected. We do
            // not fall back to asking again, because doing so would let a
            // tampered token turn into a fresh prompt.
            return Decision::Deny(error.message().to_string());
        }
    };

    // The identifier of the originating request: "rejecting state presented on a
    // request that does not match". Comparing canonical arguments (not raw JSON
    // text) keeps this from rejecting a client that re-serialised them.
    let expected_args = canonical(args);
    if claims["req"]["tool"].as_str() != Some(tool) || claims["req"]["args"] != expected_args {
        return Decision::Deny(
            "requestState was issued for a different request; nothing was written".to_string(),
        );
    }

    // Bound to the authenticated principal when there is one.
    if let Some(expected) = claims["sub"].as_str()
        && Some(expected) != principal
    {
        return Decision::Deny(
            "requestState was issued to a different principal; nothing was written".to_string(),
        );
    }

    let Some(nonce) = claims["jti"].as_str() else {
        return Decision::Deny("requestState is missing its identifier".to_string());
    };
    if !consume_nonce(nonce) {
        // Single-use: the spec notes that expiry + binding "do not by themselves
        // guarantee single-use", and that servers needing it MUST enforce it
        // server-side. For a write, replay is a duplicate write.
        return Decision::Deny(
            "requestState has already been used; nothing was written".to_string(),
        );
    }

    if approved(params.get("inputResponses")) {
        return Decision::Proceed;
    }
    Decision::Deny(
        "the bulk write was not confirmed, so nothing was written. Re-issue the call to be asked \
         again"
            .to_string(),
    )
}

/// Build the `InputRequiredResult` for a bulk write.
fn ask(tool: &str, args: &Value, principal: Option<&str>) -> Decision {
    let target = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("<path missing>");

    let claims = claims_for(tool, args, principal);
    let request_state = match state::sign(state::process_secret(), &claims) {
        Ok(token) => token,
        // Fail closed: an unverifiable state would mean asking again forever, or
        // worse, trusting the retry's say-so. Refusing the write is the safe end.
        Err(error) => return Decision::Deny(format!("cannot sign requestState: {error}")),
    };

    // The message is the product here. A prompt that says "confirm?" gets
    // rubber-stamped; one that names the target, the destination and the
    // irreversibility gets read.
    let message = format!(
        "Approve ingesting '{target}' into the memnoth vault? This writes every file it finds into \
         the shared memory vault. Facts are invalidated rather than deleted, so this cannot be \
         undone by calling something else."
    );
    let requested_schema = json!({
        "type": "object",
        "properties": {
            "confirm": {
                "type": "boolean",
                "description": "Set true to perform the ingest, false to skip it",
            }
        },
        "required": ["confirm"],
    });

    Decision::Ask(json!({
        "resultType": INPUT_REQUIRED,
        "inputRequests": {
            CONFIRM_KEY: {
                "method": ELICIT_METHOD,
                "params": {
                    "mode": FORM_MODE,
                    "message": message,
                    "requestedSchema": requested_schema,
                },
            }
        },
        "requestState": request_state,
    }))
}

fn claims_for(tool: &str, args: &Value, principal: Option<&str>) -> Value {
    let mut claims = serde_json::Map::new();
    // Omitted rather than null when there is no principal: the verifier treats
    // "absent" as "not bound", and a null would have to be special-cased.
    if let Some(principal) = principal {
        claims.insert("sub".to_string(), json!(principal));
    }
    claims.insert("jti".to_string(), json!(next_nonce()));
    claims.insert(
        "exp".to_string(),
        json!(memnoth_engine::now_ms() / 1000 + STATE_TTL_SECS),
    );
    claims.insert(
        "req".to_string(),
        json!({"tool": tool, "args": canonical(args)}),
    );
    Value::Object(claims)
}

/// The approval answer. Both halves are required: `action: "accept"` says the
/// host rendered the form and the user did not dismiss it, and `confirm: true`
/// is the user actually ticking the box. Accepting a form whose checkbox
/// defaulted to true is not consent.
fn approved(responses: Option<&Value>) -> bool {
    let Some(answer) = responses.and_then(|r| r.get(CONFIRM_KEY)) else {
        return false;
    };
    answer["action"].as_str() == Some("accept") && answer["content"]["confirm"] == json!(true)
}

/// Keys sorted recursively, so the state's digest of "the request" does not
/// depend on the order a client happened to serialise its arguments in.
pub fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                sorted.insert(key.clone(), canonical(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn nonces() -> &'static Mutex<(HashSet<String>, VecDeque<String>)> {
    static NONCES: OnceLock<Mutex<(HashSet<String>, VecDeque<String>)>> = OnceLock::new();
    NONCES.get_or_init(|| Mutex::new((HashSet::new(), VecDeque::new())))
}

fn next_nonce() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        memnoth_engine::now_ms(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Record a nonce as used. `false` means it was already used — a replay.
fn consume_nonce(nonce: &str) -> bool {
    let mut log = nonces().lock().unwrap_or_else(|p| p.into_inner());
    if log.0.contains(nonce) {
        return false;
    }
    log.0.insert(nonce.to_string());
    log.1.push_back(nonce.to_string());
    while log.1.len() > NONCE_LOG_CAP {
        if let Some(evicted) = log.1.pop_front() {
            log.0.remove(&evicted);
        }
    }
    true
}

/// Test seam.
#[cfg(test)]
pub fn reset_nonces() {
    let mut log = nonces().lock().unwrap_or_else(|p| p.into_inner());
    log.0.clear();
    log.1.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that touch the process-global nonce log.
    ///
    /// `nonces()` is global on purpose — the log has to outlive any single request — and seven
    /// tests here reset it and then assert on it. Under the default parallel runner one test's
    /// `reset_nonces()` can land between another's `Proceed` and its replay, so the replay finds
    /// an empty log and the single-use property looks broken. It is not broken: this module passes
    /// 12/12 with `--test-threads=1`. The mutex removes that false red without weakening a single
    /// assertion — which matters, because a flaky security test is one people learn to re-run
    /// instead of read.
    static NONCE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Hold this for the body of any test that resets or reads the nonce log.
    fn nonce_guard() -> std::sync::MutexGuard<'static, ()> {
        NONCE_TESTS.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn declared() -> Value {
        json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {"elicitation": {}},
            }
        })
    }

    fn args() -> Value {
        json!({"path": "/repo"})
    }

    /// Pull the state back out of an `Ask`, as a client would.
    fn state_of(decision: &Decision) -> String {
        match decision {
            Decision::Ask(result) => result["requestState"].as_str().unwrap().to_string(),
            other => panic!("expected an approval prompt, got {other:?}"),
        }
    }

    #[test]
    fn a_client_that_cannot_be_asked_is_never_asked() {
        // Legacy (no _meta) and modern-without-the-capability both proceed.
        assert!(matches!(
            decision("mem_ingest", &args(), &json!({"name": "mem_ingest"}), None),
            Decision::Proceed
        ));
        let modern_no_caps = json!({"_meta": {"io.modelcontextprotocol/clientCapabilities": {}}});
        assert!(matches!(
            decision("mem_ingest", &args(), &modern_no_caps, None),
            Decision::Proceed
        ));
    }

    #[test]
    fn tools_that_are_not_bulk_writes_are_untouched() {
        for tool in ["mem_add_fact", "mem_search", "kg_index", "kg_explore"] {
            assert!(
                matches!(
                    decision(tool, &args(), &declared(), None),
                    Decision::Proceed
                ),
                "{tool} must not be interrupted"
            );
        }
    }

    #[test]
    fn an_askable_client_gets_a_form_naming_the_target_and_the_stakes() {
        match decision("mem_ingest", &args(), &declared(), None) {
            Decision::Ask(result) => {
                assert_eq!(result["resultType"], INPUT_REQUIRED);
                let request = &result["inputRequests"][CONFIRM_KEY];
                assert_eq!(request["method"], "elicitation/create");
                assert_eq!(request["params"]["mode"], "form");
                let message = request["params"]["message"].as_str().unwrap();
                assert!(
                    message.contains("/repo"),
                    "the prompt must name the target: {message}"
                );
                assert!(
                    message.contains("cannot be undone"),
                    "the prompt must state the stakes: {message}"
                );
                assert_eq!(
                    request["params"]["requestedSchema"]["properties"]["confirm"]["type"],
                    "boolean"
                );
                assert_eq!(
                    request["params"]["requestedSchema"]["required"][0],
                    "confirm"
                );
                assert!(
                    result["requestState"].is_string(),
                    "state must be signed, never absent"
                );
            }
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    #[test]
    fn accepting_the_form_runs_the_write() {
        let _guard = nonce_guard();
        reset_nonces();
        let state = state_of(&decision("mem_ingest", &args(), &declared(), None));
        let retry = json!({
            "name": "mem_ingest",
            "arguments": {"path": "/repo"},
            "requestState": state,
            "inputResponses": {CONFIRM_KEY: {"action": "accept", "content": {"confirm": true}}},
        });
        assert!(matches!(
            decision("mem_ingest", &args(), &retry, None),
            Decision::Proceed
        ));
    }

    #[test]
    fn accepting_without_ticking_the_box_does_not_write() {
        let _guard = nonce_guard();
        reset_nonces();
        let state = state_of(&decision("mem_ingest", &args(), &declared(), None));
        for response in [
            json!({"action": "accept", "content": {}}),
            json!({"action": "accept", "content": {"confirm": false}}),
            json!({"action": "decline"}),
            json!({"action": "cancel"}),
        ] {
            let retry = json!({
                "requestState": state,
                "inputResponses": {CONFIRM_KEY: response},
            });
            assert!(
                matches!(
                    decision("mem_ingest", &args(), &retry, None),
                    Decision::Deny(_)
                ),
                "an unanswered or declined form must not write: {response}"
            );
        }
    }

    #[test]
    fn an_approval_is_single_use() {
        let _guard = nonce_guard();
        reset_nonces();
        let state = state_of(&decision("mem_ingest", &args(), &declared(), None));
        let retry = json!({
            "requestState": state,
            "inputResponses": {CONFIRM_KEY: {"action": "accept", "content": {"confirm": true}}},
        });
        assert!(matches!(
            decision("mem_ingest", &args(), &retry, None),
            Decision::Proceed
        ));
        // Replaying the identical retry — the reason single-use is enforced
        // rather than merely bounded by the TTL.
        match decision("mem_ingest", &args(), &retry, None) {
            Decision::Deny(message) => assert!(message.contains("already been used"), "{message}"),
            other => panic!("a replayed approval must be refused, got {other:?}"),
        }
    }

    #[test]
    fn an_approval_cannot_be_redirected_to_another_target() {
        let _guard = nonce_guard();
        reset_nonces();
        let state = state_of(&decision("mem_ingest", &args(), &declared(), None));
        // Same signed state, different arguments: the classic confused-deputy
        // move, where a confirmed approval is pointed at something else.
        let retry = json!({
            "requestState": state,
            "inputResponses": {CONFIRM_KEY: {"action": "accept", "content": {"confirm": true}}},
        });
        match decision("mem_ingest", &json!({"path": "/etc"}), &retry, None) {
            Decision::Deny(message) => {
                assert!(message.contains("different request"), "{message}");
                assert!(message.contains("nothing was written"), "{message}");
            }
            other => panic!("a redirected approval must be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_forged_state_is_refused_rather_than_re_prompted() {
        let _guard = nonce_guard();
        reset_nonces();
        let retry = json!({
            "requestState": "not.a.token",
            "inputResponses": {CONFIRM_KEY: {"action": "accept", "content": {"confirm": true}}},
        });
        match decision("mem_ingest", &args(), &retry, None) {
            Decision::Deny(message) => {
                assert!(message.contains("failed verification"), "{message}")
            }
            other => panic!("forged state must be refused, got {other:?}"),
        }
    }

    #[test]
    fn state_is_bound_to_the_principal_when_there_is_one() {
        let _guard = nonce_guard();
        reset_nonces();
        let state = state_of(&decision("mem_ingest", &args(), &declared(), Some("alice")));
        let retry = json!({
            "requestState": state,
            "inputResponses": {CONFIRM_KEY: {"action": "accept", "content": {"confirm": true}}},
        });
        match decision("mem_ingest", &args(), &retry, Some("bob")) {
            Decision::Deny(message) => {
                assert!(message.contains("different principal"), "{message}")
            }
            other => panic!("cross-principal reuse must be refused, got {other:?}"),
        }
        assert!(matches!(
            decision("mem_ingest", &args(), &retry, Some("alice")),
            Decision::Proceed
        ));
    }

    #[test]
    fn the_default_is_to_ask() {
        // Only asserts the default: mutating the process environment here would
        // race every other test in this binary, and the off-switch itself is the
        // same `matches!` shape as the Tasks switch (see `tasks::enabled`).
        assert!(
            enabled(),
            "write approval must be on by default; only an explicit opt-out turns it off"
        );
    }

    #[test]
    fn canonical_form_ignores_key_order() {
        let a = canonical(&json!({"b": 1, "a": {"d": 2, "c": [ {"z": 1, "y": 2} ]}}));
        let b = canonical(&json!({"a": {"c": [ {"y": 2, "z": 1} ], "d": 2}, "b": 1}));
        assert_eq!(
            a, b,
            "a client that reorders its arguments must still match"
        );
        assert_eq!(
            a,
            json!({"a": {"c": [{"y": 2, "z": 1}], "d": 2}, "b": 1}),
            "and the canonical form is stable"
        );
    }

    #[test]
    fn nonce_log_is_bounded() {
        let _guard = nonce_guard();
        reset_nonces();
        for i in 0..(NONCE_LOG_CAP + 10) {
            assert!(consume_nonce(&format!("n-{i}")));
        }
        let len = nonces().lock().unwrap().1.len();
        // The log is process-global and the rest of this suite runs in parallel,
        // so concurrent tests legitimately evict entries mid-loop: the
        // invariant is the *bound*, not an exact size.
        assert!(
            len <= NONCE_LOG_CAP,
            "the log must not grow without bound (len {len})"
        );
    }
}

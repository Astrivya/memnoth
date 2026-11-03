//! Extracted from the inline test module; see the parent module for context.
use super::*;
use crate::Profile;
use memnoth_harness_api::{HistoryMessage, SessionInfo};
use tokio::io::{AsyncRead, AsyncWrite};

/// Scripted harness-API server: Hello→HelloOk, CreateSession→Attached
/// ("s-1"), SendMessage→Ok then streams TextDeltas + TurnDone, SetModel→Ok.
/// Listens on the platform transport so the bridge is exercised end-to-end.
pub(crate) async fn spawn_fake_api() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "memnoth-agent-api-{}-{}-{seq}.sock",
        std::process::id(),
        memnoth_engine::now_ms()
    ));
    let mut listener = memnoth_transport::Listener::bind(&path).expect("bind fake api");
    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_fake_conn(stream));
            }
        }
    });
    path
}

async fn serve_fake_conn(stream: impl AsyncRead + AsyncWrite + Unpin) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    loop {
        let frame: ClientFrame = match read_frame_async(&mut reader).await {
            Ok(Some(f)) => f,
            _ => return,
        };
        let reply_id = frame.id;
        match &frame.request {
            ApiRequest::Hello { .. } => {
                write_client_frame(
                    &mut writer,
                    &ServerFrame::reply(
                        reply_id,
                        ApiEvent::HelloOk {
                            version: 1,
                            server: "fake-harness/0.0.1".into(),
                            capabilities: vec![],
                        },
                    ),
                )
                .await
                .unwrap();
            }
            ApiRequest::CreateSession { .. } => {
                write_client_frame(
                    &mut writer,
                    &ServerFrame::reply(
                        reply_id,
                        ApiEvent::Attached {
                            session: SessionInfo {
                                session_id: "s-1".into(),
                                working_dir: None,
                                title: None,
                                status: "idle".into(),
                                transcript_bytes: None,
                                archived: false,
                                archived_at_ms: None,
                            },
                        },
                    ),
                )
                .await
                .unwrap();
            }
            ApiRequest::SendMessage { session_id, .. } => {
                write_client_frame(&mut writer, &ServerFrame::reply(reply_id, ApiEvent::Ok))
                    .await
                    .unwrap();
                write_client_frame(
                    &mut writer,
                    &ServerFrame::event(ApiEvent::MessageAccepted {
                        session_id: session_id.clone(),
                    }),
                )
                .await
                .unwrap();
                write_client_frame(
                    &mut writer,
                    &ServerFrame::event(ApiEvent::TextDelta {
                        session_id: session_id.clone(),
                        text: "The final answer is 42.\n".into(),
                    }),
                )
                .await
                .unwrap();
                write_client_frame(
                    &mut writer,
                    &ServerFrame::event(ApiEvent::ToolDone {
                        session_id: session_id.clone(),
                        call_id: "c1".into(),
                        name: "bash".into(),
                        output: "42".into(),
                        error: None,
                    }),
                )
                .await
                .unwrap();
                write_client_frame(
                    &mut writer,
                    &ServerFrame::event(ApiEvent::TurnDone {
                        session_id: session_id.clone(),
                    }),
                )
                .await
                .unwrap();
                write_client_frame(
                    &mut writer,
                    &ServerFrame::event(ApiEvent::SessionStatus {
                        session_id: session_id.clone(),
                        status: "idle".into(),
                    }),
                )
                .await
                .unwrap();
            }
            ApiRequest::SetModel { .. } => {
                write_client_frame(&mut writer, &ServerFrame::reply(reply_id, ApiEvent::Ok))
                    .await
                    .unwrap();
            }
            ApiRequest::GetHistory { .. } => {
                write_client_frame(
                    &mut writer,
                    &ServerFrame::reply(
                        reply_id,
                        ApiEvent::History {
                            session_id: "s-1".into(),
                            messages: vec![HistoryMessage {
                                role: "assistant".into(),
                                content: "The final answer is 42.".into(),
                            }],
                        },
                    ),
                )
                .await
                .unwrap();
            }
            _ => {
                write_client_frame(
                    &mut writer,
                    &ServerFrame::reply(
                        reply_id,
                        ApiEvent::Error {
                            code: memnoth_harness_api::ErrorCode::UnknownRequest,
                            message: "fake server does not implement this".into(),
                        },
                    ),
                )
                .await
                .unwrap();
            }
        }
    }
}

fn bridge_at(path: std::path::PathBuf) -> AgentBridge {
    AgentBridge::new(path)
}

#[tokio::test]
async fn happy_path_session_prompt_wait_events_set_model() {
    let socket = spawn_fake_api().await;
    let bridge = bridge_at(socket);

    let created = bridge
        .call(
            "agent_session_new",
            &json!({"cwd": "/tmp/proj", "cols": 100, "rows": 30}),
        )
        .await
        .expect("session_new");
    assert_eq!(created["session_id"], "s-1");
    assert_eq!(created["grid"]["cols"], 100);

    let prompt = bridge
        .call(
            "agent_prompt",
            &json!({"session": "s-1", "text": "compute answer"}),
        )
        .await
        .expect("prompt");
    assert_eq!(prompt["submitted"], true);

    let wait = bridge
        .call(
            "agent_wait",
            &json!({"session": "s-1", "text": "final answer", "timeout_ms": 5000}),
        )
        .await
        .expect("wait text");
    assert_eq!(wait["found"], true);
    assert!(
        wait["transcript_tail"]
            .as_str()
            .unwrap()
            .contains("final answer"),
        "{wait}"
    );

    let stable = bridge
        .call(
            "agent_wait",
            &json!({"session": "s-1", "stable_ms": 120, "timeout_ms": 5000}),
        )
        .await
        .expect("wait stable");
    assert_eq!(stable["stable"], true);

    let events = bridge
        .call("agent_events", &json!({"session": "s-1", "tail_n": 10}))
        .await
        .expect("events");
    assert_eq!(events["status"], "idle");
    let kinds: Vec<&str> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"text_delta"), "{kinds:?}");
    assert!(kinds.contains(&"turn_done"), "{kinds:?}");

    let model = bridge
        .call(
            "agent_set_model",
            &json!({"session": "s-1", "provider": "kilo", "model": "gpt-x"}),
        )
        .await
        .expect("set model");
    assert_eq!(model["model_set"], "kilo/gpt-x");
}

#[tokio::test]
async fn wait_timeout_returns_structured_not_error() {
    let socket = spawn_fake_api().await;
    let bridge = bridge_at(socket);
    bridge
        .call("agent_session_new", &json!({"cwd": "/tmp/x"}))
        .await
        .expect("session_new");

    let outcome = bridge
        .call(
            "agent_wait",
            &json!({"session": "s-1", "text": "will never appear", "timeout_ms": 150}),
        )
        .await
        .expect("wait must not error");
    assert_eq!(outcome["found"], false);
    assert_eq!(outcome["timed_out"], true);
}

#[tokio::test]
async fn unknown_session_is_tool_error() {
    let socket = spawn_fake_api().await;
    let bridge = bridge_at(socket);
    let err = bridge
        .call("agent_prompt", &json!({"session": "ghost", "text": "hi"}))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("unknown agent session: ghost"));
}

#[test]
fn agent_tool_definitions_expose_exactly_six() {
    let defs = super::super::visible_tools(Profile::Agent);
    let names: Vec<&str> = defs.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 6, "{names:?}");
    for expected in AGENT_TOOLS {
        assert!(names.contains(expected), "missing {expected} in {names:?}");
    }
}

#[tokio::test]
async fn session_close_emits_lifecycle_event_and_blocks_further_calls() {
    let socket = spawn_fake_api().await;
    let bridge = bridge_at(socket);
    bridge
        .call("agent_session_new", &json!({"cwd": "/tmp/x"}))
        .await
        .expect("session_new");

    let closed = bridge
        .call("agent_session_close", &json!({"session": "s-1"}))
        .await
        .expect("close");
    assert_eq!(closed["closed"], true);

    // Further actions fail with the reason, not a silent hang.
    let err = bridge
        .call("agent_prompt", &json!({"session": "s-1", "text": "hi"}))
        .await
        .expect_err("must fail after close");
    assert!(
        err.to_string().contains("closed via agent_session_close"),
        "{err}"
    );

    // The ring log survives: lifecycle entry is readable.
    let events = bridge
        .call("agent_events", &json!({"session": "s-1"}))
        .await
        .expect("events still readable");
    assert_eq!(events["status"], "closed");
    let kinds: Vec<&str> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"lifecycle"), "{kinds:?}");

    // Double-close is idempotent and reports why.
    let again = bridge
        .call("agent_session_close", &json!({"session": "s-1"}))
        .await
        .expect("second close");
    assert_eq!(again["already"], "closed via agent_session_close");
}

#[tokio::test]
async fn idle_sessions_are_reaped_after_ttl_with_structured_event() {
    let socket = spawn_fake_api().await;
    let bridge = AgentBridge::with_ttl(socket, Duration::from_millis(80));
    bridge
        .call("agent_session_new", &json!({"cwd": "/tmp/x"}))
        .await
        .expect("session_new");

    tokio::time::sleep(Duration::from_millis(150)).await;
    bridge.reap_idle();

    // Reaping is loud: the next action names the reaper, and the log keeps
    // the lifecycle entry.
    let err = bridge
        .call("agent_prompt", &json!({"session": "s-1", "text": "hi"}))
        .await
        .expect_err("reaped session must refuse work");
    assert!(err.to_string().contains("reaped after"), "{err}");
    assert!(
        err.to_string().contains("MEMNOTH_MCP_AGENT_TTL_MS"),
        "{err}"
    );

    let events = bridge
        .call("agent_events", &json!({"session": "s-1"}))
        .await
        .expect("log survives reaping");
    assert_eq!(events["status"], "reaped");
    let kinds: Vec<&str> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"lifecycle"), "{kinds:?}");

    // Active sessions are untouched by the sweep.
    bridge
        .call("agent_session_new", &json!({"cwd": "/tmp/y"}))
        .await
        .expect("fresh session must not be reaped");
}

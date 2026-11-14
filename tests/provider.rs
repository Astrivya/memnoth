//! The model transport, exercised against a socket that answers on purpose.
//!
//! A unit test can prove that a reply *parses*. It cannot prove that a turn
//! sends the request the provider expects, reads the tool call out of it, runs
//! the tool, and sends the result back in the shape that provider demands — and
//! that round trip is the whole feature. So this file runs a real blocking HTTP
//! client against a tiny server on loopback, one canned response per request,
//! and asserts both the bytes that went out and the answer that came back.
//!
//! Nothing here reaches a provider: no key is real, no request leaves the host.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{Value, json};

use memnoth::provider::{Answer, Exchange, Kind, Settings, Tools, run_turn};

/// A server that answers `responses` in order, on loopback, then stops.
struct Mock {
    base_url: String,
    seen: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Mock {
    fn start(responses: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);

        let handle = std::thread::spawn(move || {
            for body in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let request = read_request(&mut stream, &recorded);
                drop(request);
                let payload = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            seen,
            handle: Some(handle),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().expect("requests lock").clone()
    }

    fn finish(mut self) -> JoinHandle<()> {
        self.handle.take().expect("handle")
    }
}

/// Read one HTTP request: the head, then `Content-Length` bytes of body.
///
/// The body is what the assertions are about, so it is returned with the head
/// rather than discarded.
fn read_request(stream: &mut std::net::TcpStream, recorded: &Arc<Mutex<Vec<String>>>) -> String {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return String::new();
        }
        let done = line == "\r\n" || line == "\n";
        head.push_str(&line);
        if done {
            break;
        }
    }
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    let request = format!("{head}{}", String::from_utf8_lossy(&body));
    recorded
        .lock()
        .expect("requests lock")
        .push(request.clone());
    request
}

struct FakeTools {
    answer: String,
    calls: Vec<(String, Value)>,
}

impl Tools for FakeTools {
    fn call(&mut self, name: &str, input: &Value) -> anyhow::Result<String> {
        self.calls.push((name.to_string(), input.clone()));
        Ok(self.answer.clone())
    }
}

fn settings(kind: Kind, base_url: &str) -> Settings {
    Settings {
        kind,
        model: "test-model".into(),
        base_url: base_url.to_string(),
        max_tokens: 128,
        api_key: Some("test-key".into()),
        pricing: None,
        provider_name: None,
        temperature: None,
        top_p: None,
        reasoning_effort: None,
        key_source: Some("test".into()),
    }
}

#[test]
fn an_openai_turn_calls_a_tool_and_answers_with_what_it_found() {
    let mock = Mock::start(vec![
        json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "kg_impact",
                            "arguments": "{\"symbol\":\"MemoryEngine\"}"
                        }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 21, "completion_tokens": 6 }
        }),
        json!({
            "choices": [{
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "7 definitions, 660 references." }
            }],
            "usage": { "prompt_tokens": 40, "completion_tokens": 9 }
        }),
    ]);
    let settings = settings(Kind::OpenAi, &mock.base_url);
    let mut tools = FakeTools {
        answer: "MemoryEngine: 7 definitions".into(),
        calls: Vec::new(),
    };
    let mut notes = Vec::new();

    let answer: Answer = run_turn(
        &settings,
        Path::new("/ws"),
        &[Exchange {
            role: memnoth_session::Role::User,
            text: "earlier question".into(),
        }],
        "what breaks if I change MemoryEngine?",
        &mut tools,
        &mut |note| notes.push(note.to_string()),
    )
    .expect("the turn must complete");

    assert_eq!(answer.text, "7 definitions, 660 references.");
    assert_eq!(answer.rounds, 1);
    assert_eq!((answer.input_tokens, answer.output_tokens), (61, 15));
    assert_eq!(
        tools.calls.len(),
        1,
        "the model's tool call must actually run"
    );
    assert_eq!(tools.calls[0].0, "kg_impact");
    assert_eq!(tools.calls[0].1["symbol"], "MemoryEngine");
    assert!(
        notes.iter().any(|note| note.contains("kg impact")),
        "the transcript must be told what was looked up: {notes:?}"
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].starts_with("POST /chat/completions "),
        "{}",
        requests[0]
    );
    assert!(
        requests[0].contains("authorization: Bearer test-key")
            || requests[0].contains("Authorization: Bearer test-key")
    );
    assert!(requests[0].contains("\"model\":\"test-model\""));
    assert!(requests[0].contains("\"tools\""));
    assert!(requests[0].contains("earlier question"), "history is sent");
    assert!(requests[1].contains("\"role\":\"tool\""));
    assert!(requests[1].contains("MemoryEngine: 7 definitions"));
    assert!(requests[1].contains("call-1"));
    mock.finish().join().expect("mock server");
}

#[test]
fn an_anthropic_turn_uses_the_messages_api_and_echoes_tool_blocks() {
    let mock = Mock::start(vec![
        json!({
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 30, "output_tokens": 8 },
            "content": [
                { "type": "text", "text": "Checking the vault." },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "vault_recall",
                    "input": { "query": "httpOnly cookies" }
                }
            ]
        }),
        json!({
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 55, "output_tokens": 12 },
            "content": [{ "type": "text", "text": "The decision was httpOnly cookies." }]
        }),
    ]);
    let settings = settings(Kind::Anthropic, &mock.base_url);
    let mut tools = FakeTools {
        answer: "auth · decision · use httpOnly cookies".into(),
        calls: Vec::new(),
    };

    let answer = run_turn(
        &settings,
        Path::new("/ws"),
        &[],
        "why httpOnly?",
        &mut tools,
        &mut |_| {},
    )
    .expect("the turn must complete");

    assert_eq!(answer.text, "The decision was httpOnly cookies.");
    assert_eq!(answer.input_tokens, 85);
    assert_eq!(tools.calls[0].0, "vault_recall");

    let requests = mock.requests();
    assert!(
        requests[0].starts_with("POST /v1/messages "),
        "{}",
        requests[0]
    );
    assert!(
        requests[0].contains("x-api-key: test-key"),
        "the key goes in x-api-key, not Authorization: {}",
        requests[0]
    );
    assert!(requests[0].contains("anthropic-version"));
    // The second request has to carry the assistant's own content block back
    // unchanged, or Anthropic rejects the tool result it belongs to.
    assert!(requests[1].contains("\"type\":\"tool_use\""));
    assert!(requests[1].contains("\"type\":\"tool_result\""));
    assert!(requests[1].contains("toolu_1"));
    mock.finish().join().expect("mock server");
}

#[test]
fn a_provider_error_message_reaches_the_user_instead_of_a_json_wall() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local address").port();
    let server = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        // Drain the whole request before answering. A server that closes while
        // the client is still writing the body produces a connection reset, and
        // then the test proves nothing about how an error body is read.
        let scratch = Arc::new(Mutex::new(Vec::new()));
        read_request(&mut stream, &scratch);
        let body = r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
    });

    let settings = settings(Kind::Anthropic, &format!("http://127.0.0.1:{port}"));
    let mut tools = FakeTools {
        answer: String::new(),
        calls: Vec::new(),
    };
    let error = run_turn(
        &settings,
        Path::new("/ws"),
        &[],
        "q",
        &mut tools,
        &mut |_| {},
    )
    .expect_err("a 401 must fail the turn");
    let message = format!("{error:#}");
    assert!(message.contains("401"), "{message}");
    assert!(message.contains("invalid x-api-key"), "{message}");
    server.join().expect("mock server");
}

#[test]
fn a_turn_that_runs_out_of_rounds_keeps_what_the_model_said() {
    // Every round both speaks and asks for a tool. The cap still holds, but the
    // text the model already wrote is the turn's answer: ending in an error
    // threw away work the person had paid for and read like a crash.
    let reply = json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": "half an answer",
                "tool_calls": [{
                    "id": "same",
                    "function": { "name": "kg_status", "arguments": "{}" }
                }]
            }
        }]
    });
    let mock = Mock::start(vec![reply; memnoth::provider::MAX_ROUNDS]);
    let settings = settings(Kind::OpenAi, &mock.base_url);
    let mut tools = FakeTools {
        answer: "indexed".into(),
        calls: Vec::new(),
    };

    let answer = run_turn(
        &settings,
        Path::new("/ws"),
        &[],
        "loop forever",
        &mut tools,
        &mut |_| {},
    )
    .expect("a model that spoke still produced an answer");
    assert!(answer.text.contains("half an answer"), "{}", answer.text);
    assert!(answer.text.contains("tool rounds"), "{}", answer.text);
    assert_eq!(tools.calls.len(), memnoth::provider::MAX_ROUNDS);
    mock.finish().join().expect("mock server");
}

#[test]
fn a_turn_that_never_stops_calling_tools_is_stopped_and_says_so() {
    // Every request gets the same tool call, so the loop can only end at the
    // round cap. The test asserts the cap exists rather than a timeout.
    let reply = json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": "same",
                    "function": { "name": "kg_status", "arguments": "{}" }
                }]
            }
        }]
    });
    let mock = Mock::start(vec![reply; memnoth::provider::MAX_ROUNDS]);
    let settings = settings(Kind::OpenAi, &mock.base_url);
    let mut tools = FakeTools {
        answer: "indexed".into(),
        calls: Vec::new(),
    };

    let error = run_turn(
        &settings,
        Path::new("/ws"),
        &[],
        "loop forever",
        &mut tools,
        &mut |_| {},
    )
    .expect_err("the loop must be bounded");
    assert!(format!("{error:#}").contains("without answering"));
    assert_eq!(tools.calls.len(), memnoth::provider::MAX_ROUNDS);
    mock.finish().join().expect("mock server");
}

//! `mn ask` is the pipeable form of a turn.
//!
//! These tests run the real binary against a socket on loopback that answers
//! one OpenAI-shaped reply, so the path under test includes argument parsing,
//! prompt resolution, provider config, the HTTP request and the output shape.
//! No key is real and nothing leaves the host.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

/// Text the fake provider answers with.
const ANSWER: &str = "the fake answer";

struct Provider {
    base_url: String,
}

fn start_provider() -> Provider {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local address").port();
    let payload = json!({
        "choices": [{
            "message": { "role": "assistant", "content": ANSWER },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 3, "completion_tokens": 5 }
    })
    .to_string();

    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        read_request(&mut stream);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });

    Provider {
        base_url: format!("http://127.0.0.1:{port}"),
    }
}

/// Read the head and the body so the client can finish its write, even though
/// these assertions are about the reply, not the request.
fn read_request(stream: &mut std::net::TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
}

fn command(provider: &Provider, home: &Path) -> Command {
    let mut command = Command::new(binary());
    command
        .env("MEMNOTH_HOME", home)
        .env("MEMNOTH_PROVIDER", "openai")
        .env("MEMNOTH_API_KEY", "test-key")
        .env("MEMNOTH_MODEL", "fake-model")
        .env("MEMNOTH_PROVIDER_BASE_URL", &provider.base_url)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH");
    // The trust gate refuses a headless folder. These tests are about `ask`,
    // not about the gate, so they opt in for the run and write nothing.
    command.arg("--trust-folder");
    command
}

fn run(provider: &Provider, args: &[&str], input: Option<&str>) -> Output {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut command = command(provider, &dir.path().join("home"));
    command.args(args);

    match input {
        Some(input) => {
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().expect("spawn memnoth");
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(input.as_bytes())
                .expect("write the prompt");
            child.wait_with_output().expect("wait for memnoth")
        }
        None => command
            .stdin(Stdio::null())
            .output()
            .expect("run memnoth without stdin"),
    }
}

#[test]
fn a_piped_prompt_answers_in_one_json_envelope() {
    let provider = start_provider();
    let out = run(
        &provider,
        &["ask", "--output-format", "json"],
        Some("hello\n"),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let envelope: Value = serde_json::from_slice(&out.stdout).expect("ask --output-format json");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "ask");
    assert_eq!(envelope["data"]["text"], ANSWER);
    assert_eq!(envelope["data"]["model"], "fake-model");
    assert_eq!(envelope["data"]["usage"]["input_tokens"], 3);
    assert_eq!(envelope["data"]["usage"]["output_tokens"], 5);
    assert!(
        envelope["data"]["provider"].as_str().is_some(),
        "the provider name is part of the answer"
    );
}

#[test]
fn a_positional_prompt_prints_only_the_answer() {
    let provider = start_provider();
    let out = run(&provider, &["ask", "what", "changed?"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        ANSWER,
        "text output must not wrap the answer in an envelope"
    );
}

#[test]
fn jsonl_ends_with_one_answer_event() {
    let provider = start_provider();
    let out = run(
        &provider,
        &["ask", "-p", "hello", "--output-format", "jsonl"],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout.lines().last().expect("at least one JSONL line");
    let event: Value = serde_json::from_str(last).expect("every line is JSON");
    assert_eq!(event["type"], "answer");
    assert_eq!(event["text"], ANSWER);
    assert_eq!(event["ok"], true);
}

#[test]
fn no_prompt_and_no_tty_is_a_usage_error_not_a_wait() {
    let provider = start_provider();
    let out = run(&provider, &["ask"], None);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a missing prompt is a usage error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no prompt"), "{stderr}");
    assert!(
        stderr.contains("printf"),
        "the fix should show the pipe form: {stderr}"
    );
}

/// A provider that tries to send the POST to the metadata address. The client
/// must stop at the 302 and report it rather than follow the Location.
fn start_redirect_provider() -> Provider {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local address").port();

    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        read_request(&mut stream);
        let response = "HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/latest/meta-data\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });

    Provider {
        base_url: format!("http://127.0.0.1:{port}"),
    }
}

#[test]
fn a_redirect_to_a_private_address_is_not_followed() {
    let provider = start_redirect_provider();
    let out = run(&provider, &["ask", "hello"], None);
    assert!(!out.status.success(), "a stopped redirect is a failure");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("302"),
        "the provider's 302 must be reported, proving it was not followed: {stderr}"
    );
}

//! Every documented exit code, produced through real dispatch.
//!
//! The enum round-trip in `output.rs` proves the numbers agree. This file
//! proves each number is reachable from a command and that `--json` carries it
//! in the envelope — a table that only exists in an enum is documentation, not
//! a contract.
//!
//! Codes 0–8 are covered here or in `exit_codes.rs` (8, the unbuilt feature).
//! 130 is a signal, not a request outcome; its mapping is pinned by the enum
//! test and cannot be produced without interrupting a child process.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

struct Provider {
    base_url: String,
}

/// A loopback provider that answers `status` once. A 200 carries one text
/// answer; any other status carries an error sentence the client can read.
fn start_provider(status: u16) -> Provider {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local address").port();
    let payload = if status == 200 {
        json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        })
    } else {
        json!({ "error": { "message": format!("status {status}") } })
    }
    .to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    };

    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        read_request(&mut stream);
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
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

fn command(home: &Path) -> Command {
    let mut command = Command::new(binary());
    command
        .env("MEMNOTH_HOME", home)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().expect("run memnoth")
}

fn provider_run(provider: &Provider, home: &Path, args: &[&str]) -> Output {
    command(home)
        .env("MEMNOTH_PROVIDER", "openai")
        .env("MEMNOTH_API_KEY", "test-key")
        .env("MEMNOTH_MODEL", "fake-model")
        .env("MEMNOTH_PROVIDER_BASE_URL", &provider.base_url)
        .args(args)
        .output()
        .expect("run memnoth")
}

fn temp_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let home = dir.path().join("home");
    (dir, home)
}

/// Parse the failure envelope on stderr and assert the process code agrees.
fn assert_error(out: &Output, code: u8) {
    assert_eq!(
        out.status.code(),
        Some(i32::from(code)),
        "expected exit {code}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|error| {
        panic!(
            "stderr must be one JSON envelope: {error}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["code"], code, "envelope code must agree");
    assert!(
        envelope["error"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "the envelope must carry a sentence"
    );
}

#[test]
fn code_0_is_one_success_envelope() {
    let (_dir, home) = temp_home();
    let provider = start_provider(200);
    let out = provider_run(&provider, &home, &["--json", "ask", "hello"]);
    assert!(out.status.success());
    let envelope: Value = serde_json::from_slice(&out.stdout).expect("stdout envelope");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "ask");
}

#[test]
fn code_1_is_an_unclassified_provider_rejection() {
    let (_dir, home) = temp_home();
    let provider = start_provider(400);
    let out = provider_run(&provider, &home, &["--json", "ask", "hello"]);
    assert_error(&out, 1);
}

#[test]
fn code_2_is_a_missing_prompt_without_a_tty() {
    let (_dir, home) = temp_home();
    let out = run(&home, &["--json", "ask"]);
    assert_error(&out, 2);
}

#[test]
fn code_3_is_a_rejected_credential() {
    let (_dir, home) = temp_home();
    let provider = start_provider(401);
    let out = provider_run(&provider, &home, &["--json", "ask", "hello"]);
    assert_error(&out, 3);
}

#[test]
fn code_4_is_an_unknown_verb_named_by_help() {
    let (_dir, home) = temp_home();
    let out = run(&home, &["--json", "help", "nonsense"]);
    assert_error(&out, 4);
}

#[test]
fn code_5_is_a_provider_5xx() {
    let (_dir, home) = temp_home();
    let provider = start_provider(503);
    let out = provider_run(&provider, &home, &["--json", "ask", "hello"]);
    assert_error(&out, 5);
}

#[test]
fn code_6_is_a_restore_over_an_existing_vault() {
    let (_dir, home) = temp_home();
    let backup = home.join("backup.db");
    let backup_text = backup.to_string_lossy().into_owned();

    let init = run(&home, &["vault", "init"]);
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let backed_up = run(&home, &["vault", "backup", "--output", &backup_text]);
    assert!(
        backed_up.status.success(),
        "backup failed: {}",
        String::from_utf8_lossy(&backed_up.stderr)
    );

    let out = run(
        &home,
        &["--json", "vault", "restore", "--input", &backup_text],
    );
    assert_error(&out, 6);
    let envelope: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(
        envelope["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("--force")),
        "the fix must name the state change that resolves it: {envelope}"
    );
}

#[test]
fn code_7_is_a_provider_rate_limit() {
    let (_dir, home) = temp_home();
    let provider = start_provider(429);
    let out = provider_run(&provider, &home, &["--json", "ask", "hello"]);
    assert_error(&out, 7);
}

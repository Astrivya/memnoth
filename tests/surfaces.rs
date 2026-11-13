//! The long-running surfaces, started for real and asked whether they answer.
//!
//! A placeholder shipped as truth once: `watch --port` printed a banner naming a
//! port that nothing ever bound, and `serve`'s sibling bugs were only found by
//! hand because no test ever started the thing it was describing. Every case
//! here is one line of the same assertion — *start it, connect to it, get an
//! answer* — because that is the whole class: a surface that announces itself
//! and then does nothing.
//!
//! Nothing here touches the developer's real vault: each test owns a tempdir and
//! points `MEMNOTH_HOME` at it, the way `tests/cli.rs` does. Every child is
//! killed at the end of the test that spawned it, and every wait is a bounded
//! poll with a deadline, so a surface that stops answering fails instead of
//! hanging CI.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The binary under test, as cargo built it.
fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

/// How long a surface gets to start answering. Generous: `watch` indexes the
/// fixture before it serves, and a loaded CI runner is slow, not wrong.
const START_BUDGET: Duration = Duration::from_secs(25);

/// An isolated home and workspace for one test.
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("ws/src")).unwrap();
        std::fs::write(
            dir.path().join("ws/src/lib.rs"),
            "pub fn callee(value: u32) -> u32 {\n    value + 1\n}\n",
        )
        .unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("ws")
    }

    /// Start `memnoth` in the sandbox. Output goes nowhere: these surfaces are
    /// servers, and a piped stdout nobody drains is its own kind of hang.
    fn spawn(&self, args: &[&str]) -> ChildGuard {
        let child = Command::new(binary())
            .arg("--trust-folder")
            .args(args)
            .current_dir(self.root())
            .env("MEMNOTH_HOME", self.home())
            .env_remove("MEMNOTH_VAULT")
            .env_remove("MEMNOTH_GRAPH")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("cannot spawn {args:?}: {error}"));
        ChildGuard(Some(child))
    }
}

/// A child that is killed when the test ends, pass or fail.
struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A port nothing is listening on, taken by binding and immediately releasing it.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a probe port");
    let port = listener.local_addr().expect("probe address").port();
    drop(listener);
    port
}

/// One `GET`, tolerating both a closing connection and a keep-alive one.
///
/// A server that answers and then holds the socket open is not a failure — the
/// answer is what this harness is asking about — so a read that times out keeps
/// the bytes it already has instead of discarding them.
fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(400)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(800)))
        .ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => raw.extend_from_slice(&chunk[..read]),
            // A deadline or a reset after the headers is still an answer.
            Err(_) => break,
        }
    }
    if raw.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())?;
    Some((status, text))
}

/// Poll one path until it answers with a 2xx, or the budget runs out.
///
/// The returned text is the last answer seen, which is what a failing assertion
/// wants to quote.
fn wait_for(port: u16, path: &str, budget: Duration) -> Option<(u16, String)> {
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < budget {
        if let Some((status, body)) = http_get(port, path) {
            last = Some((status, body));
            if (200..300).contains(&status) {
                return last;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

/// `serve` binds its port and answers `/health`.
///
/// This is the surface every editor integration talks to, so "it printed a
/// banner" has never been enough: the assertion is the HTTP answer.
#[test]
fn serve_answers_health_on_the_port_it_names() {
    let sandbox = Sandbox::new();
    let port = free_port();
    let port_text = port.to_string();
    let _child = sandbox.spawn(&["serve", "--host", "127.0.0.1", "--port", &port_text]);

    let answer = wait_for(port, "/health", START_BUDGET).unwrap_or_else(|| {
        panic!("`serve --port {port}` never answered GET /health within {START_BUDGET:?}")
    });
    assert_eq!(answer.0, 200, "`serve` answered {}", answer.0);
    assert!(
        answer.1.contains("\"status\":\"ok\""),
        "health did not report ok: {}",
        answer.1
    );
}

/// `serve --port 0` names the port the OS gave it, not the `0` it was asked for.
///
/// The banner used to print the *requested* port, so `--port 0` — the only way
/// to serve on a port nobody reserved — advertised `http://host:0`, an address
/// no client can dial. The assertion is therefore about the number in the
/// banner: read it from stderr, then connect to exactly that port.
#[test]
fn serve_with_port_zero_announces_the_bound_port() {
    let sandbox = Sandbox::new();
    let mut child = Command::new(binary())
        .arg("--trust-folder")
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .current_dir(sandbox.root())
        .env("MEMNOTH_HOME", sandbox.home())
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn memnoth serve");
    let stderr = child.stderr.take().expect("piped stderr");
    let _child = ChildGuard(Some(child));

    // The banner is written as soon as the listener is bound, so a thread that
    // drains stderr until the first port line answers within the same budget a
    // connection would get.
    // The reader drains to EOF after it finds the port instead of breaking.
    // Dropping the pipe closes the child's stderr, and on Windows the next
    // `eprintln!` in the banner then fails with a broken pipe, which panics the
    // server before it ever accepts — the announced port is real but nothing is
    // left listening. Reading to the end keeps the pipe open for the lifetime of
    // the process under test.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut announced = false;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if announced {
                continue;
            }
            if let Some((_, address)) = line.split_once("http://")
                && let Some(port) = address
                    .rsplit(':')
                    .next()
                    .and_then(|port| port.trim().parse::<u16>().ok())
            {
                announced = true;
                let _ = tx.send(port);
            }
        }
    });
    let port = rx.recv_timeout(START_BUDGET).unwrap_or_else(|_| {
        panic!("`serve --port 0` announced no port on stderr within {START_BUDGET:?}")
    });
    assert_ne!(
        port, 0,
        "`serve --port 0` announced the requested port instead of the bound one"
    );

    let answer = wait_for(port, "/health", START_BUDGET).unwrap_or_else(|| {
        panic!("`serve --port 0` announced {port} but nothing answered GET /health there")
    });
    assert_eq!(
        answer.0, 200,
        "`serve` announced {port} and answered {} there",
        answer.0
    );
}

/// `watch --port` really listens on the port its banner names.
///
/// `kg serve` once printed a port that nothing bound, and `watch` carried the
/// same placeholder: a thread whose entire body was the `eprintln!` announcing
/// itself. Connecting is the only way to tell a server from a banner.
#[test]
fn watch_with_a_port_really_listens() {
    let sandbox = Sandbox::new();
    let port = free_port();
    let port_text = port.to_string();
    let _child = sandbox.spawn(&["watch", "--port", &port_text]);

    let answer = wait_for(port, "/health", START_BUDGET).unwrap_or_else(|| {
        panic!(
            "`watch --port {port}` announced a port and never answered GET /health \
             within {START_BUDGET:?} — a banner is not a server"
        )
    });
    assert_eq!(answer.0, 200, "`watch` answered {}: {}", answer.0, answer.1);
}

/// Every tool profile is exposed, and none of them is empty.
///
/// `mcp exposes` is the offline surface that answers "what does a host get when
/// it registers the `code` profile?" — no server, no model, no network — so it
/// is the one MCP question this harness can ask that still pins all five
/// profiles to a real tool list.
#[test]
fn mcp_exposes_lists_every_profile_with_tools() {
    let sandbox = Sandbox::new();
    let output = Command::new(binary())
        .arg("--trust-folder")
        .args(["mcp", "exposes", "--json"])
        .current_dir(sandbox.root())
        .env("MEMNOTH_HOME", sandbox.home())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn memnoth mcp exposes");
    assert!(
        output.status.success(),
        "`mcp exposes --json` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("`mcp exposes --json` must print JSON");
    let profiles = value["data"]["profiles"]
        .as_array()
        .expect("profiles is a list");

    let mut names: Vec<&str> = profiles
        .iter()
        .map(|profile| profile["profile"].as_str().unwrap_or(""))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["code", "coord", "full", "memory", "micro"],
        "a profile went missing from `mcp exposes`"
    );

    for profile in profiles {
        let name = profile["profile"].as_str().unwrap_or("?");
        let tools = profile["tools"].as_array().expect("tools is a list");
        assert!(
            !tools.is_empty(),
            "profile {name:?} advertises no tools; a host registering it would get nothing"
        );
        assert_eq!(
            profile["tool_count"].as_u64().unwrap_or(0) as usize,
            tools.len(),
            "profile {name:?} counts {} tools but lists {}",
            profile["tool_count"],
            tools.len()
        );
    }
}

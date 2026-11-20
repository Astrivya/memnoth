//! End-to-end auth regression for `mn serve`.
//!
//! The bearer check, the loopback Host / DNS-rebinding check and the private
//! token file are also covered by unit tests in `src/serve.rs`, but a unit test
//! cannot catch a wire-level regression: a guard that is never mounted, a route
//! that is accidentally left open, a banner that names a port nothing binds.
//! This suite starts the real `mn` binary on `serve --port 0`, reads the port it
//! announces on stderr, and speaks raw HTTP/1.1 to it with `TcpStream`.
//!
//! Two refusal codes are deliberate and different:
//!   * 401 Unauthorized — the bearer token is missing, wrong, or a prefix of
//!     the real one;
//!   * 403 Forbidden — the `Host` names something other than this machine.
//!
//! Both are refusals. The Host guard runs first, so a rebound name is rejected
//! even when the caller presents the correct token.
//!
//! Nothing here touches the developer's vault: every server gets an isolated
//! `MEMNOTH_HOME` tempdir, and every child is killed and reaped when its guard
//! drops, pass or fail.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The binary under test, as cargo built it. `mn` `include!`s the same
/// `main.rs` as `memnoth`, so this is the real server, not a second path.
const BINARY: &str = env!("CARGO_BIN_EXE_mn");

/// A generous startup budget: a loaded CI runner is slow, not wrong.
const START_BUDGET: Duration = Duration::from_secs(25);

/// A distinctive token so a wrong/prefix token cannot accidentally match.
const TOKEN: &str = "serve-auth-regression-9f3c21ab7d";

const LOOPBACK_HOST: &str = "127.0.0.1";

/// A running `mn serve` plus everything needed to clean it up.
struct ServeGuard {
    // Declared first so the child is killed and reaped before the tempdir that
    // holds its state directory is removed.
    child: Option<Child>,
    port: u16,
    stderr: Arc<Mutex<String>>,
    _dir: tempfile::TempDir,
}

impl Drop for ServeGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl ServeGuard {
    /// Start a loopback server on an OS-assigned port.
    ///
    /// `token_arg` selects the CLI token source (empty means "use the env or
    /// the file"); `env_token` supplies `MEMNOTH_SERVE_TOKEN` when present.
    fn start(token_arg: &[&str], env_token: Option<&str>) -> Self {
        let dir = tempfile::tempdir().expect("a temp home");
        let mut command = Command::new(BINARY);
        command
            .arg("--trust-folder")
            .args(["serve", "--host", LOOPBACK_HOST, "--port", "0"])
            .args(token_arg)
            .current_dir(dir.path())
            .env("MEMNOTH_HOME", dir.path())
            .env_remove("MEMNOTH_VAULT")
            .env_remove("MEMNOTH_GRAPH")
            // Never inherit a token from the test runner: each case controls
            // its own source explicitly, including the "no token" cases.
            .env_remove("MEMNOTH_SERVE_TOKEN")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(value) = env_token {
            command.env("MEMNOTH_SERVE_TOKEN", value);
        }

        let mut child = command
            .spawn()
            .unwrap_or_else(|error| panic!("cannot spawn {BINARY} serve: {error}"));
        let stderr_pipe = child.stderr.take().expect("piped stderr");

        // Drain stderr on a thread. The port line is sent over a channel; the
        // thread keeps reading to EOF so the pipe stays open for the life of
        // the process (dropping it early makes the server's next eprintln!
        // fail on a broken pipe before it ever accepts, notably on Windows).
        let captured = Arc::new(Mutex::new(String::new()));
        let thread_captured = Arc::clone(&captured);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut announced = false;
            for line in BufReader::new(stderr_pipe).lines().map_while(Result::ok) {
                {
                    let mut buffer = thread_captured
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    buffer.push_str(&line);
                    buffer.push('\n');
                }
                if announced {
                    continue;
                }
                // The banner is "Server listening on http://127.0.0.1:PORT".
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
            panic!(
                "`mn serve --port 0` announced no port within {START_BUDGET:?}; stderr:\n{}",
                Self::read_stderr(&captured)
            )
        });
        assert_ne!(
            port, 0,
            "the banner named the requested port 0 instead of the bound port"
        );

        // The banner is printed just after bind, but give the accept loop a
        // bounded chance to come up before the assertions start firing.
        let deadline = Instant::now() + START_BUDGET;
        loop {
            if let Some(health) = http_get(port, "/health", Some(LOOPBACK_HOST), None)
                && health.status == 200
            {
                break;
            }
            if Instant::now() >= deadline {
                panic!(
                    "nothing answered GET /health on the announced port {port}; stderr:\n{}",
                    Self::read_stderr(&captured)
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Self {
            child: Some(child),
            port,
            stderr: captured,
            _dir: dir,
        }
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn home(&self) -> &Path {
        self._dir.path()
    }

    fn stderr(&self) -> String {
        Self::read_stderr(&self.stderr)
    }

    fn read_stderr(captured: &Arc<Mutex<String>>) -> String {
        captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// A parsed HTTP response. `raw` is kept for diagnostics and body assertions.
struct Response {
    status: u16,
    raw: String,
}

impl Response {
    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn body(&self) -> &str {
        self.raw
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or(&self.raw)
    }
}

/// One raw `GET`. `host = None` really omits the Host header; `token` is placed
/// verbatim after "Bearer ", which lets a prefix-length token be tested.
fn http_get(port: u16, path: &str, host: Option<&str>, token: Option<&str>) -> Option<Response> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(1000)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(1000)))
        .ok()?;

    let mut request = format!("GET {path} HTTP/1.1\r\n");
    if let Some(host) = host {
        request.push_str(&format!("Host: {host}\r\n"));
    }
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => raw.extend_from_slice(&chunk[..count]),
            // A deadline or a reset after the headers is still an answer.
            Err(_) => break,
        }
    }
    if raw.is_empty() {
        return None;
    }
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())?;
    Some(Response { status, raw })
}

#[test]
fn health_is_open_and_every_protected_route_rejects_a_missing_token() {
    let server = ServeGuard::start(&["--token", TOKEN], None);

    let health = http_get(server.port(), "/health", Some(LOOPBACK_HOST), None)
        .unwrap_or_else(|| panic!("GET /health got no response; stderr:\n{}", server.stderr()));
    assert_eq!(
        health.status,
        200,
        "/health must be open; got {}; stderr:\n{}",
        health.status,
        server.stderr()
    );
    assert!(
        health.body().contains("\"status\":\"ok\""),
        "/health did not report ok: {:?}",
        health.body()
    );

    // A representative of each protected family. The auth middleware runs
    // before the handlers, so none of these needs a valid query to be refused.
    for path in [
        "/kg/status",
        "/kg/search?q=x",
        "/vault/asof",
        "/vault/recall?q=x",
        "/session/current",
        "/session/list",
    ] {
        let response = http_get(server.port(), path, Some(LOOPBACK_HOST), None)
            .unwrap_or_else(|| panic!("GET {path} got no response; stderr:\n{}", server.stderr()));
        assert_eq!(
            response.status,
            401,
            "{path} answered {} without a token; stderr:\n{}",
            response.status,
            server.stderr()
        );
        assert!(
            response.body().contains("unauthorized"),
            "{path} refused without naming the token requirement: {:?}",
            response.body()
        );
    }
}

#[test]
fn a_correct_token_reaches_the_handler_but_wrong_and_prefix_tokens_do_not() {
    let server = ServeGuard::start(&["--token", TOKEN], None);

    let valid = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(TOKEN),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert!(
        valid.is_success(),
        "a correct token must reach the handler; got {}; stderr:\n{}",
        valid.status,
        server.stderr()
    );

    let wrong = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some("definitely-not-the-serve-token"),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert_eq!(
        wrong.status, 401,
        "a wrong token must be refused; got {}",
        wrong.status
    );

    let prefix = &TOKEN[..TOKEN.len() - 1];
    let truncated = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(prefix),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert_eq!(
        truncated.status, 401,
        "a prefix of the token must not authenticate; got {}",
        truncated.status
    );

    let extended = format!("{TOKEN}x");
    let longer = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(&extended),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert_eq!(
        longer.status, 401,
        "a token with an extra byte must not authenticate; got {}",
        longer.status
    );
}

#[test]
fn a_rebound_or_missing_host_is_refused_even_with_a_valid_token() {
    let server = ServeGuard::start(&["--token", TOKEN], None);

    // DNS rebinding: the browser reaches 127.0.0.1, but the Host is the
    // attacker's name. The Host guard runs first and rejects with 403, before
    // the (valid) token is ever considered.
    let rebound = http_get(
        server.port(),
        "/kg/status",
        Some("evil.example"),
        Some(TOKEN),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert_eq!(
        rebound.status,
        403,
        "a rebound Host must be refused even with a valid token; got {}; stderr:\n{}",
        rebound.status,
        server.stderr()
    );
    assert!(
        rebound.body().contains("host not allowed"),
        "the refusal must name the Host guard: {:?}",
        rebound.body()
    );

    // HTTP/1.1 without a Host is refused too — hyper answers 400, or the guard
    // answers 403; either way the protected handler is not reached.
    let no_host = http_get(server.port(), "/kg/status", None, Some(TOKEN)).unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert!(
        (400..500).contains(&no_host.status),
        "a request with no Host must be refused; got {}; stderr:\n{}",
        no_host.status,
        server.stderr()
    );

    // Sanity: the same token with a loopback Host is accepted, so the two
    // refusals above are caused by the Host, not by a dead server.
    let good = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(TOKEN),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert!(
        good.is_success(),
        "the loopback Host with a valid token must succeed; got {}",
        good.status
    );
}

#[test]
fn the_environment_token_is_accepted_in_place_of_the_flag() {
    let server = ServeGuard::start(&[], Some(TOKEN));

    let valid = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(TOKEN),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert!(
        valid.is_success(),
        "MEMNOTH_SERVE_TOKEN must authenticate; got {}; stderr:\n{}",
        valid.status,
        server.stderr()
    );

    let missing =
        http_get(server.port(), "/kg/status", Some(LOOPBACK_HOST), None).unwrap_or_else(|| {
            panic!(
                "GET /kg/status got no response; stderr:\n{}",
                server.stderr()
            )
        });
    assert_eq!(
        missing.status, 401,
        "a request with no token must be refused"
    );
}

#[test]
fn the_flag_token_takes_precedence_over_the_environment_token() {
    let env_token = "serve-auth-env-token-77aa9301";
    let server = ServeGuard::start(&["--token", TOKEN], Some(env_token));

    let by_flag = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(TOKEN),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert!(
        by_flag.is_success(),
        "the --token value must be the one in force; got {}",
        by_flag.status
    );

    let by_env = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(env_token),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert_eq!(
        by_env.status, 401,
        "the environment token must lose to --token"
    );
}

#[test]
fn the_generated_token_file_is_private_and_is_the_token_in_force() {
    // No --token and no MEMNOTH_SERVE_TOKEN: the server creates the file.
    let server = ServeGuard::start(&[], None);
    let path = server.home().join("serve.token");
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "no serve token file at {}; stderr:\n{error}\n{}",
            path.display(),
            server.stderr()
        )
    });
    let token = contents.trim();
    assert!(
        !token.is_empty(),
        "the generated token file must not be empty"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("serve token file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the serve token file must be 0600, found {mode:o}"
        );
    }
    #[cfg(not(unix))]
    {
        // Windows has no Unix mode bits; the file inherits the user profile's
        // ACL instead (see docs/cli/security.md §7).
        eprintln!(
            "SKIP: the 0600 token-file assertion is Unix-only; Windows uses the \
             user profile ACL"
        );
    }

    // The token in the file is the one the running server actually accepts,
    // and a request without it is still refused.
    let valid = http_get(
        server.port(),
        "/kg/status",
        Some(LOOPBACK_HOST),
        Some(token),
    )
    .unwrap_or_else(|| {
        panic!(
            "GET /kg/status got no response; stderr:\n{}",
            server.stderr()
        )
    });
    assert!(
        valid.is_success(),
        "the file token must authenticate; got {}; stderr:\n{}",
        valid.status,
        server.stderr()
    );

    let missing =
        http_get(server.port(), "/kg/status", Some(LOOPBACK_HOST), None).unwrap_or_else(|| {
            panic!(
                "GET /kg/status got no response; stderr:\n{}",
                server.stderr()
            )
        });
    assert_eq!(
        missing.status, 401,
        "a request with no token must be refused even when a token file exists"
    );
}

//! Real-terminal smoke: the actual binary behind an 80x24 PTY.
//!
//! `TestBackend` proves a frame draws; it cannot prove the product starts, asks
//! the terminal for a size, reads keys from a real console, and leaves cleanly.
//! W13-15, W13-24 and W13-25 asked for a terminal transcript and had no driver —
//! this is that driver. On Windows it is ConPTY, on unix a forkpty pair; the
//! assertions are on the parsed screen, so an escape sequence that wraps is a
//! failure here instead of a surprise on a user's 80-column terminal.
//!
//! On Windows the two screen tests are `ignore`d: ConPTY needs a console, and a
//! build agent starts processes without one (the child dies with 0xC0000142).
//! Run them where a real tty exists:
//!
//! ```text
//! docker run --rm -v E:/memnoth-prod:/w -v mn-target-linux:/target \
//!   -e CARGO_TARGET_DIR=/target -w /w/CLI rust:1-bookworm \
//!   cargo test --test tui_pty -- --nocapture
//! ```

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const ROWS: u16 = 24;
const COLS: u16 = 80;

struct Session {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    parser: vt100::Parser,
    raw: Vec<u8>,
    rx: Receiver<Vec<u8>>,
    // The pty's working directory and home live as long as the session: a
    // dropped temp dir while the child still writes is an error nobody asked
    // for in the middle of an assertion.
    _dir: tempfile::TempDir,
}

#[cfg(windows)]
fn ensure_console() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // A terminal user always has one; this host does not, and a ConPTY child
        // spawned without a console dies in DLL initialisation (0xC0000142)
        // before it can print a byte.
        windows_sys::Win32::System::Console::AllocConsole();
    });
}

#[cfg(not(windows))]
fn ensure_console() {}

impl Session {
    fn open(args: &[&str]) -> Self {
        ensure_console();
        let dir = tempfile::tempdir().expect("temp dir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty pair");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mn"));
        for arg in args {
            cmd.arg(arg);
        }
        cmd.cwd(dir.path());
        cmd.env("MEMNOTH_HOME", &home);
        cmd.env("MEMNOTH_VAULT", dir.path().join("vault.db"));
        cmd.env("MEMNOTH_GRAPH", dir.path().join("graph.db"));
        cmd.env("TERM", "xterm-256color");
        // The harness reads text, not colour: a theme is a separate question.
        cmd.env("NO_COLOR", "1");
        let child = pair.slave.spawn_command(cmd).expect("spawn mn");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = pair.master.take_writer().expect("pty writer");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            child,
            writer,
            parser: vt100::Parser::new(ROWS, COLS, 0),
            raw: Vec::new(),
            rx,
            _dir: dir,
        }
    }

    fn pump(&mut self) {
        while let Ok(chunk) = self.rx.try_recv() {
            self.raw.extend_from_slice(&chunk);
            self.parser.process(&chunk);
        }
    }

    fn text(&mut self) -> String {
        self.pump();
        self.parser.screen().contents()
    }

    /// What the child actually sent and how it ended. A blank screen with an
    /// empty capture is a spawn problem; a blank screen with bytes is a drawing
    /// problem, and a failure must say which.
    fn diagnose(&mut self) -> String {
        self.pump();
        let status = match self.child.try_wait() {
            Ok(Some(status)) => format!("{status:?}"),
            Ok(None) => "still running".to_string(),
            Err(err) => format!("try_wait: {err}"),
        };
        format!(
            "binary {}\nexit: {status}\nraw {} bytes: {:?}\nparsed screen:\n{}",
            env!("CARGO_BIN_EXE_mn"),
            self.raw.len(),
            String::from_utf8_lossy(&self.raw),
            self.parser.screen().contents()
        )
    }

    fn wait_for_any(&mut self, needles: &[&str], limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let screen = self.text();
            if needles.iter().any(|n| screen.contains(n)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        needles.iter().any(|n| self.text().contains(n))
    }

    /// First run: the capability card is on top and swallows keys, so a test
    /// that presses a key before dismissing it is testing the card, not the
    /// screen. Esc is the card's own skip.
    fn dismiss_onboarding(&mut self) {
        if !self.text().contains("What Mennoth can do") {
            return;
        }
        let esc = char::from(27).to_string();
        self.send(&esc);
        std::thread::sleep(Duration::from_millis(300));
        if self.text().contains("What Mennoth can do") {
            self.send(&esc);
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// A transcript on demand: `MEMNOTH_PTY_DUMP=1 cargo test --test tui_pty --
    /// --nocapture` prints the parsed screen, which is what a W13-16 transcript
    /// is made of.
    fn dump(&mut self, label: &str) {
        if std::env::var_os("MEMNOTH_PTY_DUMP").is_none() {
            return;
        }
        self.pump();
        println!("--- {label} ---\n{}", self.parser.screen().contents());
    }

    fn send(&mut self, keys: &str) {
        self.writer.write_all(keys.as_bytes()).expect("write keys");
        self.writer.flush().expect("flush keys");
    }

    /// ctrl-d is the screen's own quit key, so leaving is part of what a real
    /// terminal proves. A screen that cannot be left is a hung terminal.
    fn quit(mut self) -> bool {
        // An overlay owns esc, not ctrl-d: close it the way a person would, then
        // leave from the prompt.
        let esc = char::from(27).to_string();
        self.send(&esc);
        std::thread::sleep(Duration::from_millis(250));
        self.send(&char::from(4).to_string());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        false
    }
}

/// A failing assertion must not leave the product running: an orphaned TUI keeps
/// the pty and the target-dir lock, and the next `cargo test` waits forever.
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY needs an interactive console; this host starts processes without one"
)]
fn the_pty_captures_the_binarys_output() {
    let mut session = Session::open(&["--version"]);
    let seen = session.wait_for_any(&["memnoth"], Duration::from_secs(20));
    assert!(seen, "no output behind a pty:\n{}", session.diagnose());
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY needs an interactive console; this host starts processes without one. \
              Run `docker run --rm -v E:/memnoth-prod:/w -w /w/CLI rust:1-bookworm \
              cargo test --test tui_pty` (see the module header)."
)]
fn the_screen_opens_at_eighty_columns_and_can_be_left() {
    let mut session = Session::open(&["--trust-folder"]);
    assert!(
        session.wait_for_any(&["session", "memnoth"], Duration::from_secs(60)),
        "no frame within 60s:\n{}",
        session.diagnose()
    );
    assert_eq!(
        session.parser.screen().size(),
        (ROWS, COLS),
        "the app must adopt the terminal's size"
    );
    // The capability card is the widest thing drawn on a first run: if its
    // bottom border is on screen, the card fits 80 columns instead of being cut
    // off at the right edge.
    let screen = session.text();
    assert!(
        screen.contains("What Mennoth can do") && screen.contains('└'),
        "the first-run card is cut off at 80 columns:\n{screen}"
    );
    session.dump("brief at 80x24");
    session.dismiss_onboarding();
    session.dump("brief after the card");
    let screen = session.text();
    assert!(
        screen.lines().count() <= ROWS as usize,
        "more lines than the terminal has rows:\n{screen}"
    );
    assert!(session.quit(), "ctrl-d must leave the screen:\n{screen}");
}

/// Alt+A is ESC followed by `a` over a pty; the payload is two bytes.
fn alt(ch: char) -> String {
    format!("{}{}", char::from(27), ch)
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY needs an interactive console; this host starts processes without one. \
              Run `docker run --rm -v E:/memnoth-prod:/w -w /w/CLI rust:1-bookworm \
              cargo test --test tui_pty` (see the module header)."
)]
fn the_deck_focus_round_trip_at_eighty_columns() {
    // W13-24/W13-25: the deck is a list plus one detail block at 80x24, status
    // reads without colour, Enter focuses the child with a visible composer
    // target, and Esc returns to the deck and then to the prompt.
    let mut session = Session::open(&["--trust-folder"]);
    assert!(
        session.wait_for_any(&["session", "memnoth"], Duration::from_secs(60)),
        "no frame within 60s:\n{}",
        session.diagnose()
    );
    session.dismiss_onboarding();
    // Delegate: the child runs, the card lands in the timeline (W13-18).
    session.send("/delegate scout find the retry loop\r");
    assert!(
        session.wait_for_any(&["delegated to scout"], Duration::from_secs(20)),
        "the delegation card did not land:\n{}",
        session.diagnose()
    );
    assert!(
        session.wait_for_any(
            &["[Enter] open", "receipt: [p] view"],
            Duration::from_secs(10)
        ),
        "the result card did not carry its action keys and receipt:\n{}",
        session.diagnose()
    );
    session.dump("after /delegate at 80x24");
    // Alt+A opens the deck: roster plus inspector, never two columns.
    session.send(&alt('a'));
    assert!(
        session.wait_for_any(&["Agent Deck", "inspector"], Duration::from_secs(10)),
        "Alt+A did not open the deck:\n{}",
        session.diagnose()
    );
    let deck = session.text();
    assert!(
        deck.lines().count() <= ROWS as usize,
        "the deck drew taller than the terminal:\n{deck}"
    );
    for line in deck.lines() {
        assert!(
            line.chars().count() <= COLS as usize,
            "the deck scrolled horizontally: {line:?}"
        );
    }
    session.dump("deck at 80x24");
    // Space peeks the log tail in the inspector; it is read-only.
    session.send(" ");
    assert!(
        session.wait_for_any(&["run.done", "tool.use"], Duration::from_secs(10)),
        "Space did not peek the child log:\n{}",
        session.diagnose()
    );
    // p peeks the receipt into the same inspector.
    session.send("p");
    assert!(
        session.wait_for_any(&["\"summary\"", "changed_files"], Duration::from_secs(10)),
        "p did not peek the child receipt:\n{}",
        session.diagnose()
    );
    // m records the merge instruction in the transcript. Closing proves it
    // was not only the deck's inspector text.
    session.send("m");
    std::thread::sleep(Duration::from_millis(250));
    session.send(&char::from(27).to_string());
    assert!(
        session.wait_for_any(&["merge", "no worktree"], Duration::from_secs(10)),
        "m did not record the merge instruction:\n{}",
        session.diagnose()
    );
    session.send(&alt('a'));
    assert!(
        session.wait_for_any(&["Agent Deck"], Duration::from_secs(10)),
        "Alt+A did not reopen the deck after m:\n{}",
        session.diagnose()
    );
    // Enter focuses: the composer target names the child, and Esc returns.
    session.send("\r");
    assert!(
        session.wait_for_any(&["composer"], Duration::from_secs(10)),
        "Enter did not focus the child:\n{}",
        session.diagnose()
    );
    let focus = session.text();
    assert!(
        focus.contains("scout") || focus.contains("composer"),
        "focus does not name its child:\n{focus}"
    );
    session.dump("focus at 80x24");
    session.send(&char::from(27).to_string());
    assert!(
        session.wait_for_any(&["Agent Deck"], Duration::from_secs(10)),
        "Esc did not return to the deck:\n{}",
        session.diagnose()
    );
    assert!(session.quit(), "ctrl-d must leave after the round trip");
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY needs an interactive console; this host starts processes without one. \
              Run `docker run --rm -v E:/memnoth-prod:/w -w /w/CLI rust:1-bookworm \
              cargo test --test tui_pty` (see the module header)."
)]
fn alt_t_opens_the_trace_at_eighty_columns() {
    // W13-21: the trace is the nested tree plus the selected child's
    // timeline, both inside the 80x24 frame.
    let mut session = Session::open(&["--trust-folder"]);
    assert!(
        session.wait_for_any(&["session", "memnoth"], Duration::from_secs(60)),
        "no frame within 60s:\n{}",
        session.diagnose()
    );
    session.dismiss_onboarding();
    session.send("/delegate reviewer check the retry loop\r");
    assert!(
        session.wait_for_any(&["delegated to reviewer"], Duration::from_secs(20)),
        "the delegation card did not land:\n{}",
        session.diagnose()
    );
    session.send(&alt('t'));
    assert!(
        session.wait_for_any(&["Agent Trace"], Duration::from_secs(10)),
        "Alt+T did not open the trace:\n{}",
        session.diagnose()
    );
    let trace = session.text();
    assert!(
        trace.lines().count() <= ROWS as usize,
        "the trace drew taller than the terminal:\n{trace}"
    );
    session.dump("trace at 80x24");
    assert!(session.quit(), "ctrl-d must leave the trace too");
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY needs an interactive console; this host starts processes without one"
)]
fn question_mark_opens_help_at_eighty_columns() {
    let mut session = Session::open(&["--trust-folder"]);
    assert!(
        session.wait_for_any(&["session", "memnoth"], Duration::from_secs(60)),
        "no frame within 60s:\n{}",
        session.diagnose()
    );
    session.dismiss_onboarding();
    session.send("?");
    assert!(
        session.wait_for_any(
            &["Commands", "Keybindings", "Shortcuts"],
            Duration::from_secs(10)
        ),
        "? did not open help:\n{}",
        session.diagnose()
    );
    session.dump("help at 80x24");
    let help = session.text();
    assert!(
        help.lines().count() <= ROWS as usize,
        "help drew taller than the terminal:\n{help}"
    );
    assert!(session.quit(), "ctrl-d must leave help too:\n{help}");
}

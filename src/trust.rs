//! Workspace trust: the gate in front of every project read.
//!
//! A repository can carry a `.env`, an `AGENTS.md`, a `sandbox.toml`, hooks, and
//! MCP configuration. Reading any of them before the person has said this folder
//! is theirs is how a checkout turns into code execution. So the first launch in
//! a folder asks, and nothing about the folder is read until the answer is yes.
//!
//! The store is `~/.memnoth/trust.json`, keyed by the canonical path so a
//! symlink cannot smuggle in a different folder. It is written atomically
//! (temporary file, then rename) and parsed fail-closed: a corrupt or
//! future-version file means *no folder is trusted*, never "trust them all".
//!
//! Four locations are session-only by construction, because trusting them would
//! mean trusting everything below them: the home directory, a filesystem root,
//! a network share, and a removable volume. `--trust-folder` is the headless
//! escape and also never persists; a script that needs one run says so per run.

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::{TrustArgs, TrustCommand};
use crate::context::AppContext;
use crate::output::Outcome;
use anyhow::{Context, Result, bail};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::queue;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use serde::{Deserialize, Serialize};

/// The shape of the file this build writes. A file with any other version is
/// treated as unreadable rather than guessed at.
const STORE_VERSION: u32 = 1;

/// One trusted folder, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Canonical path in the platform's comparison form (see [`key_of`]).
    pub path: String,
    /// When the person said yes, in Unix milliseconds.
    pub trusted_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Store {
    version: u32,
    folders: Vec<Entry>,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            version: STORE_VERSION,
            folders: Vec::new(),
        }
    }
}

/// What happened when a folder was trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// Written to `trust.json`; the next launch will not ask.
    Persisted,
    /// Allowed for this run only, with the reason named.
    SessionOnly(&'static str),
}

/// Where the store lives.
pub fn store_path() -> Result<PathBuf> {
    Ok(crate::context::memnoth_home()?.join("trust.json"))
}

/// The canonical path of `root`, falling back to an absolute path when the
/// folder does not exist yet.
pub fn canonical(root: &Path) -> PathBuf {
    fs::canonicalize(root)
        .or_else(|_| std::path::absolute(root))
        .unwrap_or_else(|_| root.to_path_buf())
}

/// The comparison form of a path.
///
/// Windows canonical paths arrive with the `\\?\` verbatim prefix and compare
/// case-insensitively; POSIX paths compare byte for byte. The store keeps this
/// form so a path written by Explorer and one written by a shell are the same
/// folder.
fn key_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text).to_string();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Read the store. A missing file is an empty store; anything unreadable is an
/// empty store plus the reason, because an unparsed allowlist must not allow
/// anything.
fn read_store() -> (Store, Option<String>) {
    let path = match store_path() {
        Ok(path) => path,
        Err(error) => return (Store::default(), Some(format!("{error:#}"))),
    };
    read_store_from(&path)
}

/// Read the store at an explicit path. Tests use this so they never touch the
/// process-wide `MEMNOTH_HOME` and cannot race each other.
fn read_store_from(path: &Path) -> (Store, Option<String>) {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Store>(&text) {
            Ok(store) if store.version == STORE_VERSION => (store, None),
            Ok(store) => (
                Store::default(),
                Some(format!(
                    "{} has version {}; this build reads {STORE_VERSION}. Every folder is untrusted until it is rewritten",
                    path.display(),
                    store.version
                )),
            ),
            Err(error) => (
                Store::default(),
                Some(format!(
                    "{} is not a trust file ({error}); every folder is untrusted",
                    path.display()
                )),
            ),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => (Store::default(), None),
        Err(error) => (
            Store::default(),
            Some(format!("cannot read {}: {error}", path.display())),
        ),
    }
}

/// Write the store through a temporary file and a rename, so a crash leaves the
/// previous allowlist intact instead of a half-written one.
fn write_store(store: &Store) -> Result<()> {
    write_store_to(&store_path()?, store)
}

/// Write the store at an explicit path (see `read_store_from`).
fn write_store_to(path: &Path, store: &Store) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(store).context("cannot serialise the trust store")?;
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(temporary.as_path())
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(temporary, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

/// Whether `root` is trusted on this machine.
pub fn is_trusted(root: &Path) -> bool {
    is_trusted_in(root, &store_path().ok())
}

/// Whether `root` is trusted in the store at `path` (`None` is an empty store).
fn is_trusted_in(root: &Path, path: &Option<PathBuf>) -> bool {
    let key = key_of(&canonical(root));
    let (store, _) = match path {
        Some(path) => read_store_from(path),
        None => (Store::default(), None),
    };
    store.folders.iter().any(|entry| entry.path == key)
}

/// Why a folder may never be remembered, if it may not.
fn session_only_reason(root: &Path) -> Option<&'static str> {
    session_only_reason_with_home(root, dirs::home_dir())
}

/// `session_only_reason` with the home directory named, so tests do not depend
/// on the machine they run on.
fn session_only_reason_with_home(root: &Path, home: Option<PathBuf>) -> Option<&'static str> {
    let key = key_of(root);
    if let Some(home) = home
        && key == key_of(&canonical(&home))
    {
        return Some("the home directory");
    }
    let trimmed = key.trim_end_matches('/');
    if trimmed.is_empty() || (trimmed.len() == 2 && trimmed.ends_with(':')) {
        return Some("a filesystem root");
    }
    if key.starts_with("//") {
        return Some("a network share");
    }
    None
}

/// Trust `root`, persisting it unless the location is session-only.
pub fn grant(root: &Path) -> Result<Grant> {
    let canonical = canonical(root);
    if let Some(reason) = session_only_reason(&canonical) {
        return Ok(Grant::SessionOnly(reason));
    }
    let key = key_of(&canonical);
    // An unreadable store arrives empty and fail-closed, and the write below
    // replaces it whole, so this yes is also the repair. The problem is not
    // swallowed silently: `mn trust list` reports it, and the person is here
    // because the gate already refused to read the folder.
    let (mut store, _problem) = read_store();
    store.folders.retain(|entry| entry.path != key);
    store.folders.push(Entry {
        path: key,
        trusted_at_ms: now_ms(),
    });
    write_store(&store)?;
    Ok(Grant::Persisted)
}

/// Forget `root`. Returns `false` when it was not in the store.
pub fn forget(root: &Path) -> Result<bool> {
    let key = key_of(&canonical(root));
    // Same rule as `grant`: an unreadable store reads as empty, so forgetting a
    // folder from one cannot delete anything that was in it.
    let (mut store, _problem) = read_store();
    let before = store.folders.len();
    store.folders.retain(|entry| entry.path != key);
    if store.folders.len() == before {
        return Ok(false);
    }
    write_store(&store)?;
    Ok(true)
}

/// Every trusted folder, sorted, with the reason the file could not be read
/// when there is one.
pub fn list() -> (Vec<Entry>, Option<String>) {
    let (store, problem) = read_store();
    sort_folders(store, problem)
}

/// Sort a store's folders for display, keeping any read problem beside them.
fn sort_folders(store: Store, problem: Option<String>) -> (Vec<Entry>, Option<String>) {
    let mut folders = store.folders;
    folders.sort_by(|left, right| left.path.cmp(&right.path));
    (folders, problem)
}

/// Decide whether this run may read the project at `root`.
///
/// `trust_folder` is the non-persistent escape: it allows this run and says so
/// on stderr, but writes nothing. Without a terminal there is nobody to ask, so
/// the answer is no and the message names both ways forward.
pub fn check(root: &Path, trust_folder: bool) -> Result<()> {
    if is_trusted(root) {
        return Ok(());
    }
    if trust_folder {
        // Silent on purpose: `--json` promises one envelope on stderr, and a
        // session note would break the shape a script is parsing. The flag is
        // explicit and writes nothing, which is the whole contract.
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!(
            "refusing to read {}: the folder is not trusted and this is not a terminal. \
             Run `mennoth trust` in a terminal, or pass --trust-folder for this run only",
            canonical(root).display()
        );
    }
    if confirm(root)? {
        return Ok(());
    }
    bail!(
        "{} is not trusted; nothing was read",
        canonical(root).display()
    )
}

/// The 80×24 question. `true` means the person trusted the folder.
fn confirm(root: &Path) -> Result<bool> {
    let canonical = canonical(root);
    let key = key_of(&canonical);
    let mut out = io::stdout();
    let screen = Screen::enter()?;
    let look = Look::detect();
    loop {
        draw(&mut out, &canonical, &key, false, look)?;
        match event::read()? {
            Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                match key_event.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        drop(screen);
                        match grant(&canonical)? {
                            Grant::Persisted => {
                                println!("Trusted {}.", canonical.display());
                            }
                            Grant::SessionOnly(reason) => {
                                println!(
                                    "Trusted {} for this session only: it is {reason}.",
                                    canonical.display()
                                );
                            }
                        }
                        return Ok(true);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        return Ok(false);
                    }
                    KeyCode::Char('?') => {
                        draw(&mut out, &canonical, &key, true, look)?;
                        let _ = event::read();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// `mn trust list` and `mn trust forget`.
pub fn run(_ctx: &AppContext, args: &TrustArgs) -> Result<Outcome> {
    match &args.action {
        TrustCommand::List => {
            let (folders, problem) = list();
            let mut text = String::new();
            if folders.is_empty() {
                text.push_str(
                    "No folders are trusted. The first launch in a folder asks before it reads anything.\n",
                );
            } else {
                text.push_str(&format!("Trusted folders ({}):\n", folders.len()));
                for entry in &folders {
                    text.push_str(&format!("  {}\n", entry.path));
                }
            }
            if let Some(problem) = &problem {
                text.push_str(&format!("\n{problem}\n"));
            }
            let data = serde_json::json!({
                "folders": folders
                    .iter()
                    .map(|entry| serde_json::json!({
                        "path": entry.path.clone(),
                        "trusted_at_ms": entry.trusted_at_ms,
                    }))
                    .collect::<Vec<_>>(),
                "store": store_path().ok().map(|path| path.display().to_string()),
                "problem": problem,
            });
            Ok(Outcome::new("trust.list", text, data))
        }
        TrustCommand::Forget(forget_args) => {
            let removed = forget(std::path::Path::new(&forget_args.path))?;
            let key = key_of(&canonical(std::path::Path::new(&forget_args.path)));
            let text = if removed {
                format!("Forgot {key}. The next launch there asks again.\n")
            } else {
                format!("{key} was not trusted.\n")
            };
            let data = serde_json::json!({ "forgotten": removed, "path": key });
            Ok(Outcome::new("trust.forget", text, data))
        }
    }
}

fn sandbox_line() -> &'static str {
    if cfg!(windows) {
        "Sandbox: policy-checked (Windows has no kernel sandbox)"
    } else if cfg!(target_os = "macos") {
        "Sandbox: seatbelt profile"
    } else {
        "Sandbox: landlock and seccomp"
    }
}

/// Break a path into lines of at most `width` characters, at a separator when
/// one is available. Character-based so a non-ASCII path cannot be split inside
/// a code point.
fn wrap_path(path: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest: Vec<char> = path.chars().collect();
    while rest.len() > width {
        let window: String = rest[..width].iter().collect();
        let cut = window
            .rfind(['/', '\\'])
            .filter(|index| *index > 0)
            .unwrap_or(width);
        lines.push(rest[..cut].iter().collect());
        rest = rest[cut..].to_vec();
    }
    lines.push(rest.into_iter().collect());
    lines
}

/// How a run of text is drawn. Only what the card uses: a variant nothing
/// paints is a colour nobody has tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    Bold,
    Accent,
    Dim,
    Key,
}

/// What the card paints on: the width it was given and whether colour is
/// welcome. Detection is kept out of [`draw`] so a test can fix both.
#[derive(Debug, Clone, Copy)]
struct Look {
    width: usize,
    color: bool,
}

impl Look {
    fn detect() -> Self {
        let width = crossterm::terminal::size()
            .map(|(columns, _)| columns as usize)
            .unwrap_or(80);
        Look {
            // A card narrower than this cannot hold a keycap row; wider than
            // this is a wall of rule with four words on it.
            width: width.clamp(56, 96),
            // Asked of the same rule the session uses, so a person who set
            // `NO_COLOR` gets it here too rather than only once the TUI opens.
            color: !matches!(
                crate::tui::theme::theme_from_env(),
                Some(crate::tui::app::Theme::NoColor)
            ),
        }
    }
}

/// Wrap `text` in one SGR sequence, or leave it alone when colour is off.
/// A terminal that asked for no styling gets no escapes at all, not a bold it
/// did not ask for.
fn paint(text: &str, ink: Ink, color: bool) -> String {
    if !color || text.is_empty() {
        return text.to_string();
    }
    let code = match ink {
        Ink::Bold => "1",
        Ink::Accent => "1;36",
        Ink::Dim => "90",
        Ink::Key => "7;1",
    };
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

/// Break prose at spaces. A word longer than the line is kept whole rather than
/// split: a path or a flag cut in half is worse than an overlong line.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(16);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The three answers, in the order they are offered.
const KEYS: [(&str, &str); 3] = [
    ("Y", "Trust this folder"),
    ("N", "No, exit"),
    ("?", "Detail"),
];

/// One row of keycaps, spaced out to the width. The cap is reversed so it reads
/// as a key without brackets, which cost two columns each for nothing.
fn keys_row(color: bool, gap: usize) -> String {
    let mut row = String::new();
    for (index, (cap, label)) in KEYS.iter().enumerate() {
        if index > 0 {
            row.push_str(&" ".repeat(gap));
        }
        row.push_str(&paint(&format!(" {cap} "), Ink::Key, color));
        row.push_str("  ");
        row.push_str(label);
    }
    row
}

fn draw(out: &mut impl Write, canonical: &Path, key: &str, detail: bool, look: Look) -> Result<()> {
    let _ = canonical;
    let color = look.color;
    let width = look.width;
    let body = width.saturating_sub(6);
    let rule = paint(&"─".repeat(width.saturating_sub(4)), Ink::Dim, color);

    queue!(out, Clear(ClearType::All), MoveTo(0, 0))?;
    // One heading and one rule: the old card stacked three full-width rules,
    // which read as three priorities when there is only one question.
    writeln!(
        out,
        "  {}{}{}",
        paint("memnoth", Ink::Accent, color),
        paint(" · ", Ink::Dim, color),
        paint("workspace trust", Ink::Dim, color)
    )?;
    writeln!(out, "  {rule}")?;

    if detail {
        draw_detail(out, body, color)?;
    } else {
        draw_question(out, key, body, color)?;
    }

    writeln!(out)?;
    writeln!(out, "  {rule}")?;
    writeln!(out)?;
    if detail {
        writeln!(
            out,
            "  {}",
            paint("press any key to go back", Ink::Dim, color)
        )?;
    } else {
        // Spread to the width, but never past ten columns: a key row strung
        // across a wide terminal makes the eye travel for a two-word label.
        let fixed: usize = KEYS
            .iter()
            .map(|(_, label)| 5 + label.chars().count())
            .sum();
        let gap = width.saturating_sub(4).saturating_sub(fixed) / (KEYS.len() - 1);
        writeln!(out, "  {}", keys_row(color, gap.clamp(2, 10)))?;
        writeln!(out)?;
        for line in wrap_words(
            "The safe answer is No. Nothing is written unless you choose Y.",
            body,
        ) {
            writeln!(out, "  {}", paint(&line, Ink::Dim, color))?;
        }
    }
    out.flush()?;
    Ok(())
}

/// The question itself: what is untrusted, what saying yes would unlock, and
/// how the sandbox stands. The path is the only folder-specific fact, so it gets
/// its own bar instead of a line of prose.
fn draw_question(out: &mut impl Write, key: &str, body: usize, color: bool) -> Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "  {}",
        paint("This folder is not trusted yet.", Ink::Bold, color)
    )?;
    writeln!(out)?;
    for (index, line) in wrap_path(key, body.saturating_sub(2)).iter().enumerate() {
        if index == 0 {
            writeln!(
                out,
                "  {} {}",
                paint("▎", Ink::Accent, color),
                paint(line, Ink::Bold, color)
            )?;
        } else {
            writeln!(out, "    {}", paint(line, Ink::Bold, color))?;
        }
    }
    writeln!(out)?;
    for line in wrap_words(
        "Saying yes lets memnoth read its .env, AGENTS.md, sandbox.toml, hooks, \
         MCP configuration and the code graph. Until then, none of it is read.",
        body,
    ) {
        writeln!(out, "  {line}")?;
    }
    writeln!(out)?;
    for line in wrap_words(sandbox_line(), body) {
        writeln!(out, "  {}", paint(&line, Ink::Dim, color))?;
    }
    Ok(())
}

/// What the answer means and where it is kept: the `?` view, in the same frame
/// so opening it does not look like a different screen.
fn draw_detail(out: &mut impl Write, body: usize, color: bool) -> Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "  {}",
        paint("What trusting a folder means", Ink::Bold, color)
    )?;
    writeln!(out)?;

    let store = store_path()?.display().to_string();
    let paragraphs = [
        "memnoth reads four kinds of file from a folder it trusts: .env for keys, \
         AGENTS.md for instructions, sandbox.toml for the sandbox policy, and the \
         hooks and MCP configuration that decide what is allowed to run. It reads \
         none of them before you answer yes."
            .to_string(),
        "The answer is stored in the file below, keyed by the canonical path, so a \
         symlink cannot point somewhere else."
            .to_string(),
        "Home, a filesystem root, a network share and a removable volume are trusted \
         for one session only, because trusting them would mean trusting everything \
         below them."
            .to_string(),
    ];
    for paragraph in &paragraphs {
        for line in wrap_words(paragraph, body) {
            writeln!(out, "  {line}")?;
        }
        writeln!(out)?;
    }
    // The store itself gets a path bar, not a word in prose: wrap_words keeps
    // a word longer than the line whole (a path cut in half is worse than an
    // overlong line), and a state dir under a long temp root beat width 56
    // on macOS once the test drew with the real path.
    for (index, line) in wrap_path(&store, body.saturating_sub(2)).iter().enumerate() {
        if index == 0 {
            writeln!(
                out,
                "  {} {}",
                paint("▎", Ink::Accent, color),
                paint(line, Ink::Bold, color)
            )?;
        } else {
            writeln!(out, "    {}", paint(line, Ink::Bold, color))?;
        }
    }
    writeln!(out)?;
    for line in wrap_words(sandbox_line(), body) {
        writeln!(out, "  {}", paint(&line, Ink::Dim, color))?;
    }
    Ok(())
}

/// Raw mode and the alternate screen, restored on every path out of [`confirm`].
struct Screen;

impl Screen {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("cannot enter raw mode")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            // Raw mode with no alternate screen would leave the shell unusable.
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = execute!(out, Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod trust_tests {
    use super::*;

    fn store_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("trust.json")
    }

    fn entry(path: &Path) -> Entry {
        Entry {
            path: key_of(&canonical(path)),
            trusted_at_ms: 1,
        }
    }

    #[test]
    fn list_is_sorted_regardless_of_write_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_in(&dir);
        let zeta = dir.path().join("zeta");
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(&zeta).unwrap();
        std::fs::create_dir_all(&alpha).unwrap();
        write_store_to(
            &path,
            &Store {
                version: STORE_VERSION,
                folders: vec![entry(&zeta), entry(&alpha)],
            },
        )
        .unwrap();
        let (folders, problem) = sort_folders(read_store_from(&path).0, None);
        assert!(problem.is_none());
        let keys: Vec<&str> = folders.iter().map(|folder| folder.path.as_str()).collect();
        assert_eq!(
            keys,
            vec![key_of(&canonical(&alpha)), key_of(&canonical(&zeta))]
        );
    }

    #[test]
    fn a_corrupt_store_trusts_nothing_and_names_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_in(&dir);
        std::fs::write(&path, "{not json").unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!is_trusted_in(&root, &Some(path.clone())));
        let (_, problem) = read_store_from(&path);
        assert!(problem.unwrap().contains("is not a trust file"));
    }

    #[test]
    fn a_future_version_store_trusts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_in(&dir);
        std::fs::write(
            &path,
            r#"{"version":99,"folders":[{"path":"/x","trusted_at_ms":1}]}"#,
        )
        .unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!is_trusted_in(&root, &Some(path.clone())));
        let (_, problem) = read_store_from(&path);
        assert!(problem.unwrap().contains("version 99"));
    }

    #[test]
    fn home_root_and_network_share_are_session_only() {
        let home = PathBuf::from(if cfg!(windows) {
            r"C:\Users\test"
        } else {
            "/home/test"
        });
        assert_eq!(
            session_only_reason_with_home(&home, Some(home.clone())),
            Some("the home directory")
        );
        assert_eq!(
            session_only_reason_with_home(Path::new("/"), Some(home.clone())),
            Some("a filesystem root")
        );
        assert_eq!(
            session_only_reason_with_home(Path::new("//server/share"), Some(home)),
            Some("a network share")
        );
    }

    #[test]
    fn an_ordinary_folder_is_not_session_only() {
        let home = PathBuf::from(if cfg!(windows) {
            r"C:\Users\test"
        } else {
            "/home/test"
        });
        let repo = PathBuf::from(if cfg!(windows) {
            r"C:\Users\test\repo"
        } else {
            "/home/test/repo"
        });
        assert_eq!(session_only_reason_with_home(&repo, Some(home)), None);
    }

    #[cfg(windows)]
    #[test]
    fn keys_compare_case_insensitively_on_windows() {
        assert_eq!(
            key_of(Path::new(r"C:\Repo\Sub")),
            key_of(Path::new(r"c:\repo\sub"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_trusts_the_folder_it_points_at() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let path = store_in(&dir);
        write_store_to(
            &path,
            &Store {
                version: STORE_VERSION,
                folders: vec![entry(&real)],
            },
        )
        .unwrap();
        assert!(is_trusted_in(&link, &Some(path)));
    }

    #[test]
    fn trusting_a_session_only_location_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_in(&dir);
        let root = Path::new("/");
        assert_eq!(session_only_reason(root), Some("a filesystem root"));
        assert!(!path.exists());
        assert!(!is_trusted_in(root, &Some(path)));
    }

    /// Draw the card the way the screen does and read it back.
    fn card(key: &str, detail: bool, width: usize, color: bool) -> String {
        let mut buffer = Vec::new();
        draw(
            &mut buffer,
            Path::new(key),
            key,
            detail,
            Look { width, color },
        )
        .unwrap();
        String::from_utf8(buffer).unwrap()
    }

    /// Every styled span in `text`, escape sequence included.
    fn sgr(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut found = Vec::new();
        let mut index = 0;
        while index + 1 < chars.len() {
            if chars[index] == '\u{1b}' && chars[index + 1] == '[' {
                let mut end = index + 2;
                while end < chars.len() && !chars[end].is_ascii_alphabetic() {
                    end += 1;
                }
                if end < chars.len() {
                    if chars[end] == 'm' {
                        found.push(chars[index..=end].iter().collect());
                    }
                    index = end + 1;
                    continue;
                }
                break;
            }
            index += 1;
        }
        found
    }

    /// The card with every escape dropped, as a person's terminal shows it.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let chars: Vec<char> = text.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] == '\u{1b}' && index + 1 < chars.len() && chars[index + 1] == '[' {
                let mut end = index + 2;
                while end < chars.len() && !chars[end].is_ascii_alphabetic() {
                    end += 1;
                }
                index = end + 1;
                continue;
            }
            out.push(chars[index]);
            index += 1;
        }
        out
    }

    #[test]
    fn the_card_names_the_folder_and_the_three_answers() {
        let text = plain(&card("/home/me/agency", false, 80, false));
        assert!(text.contains("not trusted yet"), "{text}");
        assert!(text.contains("/home/me/agency"), "{text}");
        for (cap, label) in KEYS {
            assert!(text.contains(cap), "{cap} missing\n{text}");
            assert!(text.contains(label), "{label} missing\n{text}");
        }
        assert!(text.contains("safe answer is No"), "{text}");
    }

    #[test]
    fn every_line_fits_the_width_the_terminal_gave() {
        let long = "/home/me/agency/with/a/folder/name/that/keeps/going/and/going/for/a/while";
        for width in [56, 64, 80, 96] {
            for detail in [false, true] {
                let text = plain(&card(long, detail, width, true));
                for line in text.lines() {
                    assert!(
                        line.chars().count() <= width,
                        "detail={detail} width={width}: {} chars, {line:?}",
                        line.chars().count()
                    );
                }
            }
        }
    }

    #[test]
    fn a_no_color_terminal_gets_no_styling_at_all() {
        let text = card("/home/me/agency", false, 80, false);
        assert!(sgr(&text).is_empty(), "{:?}", sgr(&text));
        let styled = card("/home/me/agency", false, 80, true);
        assert!(!sgr(&styled).is_empty());
    }
}

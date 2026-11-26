//! Host-selection wizard for `mn setup --wizard`.
//!
//! `mn setup` runs outside the TUI, so this is a plain line-oriented menu:
//! every key is read as a line from stdin and the menu is redrawn with
//! `eprintln!`. There is no raw terminal mode, no alternate screen, and no
//! dependency on a terminal at all — when stdin is not a TTY the caller prints
//! the plan and falls back to the detected hosts.
//!
//! Two invariants matter here:
//!
//! * Detection only sorts and badges rows. Every host in
//!   [`ClientKind::ALL`] is offered, detected or not, and every row can be
//!   toggled. A missing config file is not a refusal to register a host.
//! * The verdict is never "configured". A written config is `needs restart`
//!   until the `server/discover` proof that `mcp::add` already performs says
//!   otherwise; only then is it `registered`.

use anyhow::{Context, Result, bail};
use memnoth_capability::Profile;
use serde::Serialize;
use std::path::Path;

use crate::context::AppContext;
use crate::mcp::clients::{self, ClientKind};

const DETECTED_BADGE: &str = "[detected]";
const UNDETECTED_BADGE: &str = "[not detected]";

/// One offered host. `detected` is an observation used for sorting, badging and
/// the non-interactive default — never a gate on whether the row is selectable.
#[derive(Debug, Clone)]
pub(crate) struct HostRow {
    pub(crate) kind: ClientKind,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) detected: bool,
    pub(crate) selected: bool,
}

/// A key from the line-oriented menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Up,
    Down,
    Toggle,
    AllDetected,
    Confirm,
    Cancel,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuOutcome {
    Confirmed,
    Cancelled,
}

/// One applied host's honest result.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HostResult {
    pub(crate) client: String,
    pub(crate) path: String,
    /// Exactly one of `registered`, `needs restart`, `not configured`.
    pub(crate) verdict: String,
    /// Error and stderr text, passed through [`redact`].
    pub(crate) detail: String,
    /// Internal: `server/discover` answered for this run. Not serialized.
    #[serde(skip)]
    pub(crate) proof: bool,
}

/// What the wizard did, for the `Outcome` envelope and the summary text.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WizardReport {
    pub(crate) dry_run: bool,
    pub(crate) selected: Vec<String>,
    pub(crate) results: Vec<HostResult>,
    /// Exactly one of `registered`, `needs restart`, `not configured`.
    pub(crate) verdict: String,
    pub(crate) server_detail: String,
}

/// The same stdin guard `confirm_simple` uses. Kept here so the wizard can ask
/// before entering its menu without a second policy for what "interactive"
/// means. `std::io::IsTerminal` is stable; the `atty` crate is not maintained.
pub(crate) fn stdin_is_terminal() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}

/// Every supported host, detected ones first, with all detected ones preselected.
pub(crate) fn build_rows(home: &Path, workspace: &Path, detected: &[ClientKind]) -> Vec<HostRow> {
    let mut rows: Vec<HostRow> = ClientKind::ALL
        .into_iter()
        .map(|kind| {
            let is_detected = detected.contains(&kind);
            HostRow {
                kind,
                name: kind.name().to_string(),
                path: kind
                    .config_path_for(home, Some(workspace))
                    .display()
                    .to_string(),
                detected: is_detected,
                selected: is_detected,
            }
        })
        .collect();
    // Stable within each group, so the menu order is predictable in tests and
    // for a person using the arrow keys.
    rows.sort_by(|a, b| {
        b.detected
            .cmp(&a.detected)
            .then_with(|| a.name.cmp(&b.name))
    });
    rows
}

pub(crate) fn selected_rows(rows: &[HostRow]) -> Vec<HostRow> {
    rows.iter().filter(|row| row.selected).cloned().collect()
}

/// The no-TTY default: detected hosts only, in menu order. This function never
/// reads stdin, so a piped or closed input cannot hang setup.
pub(crate) fn non_interactive_selection(rows: &[HostRow]) -> Vec<HostRow> {
    rows.iter().filter(|row| row.detected).cloned().collect()
}

/// The menu text for one frame. Kept separate from stdin so it can be tested
/// without a terminal.
pub(crate) fn render_menu(rows: &[HostRow], cursor: usize) -> String {
    let mut out = String::new();
    out.push_str("select hosts for the local read-only MCP server\n");
    out.push_str("  one key per line: j/k or arrows move, space toggles, a selects detected\n");
    out.push_str("  enter confirms, esc/q cancels (press enter after each key)\n");
    for (index, row) in rows.iter().enumerate() {
        let pointer = if index == cursor { '>' } else { ' ' };
        let mark = if row.selected { 'x' } else { ' ' };
        let badge = if row.detected {
            DETECTED_BADGE
        } else {
            UNDETECTED_BADGE
        };
        out.push_str(&format!(
            "  {pointer} [{mark}] {:<10} {:<14} {}\n",
            row.name, badge, row.path
        ));
    }
    let count = rows.iter().filter(|row| row.selected).count();
    out.push_str(&format!("  {count} host(s) selected\n"));
    out
}

/// Interpret one line of input. Arrow keys arrive as an escape sequence after
/// Enter in canonical mode; the single-letter aliases need no such trick.
pub(crate) fn parse_key(line: &str) -> MenuAction {
    let key = line.trim_end_matches(['\r', '\n']);
    match key {
        "" => MenuAction::Confirm,
        "\x1b[A" | "up" | "k" | "K" => MenuAction::Up,
        "\x1b[B" | "down" | "j" | "J" => MenuAction::Down,
        " " => MenuAction::Toggle,
        "a" | "A" => MenuAction::AllDetected,
        "\x1b" | "esc" | "q" | "Q" => MenuAction::Cancel,
        _ => MenuAction::Ignore,
    }
}

/// Drive the menu from an iterator of input lines, emitting each frame. EOF
/// without an explicit confirm is a cancellation, never an accidental apply.
pub(crate) fn run_menu(
    rows: &mut [HostRow],
    keys: impl IntoIterator<Item = String>,
    mut emit: impl FnMut(&str),
) -> MenuOutcome {
    if rows.is_empty() {
        return MenuOutcome::Confirmed;
    }
    let mut cursor = 0usize;
    emit(&render_menu(rows, cursor));
    for key in keys {
        match parse_key(&key) {
            MenuAction::Up => cursor = cursor.saturating_sub(1),
            MenuAction::Down => cursor = (cursor + 1).min(rows.len() - 1),
            MenuAction::Toggle => {
                if let Some(row) = rows.get_mut(cursor) {
                    row.selected = !row.selected;
                }
            }
            MenuAction::AllDetected => {
                for row in rows.iter_mut() {
                    if row.detected {
                        row.selected = true;
                    }
                }
            }
            MenuAction::Confirm => return MenuOutcome::Confirmed,
            MenuAction::Cancel => return MenuOutcome::Cancelled,
            MenuAction::Ignore => {}
        }
        emit(&render_menu(rows, cursor));
    }
    MenuOutcome::Cancelled
}

/// The exact plan, including each host's config path and the stdio command the
/// registration will launch. Every interpolated string goes through [`redact`].
pub(crate) fn plan_text(selected: &[HostRow], ctx: &AppContext, dry_run: bool) -> String {
    let command = std::env::current_exe()
        .unwrap_or_else(|_| ctx.exe_path.clone())
        .display()
        .to_string();
    let profile = Profile::from_name(&ctx.profile()).unwrap_or(Profile::Full);
    let mut out = String::new();
    out.push_str(if dry_run {
        "plan (dry run — nothing will be written):\n"
    } else {
        "plan:\n"
    });
    for row in selected {
        out.push_str(&format!(
            "  {} -> {}\n",
            redact(&row.name),
            redact(&row.path)
        ));
        out.push_str(&format!(
            "    command: {} mcp serve --db {}   [MEMNOTH_MCP_PROFILE={}, MEMNOTH_MCP_READ_ONLY=1]\n",
            redact(&command),
            redact(&ctx.vault_path.display().to_string()),
            redact(profile.name())
        ));
        out.push_str(&format!(
            "    restart {} to load tools\n",
            redact(&row.name)
        ));
    }
    if selected.is_empty() {
        out.push_str("  (no hosts selected)\n");
    }
    out
}

/// Copy a file aside before it is overwritten, never clobbering an older
/// backup. Mirrors `doctor::backup_if_exists` (read-only reference) because
/// that helper is private to `doctor`.
fn backup_if_exists(path: &Path) -> Result<Option<std::path::PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut candidate = {
        let mut name = path.as_os_str().to_os_string();
        name.push(".bak");
        std::path::PathBuf::from(name)
    };
    let mut n = 1;
    while candidate.exists() {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".bak.{n}"));
        candidate = std::path::PathBuf::from(name);
        n += 1;
        if n > 99 {
            bail!("too many backups beside {}", path.display());
        }
    }
    std::fs::copy(path, &candidate)
        .with_context(|| format!("back up {} to {}", path.display(), candidate.display()))?;
    Ok(Some(candidate))
}

/// Apply the chosen hosts through `mcp::add` only — no config writer is
/// reimplemented here and `doctor --fix` is never used.
///
/// NOTE: `mcp::add`'s client writers (`write_json`, `write_codex`,
/// `write_opencode`, `write_zed_jsonc` in `mcp/clients.rs`) use `fs::write`
/// directly rather than a temp-file-plus-rename atomic replace. Making the
/// write itself atomic belongs in those writers, which this phase must not
/// edit. Until then the wizard backs up every config it is about to touch.
pub(crate) fn apply_selected(
    ctx: &AppContext,
    selected: &[HostRow],
    dry_run: bool,
) -> Result<WizardReport> {
    let home = clients::home_dir().context(
        "cannot find a home directory (set HOME or USERPROFILE); no client config can be written",
    )?;
    let mut results = Vec::new();

    if !dry_run && !selected.is_empty() {
        // `mcp::add` also persists the chosen profile in memnoth's own config.
        backup_if_exists(&ctx.config_path)
            .with_context(|| format!("cannot back up {}", ctx.config_path.display()))?;
    }

    for row in selected {
        let path = row.kind.config_path_for(&home, Some(&ctx.root));
        let (verdict, detail, proof) = if dry_run {
            (
                "not configured",
                format!("dry run: would register {} at {}", row.name, path.display()),
                false,
            )
        } else {
            let backup = match backup_if_exists(&path) {
                Ok(backup) => backup,
                Err(error) => {
                    results.push(HostResult {
                        client: row.name.clone(),
                        path: path.display().to_string(),
                        verdict: "not configured".to_string(),
                        detail: redact(&format!("skipped: backup failed: {error:#}")),
                        proof: false,
                    });
                    continue;
                }
            };
            let names = [row.name.clone()];
            let profile = Profile::from_name(&ctx.profile()).unwrap_or(Profile::Full);
            let options = crate::mcp::AddOptions {
                client_names: &names,
                profile: Some(profile),
                url: None,
                token: None,
                db: Some(ctx.vault_path.clone()),
                team: None,
                local: true,
                allow_writes: false,
                dry_run: false,
            };
            match crate::mcp::add(ctx, options) {
                Ok(report) => {
                    let status = report
                        .results
                        .iter()
                        .find(|status| status.client == row.name)
                        .or_else(|| report.results.first());
                    let registered = status.is_some_and(|status| status.registered);
                    // `mcp::add` already probes `server/discover` in
                    // `verify_written`; reimplementing a stdio client here would
                    // be a second, weaker proof of the same fact.
                    let proof = report
                        .verification
                        .as_ref()
                        .is_some_and(|verification| verification.ok && verification.initialized);
                    let mut detail = status
                        .map(|status| status.detail.clone())
                        .unwrap_or_else(|| "no status returned by mcp add".to_string());
                    if let Some(backup) = backup {
                        detail.push_str(&format!(" (backed up to {})", backup.display()));
                    }
                    if let Some(verification) = &report.verification
                        && !verification.detail.is_empty()
                    {
                        detail.push_str(&format!("; verify: {}", verification.detail));
                    }
                    (
                        if registered {
                            "needs restart"
                        } else {
                            "not configured"
                        },
                        detail,
                        proof,
                    )
                }
                Err(error) => (
                    "not configured",
                    format!("mcp add failed: {error:#}"),
                    false,
                ),
            }
        };
        results.push(HostResult {
            client: row.name.clone(),
            path: path.display().to_string(),
            verdict: verdict.to_string(),
            detail: redact(&detail),
            proof,
        });
    }

    let mut report = WizardReport {
        dry_run,
        selected: selected.iter().map(|row| row.name.clone()).collect(),
        results,
        verdict: "not configured".to_string(),
        server_detail: if dry_run {
            "dry run: verification not run".to_string()
        } else if selected.is_empty() {
            "no hosts selected: verification not run".to_string()
        } else {
            String::new()
        },
    };
    if !dry_run && !selected.is_empty() {
        let doctor = crate::mcp::doctor(ctx);
        report.finish_verification(doctor);
    }
    Ok(report)
}

impl WizardReport {
    /// Fold the `mcp::doctor` report into the verdict. A config that was
    /// written but not proved by `server/discover` stays `needs restart`; only
    /// a direct proof promotes it to `registered`.
    fn finish_verification(&mut self, doctor: Result<crate::mcp::DoctorReport>) {
        self.server_detail = match doctor {
            Ok(report) => {
                let selftest = match report.server_ok {
                    Some(true) => "server selftest ok",
                    Some(false) => "server selftest failed",
                    None => "no server selftest",
                };
                redact(&format!("{selftest}: {}", report.server_detail))
            }
            Err(error) => redact(&format!("mcp doctor failed: {error:#}")),
        };
        for result in &mut self.results {
            if result.verdict != "not configured" {
                result.verdict = if result.proof {
                    "registered"
                } else {
                    "needs restart"
                }
                .to_string();
            }
        }
        self.verdict = if self.results.iter().any(|r| r.verdict == "registered") {
            "registered"
        } else if self.results.iter().any(|r| r.verdict == "needs restart") {
            "needs restart"
        } else {
            "not configured"
        }
        .to_string();
    }

    /// Human-readable lines appended to the setup summary. A restart line is
    /// printed for every host, regardless of verdict: registration is never a
    /// claim that a running agent has loaded the new tools.
    pub(crate) fn summary(&self) -> String {
        let mut out = format!(
            "mcp wizard: {} (selected: {})\n",
            self.verdict,
            if self.selected.is_empty() {
                "none".to_string()
            } else {
                self.selected.join(", ")
            }
        );
        for result in &self.results {
            out.push_str(&format!(
                "  {}: {} — {}\n",
                result.client, result.verdict, result.detail
            ));
        }
        if !self.server_detail.is_empty() {
            out.push_str(&format!("  server: {}\n", self.server_detail));
        }
        if self.results.is_empty() {
            out.push_str("  restart your agent to load tools\n");
        } else {
            for result in &self.results {
                out.push_str(&format!("  restart {} to load tools\n", result.client));
            }
        }
        out
    }
}

/// A no-write report for `--dry-run` and for a cancelled wizard.
pub(crate) fn dry_run_report(selected: &[HostRow]) -> WizardReport {
    WizardReport {
        dry_run: true,
        selected: selected.iter().map(|row| row.name.clone()).collect(),
        results: selected
            .iter()
            .map(|row| HostResult {
                client: row.name.clone(),
                path: row.path.clone(),
                verdict: "not configured".to_string(),
                detail: "dry run: nothing written".to_string(),
                proof: false,
            })
            .collect(),
        verdict: "not configured".to_string(),
        server_detail: "dry run: verification not run".to_string(),
    }
}

/// Strip bearer tokens, `sk-` keys and JWTs from anything we echo. This is a
/// small, dependency-free redactor for host CLI stderr, not a parser: it
/// errs toward hiding a secret that looks like one.
pub(crate) fn redact(text: &str) -> String {
    let chunks: Vec<&str> = text.split_inclusive(|c: char| c.is_whitespace()).collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chunks.len() {
        let chunk = chunks[i];
        let word = chunk.trim_end_matches(char::is_whitespace);
        let tail = &chunk[word.len()..];
        if word.eq_ignore_ascii_case("bearer")
            && let Some(next) = chunks.get(i + 1)
        {
            let next_word = next.trim_end_matches(char::is_whitespace);
            if looks_like_secret(next_word) {
                out.push_str("[REDACTED]");
                out.push_str(&next[next_word.len()..]);
                out.push_str(tail);
                i += 2;
                continue;
            }
        }
        out.push_str(&redact_word(word));
        out.push_str(tail);
        i += 1;
    }
    out
}

fn redact_word(word: &str) -> String {
    if looks_like_secret(word) {
        return "[REDACTED]".to_string();
    }
    // `MEMNOTH_MCP_TOKEN=sk-...` and `token: sk-...` are one word, so redact
    // just the value and keep the key that made the line useful.
    if let Some(pos) = word.rfind('=').or_else(|| word.rfind(':'))
        && looks_like_secret(&word[pos + 1..])
    {
        return format!("{}[REDACTED]", &word[..=pos]);
    }
    word.to_string()
}

fn looks_like_secret(word: &str) -> bool {
    let word = word.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '(' | ')' | ',' | ';'));
    let lower = word.to_ascii_lowercase();
    if lower.starts_with("sk-")
        && word.len() >= 8
        && word[3..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return true;
    }
    // JWT: header.payload.signature. Requiring an `eyJ` header avoids hiding
    // ordinary dotted version strings.
    let parts: Vec<&str> = word.split('.').collect();
    parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Config;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    /// Tests change HOME/USERPROFILE because `mcp::add` resolves client configs
    /// from the environment. Host-CLI interception is blocked by
    /// `MEMNOTH_TEST_NO_HOST_CLI` (in `clients::find_binary`) rather than by
    /// clobbering PATH — a global empty PATH also broke unrelated tests that
    /// spawn `git`. Hold a static lock and restore every override on drop.
    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        home: Option<OsString>,
        userprofile: Option<OsString>,
        no_host_cli: Option<OsString>,
    }

    impl EnvGuard {
        fn set(dir: &Path) -> Self {
            static LOCK: Mutex<()> = Mutex::new(());
            let lock = LOCK.lock().unwrap_or_else(|error| error.into_inner());
            let home = std::env::var_os("HOME");
            let userprofile = std::env::var_os("USERPROFILE");
            let no_host_cli = std::env::var_os("MEMNOTH_TEST_NO_HOST_CLI");
            unsafe {
                // `clients::home_dir()` prefers USERPROFILE on Windows. Set
                // both, or a test would write into the real user's configs.
                std::env::set_var("HOME", dir);
                std::env::set_var("USERPROFILE", dir);
                std::env::set_var("MEMNOTH_TEST_NO_HOST_CLI", "1");
            }
            Self {
                _lock: lock,
                home,
                userprofile,
                no_host_cli,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                for (key, value) in [
                    ("HOME", &self.home),
                    ("USERPROFILE", &self.userprofile),
                    ("MEMNOTH_TEST_NO_HOST_CLI", &self.no_host_cli),
                ] {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    fn test_ctx(dir: &Path) -> AppContext {
        AppContext {
            root: dir.to_path_buf(),
            home: dir.join("state"),
            vault_path: dir.join("state").join("vault.db"),
            namespace: "default".to_string(),
            graph_path: dir.join(".memnoth").join("graph.db"),
            sessions_dir: dir.join("state").join("sessions"),
            config_path: dir.join("state").join("config.toml"),
            config: Config::default(),
            json: true,
            quiet: true,
            progress_tx: None,
            exe_path: Path::new("mn").to_path_buf(),
        }
    }

    fn fake_hosts(dir: &Path) {
        std::fs::create_dir_all(dir.join("state")).expect("create state dir");
        std::fs::write(dir.join(".claude.json"), "{}\n").expect("write claude config");
        std::fs::create_dir_all(dir.join(".codex")).expect("create codex dir");
        std::fs::write(dir.join(".codex").join("config.toml"), "").expect("write codex config");
    }

    #[test]
    fn menu_lists_every_host_and_badges_detected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let detected = [ClientKind::Claude, ClientKind::Codex];
        let rows = build_rows(dir.path(), dir.path(), &detected);
        assert_eq!(rows.len(), ClientKind::ALL.len());
        let frame = render_menu(&rows, 0);
        for kind in ClientKind::ALL {
            assert!(frame.contains(kind.name()), "{frame}");
        }
        assert!(frame.contains(DETECTED_BADGE), "{frame}");
        assert!(frame.contains(UNDETECTED_BADGE), "{frame}");
        assert_eq!(rows[0].name, "claude");
        assert_eq!(rows[1].name, "codex");
        assert!(rows.iter().all(|row| row.detected == row.selected));
    }

    #[test]
    fn menu_keys_toggle_move_and_select_all_detected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let detected = [ClientKind::Claude, ClientKind::Codex];
        let mut rows = build_rows(dir.path(), dir.path(), &detected);
        let keys = ["k", " ", "", "ignored", ""].map(str::to_string);
        let mut frames = Vec::new();
        let outcome = run_menu(&mut rows, keys, |frame| frames.push(frame.to_string()));
        assert_eq!(outcome, MenuOutcome::Confirmed);
        assert_eq!(selected_rows(&rows).len(), 1, "{frames:?}");
        assert_eq!(selected_rows(&rows)[0].name, "codex");
        assert_eq!(parse_key("\x1b[A"), MenuAction::Up);
        assert_eq!(parse_key("\x1b[B"), MenuAction::Down);
        assert_eq!(parse_key("\x1b"), MenuAction::Cancel);
        assert_eq!(parse_key("a"), MenuAction::AllDetected);

        let mut undetected = build_rows(dir.path(), dir.path(), &[]);
        let outcome = run_menu(&mut undetected, ["a", ""].map(str::to_string), |_frame| {});
        assert_eq!(outcome, MenuOutcome::Confirmed);
        assert!(
            selected_rows(&undetected).is_empty(),
            "`a` cannot invent a detected host"
        );

        let mut cancelled = build_rows(dir.path(), dir.path(), &detected);
        let outcome = run_menu(&mut cancelled, ["\x1b"].map(str::to_string), |_frame| {});
        assert_eq!(outcome, MenuOutcome::Cancelled);
    }

    #[test]
    fn selection_applies_only_chosen_hosts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        fake_hosts(dir.path());
        let ctx = test_ctx(dir.path());
        let detected = clients::detect(dir.path());
        assert!(detected.contains(&ClientKind::Claude));
        assert!(detected.contains(&ClientKind::Codex));

        let mut rows = build_rows(dir.path(), &ctx.root, &detected);
        for row in &mut rows {
            row.selected = row.name == "claude";
        }
        let before =
            std::fs::read(dir.path().join(".codex").join("config.toml")).expect("read codex");
        let report = apply_selected(&ctx, &selected_rows(&rows), false).expect("apply");

        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].client, "claude");
        assert!(
            ["registered", "needs restart", "not configured"].contains(&report.verdict.as_str())
        );
        assert!(report.summary().contains("restart claude to load tools"));
        let claude = std::fs::read_to_string(dir.path().join(".claude.json")).expect("read claude");
        assert!(
            claude.contains("memnoth"),
            "verdict={} detail={} config={claude}",
            report.verdict,
            report.results[0].detail
        );
        assert_eq!(
            std::fs::read(dir.path().join(".codex").join("config.toml")).expect("read codex"),
            before,
            "an unchosen host must not be written"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        fake_hosts(dir.path());
        let ctx = test_ctx(dir.path());
        let detected = clients::detect(dir.path());
        let rows = build_rows(dir.path(), &ctx.root, &detected);
        let plan = plan_text(&rows, &ctx, true);
        assert!(plan.contains("dry run"), "{plan}");
        assert!(plan.contains("mcp serve --db"), "{plan}");
        assert!(plan.contains("MEMNOTH_MCP_READ_ONLY=1"), "{plan}");
        assert!(plan.contains("restart claude to load tools"), "{plan}");

        let before = std::fs::read(dir.path().join(".claude.json")).expect("read claude");
        let report = apply_selected(&ctx, &selected_rows(&rows), true).expect("dry run");
        assert!(report.dry_run);
        assert_eq!(report.verdict, "not configured");
        assert_eq!(
            std::fs::read(dir.path().join(".claude.json")).expect("read claude"),
            before
        );
        assert!(!dir.path().join(".claude.json.bak").exists());
        assert!(!dir.path().join("state").join("config.toml").exists());
    }

    #[test]
    fn no_tty_selection_is_detected_only_and_terminates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let detected = [ClientKind::Claude, ClientKind::Codex];
        let rows = build_rows(dir.path(), dir.path(), &detected);
        let names: Vec<String> = non_interactive_selection(&rows)
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(names, ["claude", "codex"]);
        // A second call must return the same answer without consuming stdin.
        assert_eq!(
            non_interactive_selection(&rows)
                .into_iter()
                .map(|row| row.name)
                .collect::<Vec<_>>(),
            names
        );
    }

    #[test]
    fn redaction_strips_bearer_sk_and_jwt() {
        let line = "claude failed: Authorization: Bearer sk-abcdefghijklmnop declined";
        let redacted = redact(line);
        assert!(!redacted.contains("sk-abcdefghijklmnop"), "{redacted}");
        assert!(!redacted.contains("Bearer"), "{redacted}");
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefghijklmnop";
        assert!(!redact(&format!("token={jwt}")).contains(jwt));
        assert!(!redact("MEMNOTH_MCP_TOKEN=sk-secretvalue1234").contains("secretvalue"));
        assert!(redact("plain error: no config file").contains("no config file"));
    }

    #[test]
    fn setup_run_with_wizard_dry_run_writes_nothing_and_reports_a_verdict() {
        if stdin_is_terminal() {
            // A person's interactive terminal cannot be driven from a test.
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = EnvGuard::set(dir.path());
        fake_hosts(dir.path());
        let ctx = test_ctx(dir.path());
        let args = crate::cli::SetupArgs {
            skip: [
                "vault",
                "graph",
                "auth",
                "completions",
                "provider",
                "config",
                "sandbox",
            ]
            .map(str::to_string)
            .to_vec(),
            yes: true,
            json: true,
            wizard: true,
            dry_run: true,
        };
        let before = std::fs::read(dir.path().join(".claude.json")).expect("read claude");
        let outcome = crate::setup::run(&ctx, &args).expect("run");
        assert_eq!(outcome.data["wizard"]["dry_run"], true);
        assert_eq!(outcome.data["wizard"]["verdict"], "not configured");
        assert!(
            outcome.text.contains("restart claude to load tools"),
            "{}",
            outcome.text
        );
        assert_eq!(
            std::fs::read(dir.path().join(".claude.json")).expect("read claude"),
            before
        );
    }

    #[test]
    fn backup_never_clobbers_an_older_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, "first").expect("write");
        assert_eq!(
            backup_if_exists(&path).expect("backup").unwrap(),
            dir.path().join("mcp.json.bak")
        );
        std::fs::write(&path, "second").expect("rewrite");
        assert_eq!(
            backup_if_exists(&path).expect("backup").unwrap(),
            dir.path().join("mcp.json.bak.1")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mcp.json.bak")).expect("read"),
            "first"
        );
    }
}

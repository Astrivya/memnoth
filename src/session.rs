//! Named sessions on disk: the conversation that outlives a terminal.
//!
//! A session is a name and a JSONL file under `~/.memnoth/sessions/`. The
//! format belongs to `memnoth-session`; this module owns only the naming rule
//! and the directory, because a name typed by a person reaches a filename and
//! therefore has to be validated rather than trusted.
//!
//! `--resume payments` loads `payments.jsonl` if it exists and creates it
//! otherwise. Resuming carries decisions and corrections forward because the
//! graph is the memory and the session is only the conversation that reads it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use memnoth_session::{Role, Session};

/// The label recorded as the session's model.
///
/// No provider is wired in this build, so the label states that rather than
/// naming a model that never ran. `memnoth session` is still the transcript the
/// vault answers into, and the field is what a future runtime will overwrite.
pub const SESSION_MODEL: &str = "memnoth-local";

/// Longest accepted session name. A name becomes a filename, so it is capped
/// well inside every filesystem's limit.
const MAX_NAME: usize = 64;

/// The session directory, and the naming rule for what lives in it.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

/// One row of the session list.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    /// The name the session is resumed by.
    pub name: String,
    /// Recorded model label.
    pub model: String,
    /// Created at (ms).
    pub created_ms: i64,
    /// Last activity (the file's mtime, ms).
    ///
    /// Created-at is when a conversation was born; this is when it was last
    /// used, which is the question a picker is actually asked. `0` when the
    /// file cannot be stat-ed.
    pub modified_ms: i64,
    /// Messages on disk.
    pub messages: usize,
    /// The first thing the person asked, flattened and shortened.
    ///
    /// A name is a filename, so it cannot be "fix the login redirect" without
    /// becoming one. The first prompt is the title a person recognises, and it
    /// is derived rather than stored: every session already on disk has one,
    /// including the ones written before this field existed.
    pub title: String,
    /// Where the file is.
    pub path: String,
}

impl SessionStore {
    /// Point the store at a directory (created on first save).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory sessions live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file a name maps to.
    pub fn path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.dir.join(format!("{}.jsonl", validate_name(name)?)))
    }

    /// Where the command history lives.
    ///
    /// It sits beside the sessions because it is read at the moment a session
    /// starts: the dropdown's ranking is the first thing on screen, and a
    /// second directory to look in would be one more thing to get wrong.
    /// [`crate::tui::slash::load_usage`] treats a missing file as no history.
    pub fn usage_path(&self) -> PathBuf {
        self.dir.join("commands.json")
    }

    /// Load `name`, or create it when it does not exist yet.
    ///
    /// The boolean says whether it was resumed, so the session header and the
    /// TUI can say "resumed payments" instead of pretending a fresh session is
    /// a continuation of one.
    pub fn load_or_create(&self, name: &str) -> Result<(Session, bool)> {
        let path = self.path(name)?;
        if path.exists() {
            let mut session = Session::load(&path)
                .with_context(|| format!("cannot resume {}", path.display()))?;
            // The file name is the identity a user types, so it wins over the
            // header id: renaming the file renames the session.
            session.id = validate_name(name)?.to_string();
            return Ok((session, true));
        }
        let mut session = Session::new(SESSION_MODEL);
        session.id = validate_name(name)?.to_string();
        Ok((session, false))
    }

    /// Persist a session. Write-then-rename is `Session::save`'s job.
    pub fn save(&self, session: &Session) -> Result<PathBuf> {
        session
            .save(&self.dir)
            .with_context(|| format!("cannot write the session under {}", self.dir.display()))
    }

    /// The newest `limit` conversations, newest first.
    ///
    /// `list` parses every file because the picker draws them all. The home
    /// screen's activity strip shows three, so this sorts by the file's mtime
    /// first — a stat per session instead of a parse per session — and reads
    /// only the files it is about to name.
    pub fn recent(&self, limit: usize) -> Vec<SessionSummary> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<(String, PathBuf, i64)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let modified_ms = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or(0);
            files.push((name.to_string(), path, modified_ms));
        }
        files.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        files.truncate(limit);
        files
            .into_iter()
            .map(|(name, path, modified_ms)| {
                let (model, created_ms, messages, title) = match Session::load(&path) {
                    Ok(session) => {
                        let title = title_of(&session);
                        (
                            session.model,
                            session.created_ms,
                            session.messages.len(),
                            title,
                        )
                    }
                    Err(_) => (String::new(), 0, 0, String::new()),
                };
                SessionSummary {
                    name,
                    model,
                    created_ms,
                    modified_ms,
                    messages,
                    title,
                    path: path.display().to_string(),
                }
            })
            .collect()
    }

    /// Every session on disk, newest first.
    ///
    /// A file that cannot be parsed is listed with `messages: 0` rather than
    /// skipped: a corrupted session the user can see beats one that vanished
    /// from the picker without a word.
    pub fn list(&self) -> Vec<SessionSummary> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let (model, created_ms, messages, title) = match Session::load(&path) {
                Ok(session) => {
                    let title = title_of(&session);
                    (
                        session.model,
                        session.created_ms,
                        session.messages.len(),
                        title,
                    )
                }
                Err(_) => (String::new(), 0, 0, String::new()),
            };
            let modified_ms = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or(created_ms);
            out.push(SessionSummary {
                name: name.to_string(),
                model,
                created_ms,
                modified_ms,
                messages,
                title,
                path: path.display().to_string(),
            });
        }
        // Most recently used first: a picker is the way back to what you were
        // doing, not an archive sorted by birthday.
        out.sort_by(|a, b| {
            b.modified_ms
                .cmp(&a.modified_ms)
                .then(b.created_ms.cmp(&a.created_ms))
                .then(a.name.cmp(&b.name))
        });
        out
    }

    /// A session that does not exist yet, under a free name.
    ///
    /// `/new` cannot reuse the default name: `session` is a real file that may
    /// already hold a conversation, and opening it would continue that one —
    /// the opposite of what was asked. The first free `chat-N` is readable,
    /// typeable as `--resume chat-2`, and needs no clock to stay unique.
    pub fn next_name(&self) -> String {
        for index in 1..=9999 {
            let candidate = format!("chat-{index}");
            let taken = self
                .path(&candidate)
                .map(|path| path.exists())
                .unwrap_or(true);
            if !taken {
                return candidate;
            }
        }
        "chat".to_string()
    }

    /// A fresh session under `name`, refusing to reuse one that exists.
    pub fn create(&self, name: &str) -> Result<Session> {
        let name = validate_name(name)?;
        let path = self.path(&name)?;
        if path.exists() {
            bail!("session \"{name}\" already exists — open it, or pick another name");
        }
        let mut session = Session::new(SESSION_MODEL);
        session.id = name;
        Ok(session)
    }

    /// Load a session that must already exist.
    pub fn load(&self, name: &str) -> Result<Session> {
        let name = validate_name(name)?;
        let path = self.path(&name)?;
        if !path.exists() {
            bail!("no session named \"{name}\"");
        }
        let mut session =
            Session::load(&path).with_context(|| format!("cannot read {}", path.display()))?;
        // The file name is the identity a person types, so it wins over the
        // header id — the same rule `load_or_create` applies. Without it, a
        // session renamed on disk would be saved back under its old name.
        session.id = name;
        Ok(session)
    }

    /// Move a session to a new name, refusing to overwrite another one.
    ///
    /// The name is the identity here, so renaming is the only way to give a
    /// conversation a handle a person can remember — which is exactly what the
    /// picker's rename key exists for.
    pub fn rename(&self, from: &str, to: &str) -> Result<String> {
        let from = validate_name(from)?;
        let to = validate_name(to)?;
        if from == to {
            return Ok(to);
        }
        let source = self.path(&from)?;
        let target = self.path(&to)?;
        if !source.exists() {
            bail!("no session named \"{from}\"");
        }
        if target.exists() {
            bail!("session \"{to}\" already exists");
        }
        std::fs::rename(&source, &target)
            .with_context(|| format!("cannot rename {from} to {to}"))?;
        Ok(to)
    }

    /// Delete a session file.
    ///
    /// The name is validated first, so a stray `..` can never become a path
    /// outside the sessions directory.
    pub fn delete(&self, name: &str) -> Result<()> {
        let name = validate_name(name)?;
        let path = self.path(&name)?;
        if !path.exists() {
            bail!("no session named \"{name}\"");
        }
        std::fs::remove_file(&path).with_context(|| format!("cannot delete {}", path.display()))
    }

    /// Append one exchange and write the file.
    ///
    /// A prompt with no answer is still written: a session that crashed mid-turn
    /// should show the question that was asked, which is the half a person can
    /// act on.
    pub fn record(&self, session: &mut Session, prompt: &str, answer: &str) -> Result<PathBuf> {
        session.push(Role::User, prompt);
        if !answer.trim().is_empty() {
            session.push(Role::Assistant, answer);
        }
        self.save(session)
    }
}

/// The first question asked, flattened to one line and shortened.
///
/// Empty for a session with no user turn yet, which the picker reads as "name
/// it, or ask something": a title invented from nothing would be a lie in the
/// one column people scan.
pub(crate) fn title_of(session: &Session) -> String {
    let Some(first) = session
        .messages
        .iter()
        .find(|message| matches!(message.role, Role::User))
    else {
        return String::new();
    };
    let flat = first
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::with_capacity(flat.len().min(80));
    for (index, ch) in flat.chars().enumerate() {
        if index >= 72 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// A session name as a safe filename.
///
/// Refuses empty, `.`/`..`, path separators and control characters; replaces
/// anything else exotic with `-` so a pasted title still works. The result is
/// what `--resume` accepts, which is why it is also what the banner prints.
pub fn validate_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("session name is empty");
    }
    if trimmed == "." || trimmed == ".." {
        bail!("session name must not be a path segment");
    }
    if trimmed.chars().count() > MAX_NAME {
        bail!("session name is longer than {MAX_NAME} characters");
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        bail!("session name must not contain a path separator");
    }
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        } else if ch.is_control() {
            // Checked before whitespace: `\n` and `\t` are both, and a control
            // character in a filename is refused rather than replaced.
            bail!("session name contains a control character");
        } else if ch.is_whitespace() || ch == ':' {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() {
        bail!("session name has no usable characters");
    }
    Ok(out)
}

/// The transcript as JSON, for `--json` and for the TUI's export.
pub fn transcript(session: &Session) -> Value {
    json!({
        "name": session.id,
        "model": session.model,
        "created_ms": session.created_ms,
        "messages": session.messages.iter().map(|message| json!({
            "role": match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
                Role::Tool => "tool",
            },
            "content": message.content,
            "at_ms": message.at_ms,
        })).collect::<Vec<_>>(),
        "count": session.messages.len(),
    })
}

/// Milliseconds since the Unix epoch. Time is recorded, never inferred: a
/// receipt that cannot be dated is worse than one that is slightly wrong.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_reach_the_filesystem_only_after_validation() {
        assert_eq!(validate_name("payments").unwrap(), "payments");
        assert_eq!(validate_name("team one").unwrap(), "team-one");
        for bad in ["", "  ", ".", "..", "a/b", "a\\b", "a\nb"] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(validate_name(&"x".repeat(MAX_NAME + 1)).is_err());
    }

    #[test]
    fn a_session_is_created_then_resumed_with_the_name_it_was_given() {
        let dir = std::env::temp_dir().join(format!("memnoth-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SessionStore::new(&dir);

        let (mut session, resumed) = store.load_or_create("payments").unwrap();
        assert!(!resumed);
        store
            .record(&mut session, "what changed?", "a fact")
            .unwrap();

        let (loaded, resumed) = store.load_or_create("payments").unwrap();
        assert!(resumed);
        assert_eq!(loaded.id, "payments");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].messages, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_session_stays_visible_in_the_list() {
        let dir = std::env::temp_dir().join(format!("memnoth-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.jsonl"), "not json").unwrap();
        let rows = SessionStore::new(&dir).list();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "broken");
        assert_eq!(rows[0].messages, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

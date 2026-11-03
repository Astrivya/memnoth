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
    /// Messages on disk.
    pub messages: usize,
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
            let (model, created_ms, messages) = match Session::load(&path) {
                Ok(session) => (session.model, session.created_ms, session.messages.len()),
                Err(_) => (String::new(), 0, 0),
            };
            out.push(SessionSummary {
                name: name.to_string(),
                model,
                created_ms,
                messages,
                path: path.display().to_string(),
            });
        }
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then(a.name.cmp(&b.name)));
        out
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

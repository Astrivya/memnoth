//! Checkpoints: a snapshot of one session on disk, and a cursor over them.
//!
//! A checkpoint is not a promise held in memory: it is a copy of the session's
//! JSONL taken at a known point, plus the commit the working tree was on, so
//! `/undo` can say what it went back to. `/undo` and `/redo` move a cursor over
//! that list and restore the chosen snapshot into a *separate* session file
//! (`<session>-rewind.jsonl`). Restoring over the live session would fight the
//! transcript the process is already appending to, so the live conversation is
//! left alone and the rewound one is opened by name.
//!
//! Everything here is on disk under `$MEMNOTH_HOME/checkpoints/<session>/`:
//! `<seq>.jsonl` (the snapshot), `<seq>.json` (its metadata), `cursor.json`
//! (the sequence `/undo` would go back to before it ran).

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use memnoth_session::Session;

use crate::session::{SessionStore, now_ms};

/// One snapshot of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// 1-based position in the session's checkpoint list.
    pub seq: usize,
    /// The label the person gave it, or `checkpoint`.
    pub name: String,
    /// When it was taken (ms since the epoch).
    pub created_ms: i64,
    /// Messages in the conversation at this point.
    pub messages: usize,
    /// The snapshot file this row points at.
    pub snapshot: String,
    /// `git rev-parse HEAD` in the workspace at the time, when it was a repo.
    pub git_head: Option<String>,
}

/// The checkpoint directory for every session on this machine.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    dir: PathBuf,
}

impl CheckpointStore {
    /// Point the store at `$MEMNOTH_HOME/checkpoints`.
    pub fn new(home: &Path) -> Self {
        Self {
            dir: home.join("checkpoints"),
        }
    }

    /// The directory this store owns.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn session_dir(&self, session: &str) -> PathBuf {
        self.dir.join(session)
    }

    /// Snapshot `session` from `sessions_dir` under `label`.
    ///
    /// The conversation must already exist on disk: a checkpoint of a session
    /// nothing has written is an empty file, which restores to nothing.
    pub fn create(
        &self,
        sessions_dir: &Path,
        session: &str,
        label: &str,
        root: Option<&Path>,
    ) -> Result<Checkpoint> {
        let source = sessions_dir.join(format!("{session}.jsonl"));
        if !source.is_file() {
            bail!(
                "no session file at {} — /checkpoint snapshots a conversation that is on disk",
                source.display()
            );
        }
        let loaded = Session::load(&source)
            .with_context(|| format!("cannot read session {}", source.display()))?;

        let dir = self.session_dir(session);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;

        let seq = self
            .list(session)
            .unwrap_or_default()
            .last()
            .map(|row| row.seq + 1)
            .unwrap_or(1);
        let snapshot = dir.join(format!("{seq}.jsonl"));
        std::fs::copy(&source, &snapshot).with_context(|| {
            format!("cannot copy {} to {}", source.display(), snapshot.display())
        })?;

        let checkpoint = Checkpoint {
            seq,
            name: label.trim().chars().take(80).collect::<String>(),
            created_ms: now_ms(),
            messages: loaded.messages.len(),
            snapshot: snapshot.display().to_string(),
            git_head: git_head(root),
        };
        std::fs::write(
            dir.join(format!("{seq}.json")),
            serde_json::to_string_pretty(&checkpoint)?,
        )?;
        self.set_cursor(session, seq)?;
        Ok(checkpoint)
    }

    /// Every checkpoint for `session`, oldest first.
    pub fn list(&self, session: &str) -> Result<Vec<Checkpoint>> {
        let dir = self.session_dir(session);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // `cursor.json` is the position, not a checkpoint.
            if path.file_name().and_then(|n| n.to_str()) == Some("cursor.json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            if let Ok(row) = serde_json::from_str::<Checkpoint>(&text) {
                rows.push(row);
            }
        }
        rows.sort_by_key(|row| row.seq);
        Ok(rows)
    }

    /// The sequence the last `/checkpoint` or `/undo` left the cursor on.
    pub fn cursor(&self, session: &str) -> Option<usize> {
        let text = std::fs::read_to_string(self.session_dir(session).join("cursor.json")).ok()?;
        serde_json::from_str::<Cursor>(&text)
            .ok()
            .map(|row| row.seq)
    }

    /// Move the cursor to `seq`.
    pub fn set_cursor(&self, session: &str, seq: usize) -> Result<()> {
        let dir = self.session_dir(session);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("cursor.json"),
            serde_json::to_string(&Cursor { seq })?,
        )?;
        Ok(())
    }

    /// Write `checkpoint`'s snapshot into `<session>-rewind.jsonl`.
    ///
    /// The live session file is never touched: a process appending to it would
    /// overwrite the restore on its next turn.
    pub fn restore(
        &self,
        sessions_dir: &Path,
        session: &str,
        checkpoint: &Checkpoint,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(sessions_dir)
            .with_context(|| format!("cannot create {}", sessions_dir.display()))?;
        let target = sessions_dir.join(format!("{session}-rewind.jsonl"));
        std::fs::copy(&checkpoint.snapshot, &target).with_context(|| {
            format!(
                "cannot restore {} to {}",
                checkpoint.snapshot,
                target.display()
            )
        })?;
        Ok(target)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Cursor {
    seq: usize,
}

fn git_head(root: Option<&Path>) -> Option<String> {
    let root = root?;
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!head.is_empty()).then_some(head)
}

/// The session most recently written to in `sessions_dir`.
///
/// A slash command is typed at a prompt that already belongs to a session, but
/// the handler only sees the context. The newest file is that session: the TUI
/// writes it every turn, and the name is reported back so a wrong guess is
/// visible rather than silent.
pub fn latest_session(sessions_dir: &Path) -> Result<String> {
    let mut rows = SessionStore::new(sessions_dir).list();
    rows.sort_by_key(|row| Reverse(row.modified_ms));
    rows.first().map(|row| row.name.clone()).ok_or_else(|| {
        anyhow::anyhow!(
            "no conversations in {} yet — say something first, then /checkpoint",
            sessions_dir.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_session::Role;

    fn write_session(dir: &Path, name: &str, turns: usize) -> PathBuf {
        let mut session = Session::new("memnoth-local");
        session.id = name.to_string();
        for index in 0..turns {
            session.push(Role::User, format!("question {index}"));
            session.push(Role::Assistant, format!("answer {index}"));
        }
        session.save(dir).expect("save session")
    }

    #[test]
    fn a_checkpoint_copies_the_conversation_and_remembers_the_commit() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        write_session(sessions.path(), "payments", 2);

        let store = CheckpointStore::new(home.path());
        let checkpoint = store
            .create(sessions.path(), "payments", "before the rename", None)
            .unwrap();

        assert_eq!(checkpoint.seq, 1);
        assert_eq!(checkpoint.messages, 4);
        assert_eq!(checkpoint.name, "before the rename");
        assert!(Path::new(&checkpoint.snapshot).is_file());
        assert_eq!(store.cursor("payments"), Some(1));
    }

    #[test]
    fn checkpoints_are_numbered_and_listed_in_order() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        write_session(sessions.path(), "payments", 1);

        let store = CheckpointStore::new(home.path());
        store
            .create(sessions.path(), "payments", "one", None)
            .unwrap();
        store
            .create(sessions.path(), "payments", "two", None)
            .unwrap();

        let rows = store.list("payments").unwrap();
        let seqs: Vec<usize> = rows.iter().map(|row| row.seq).collect();
        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(store.cursor("payments"), Some(2));
    }

    #[test]
    fn a_missing_session_is_refused_by_name() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(home.path());
        let error = store
            .create(sessions.path(), "ghost", "x", None)
            .unwrap_err();
        assert!(error.to_string().contains("no session file"), "{error}");
    }

    #[test]
    fn restoring_writes_a_second_session_and_leaves_the_live_one_alone() {
        let home = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        write_session(sessions.path(), "payments", 1);
        let store = CheckpointStore::new(home.path());
        let checkpoint = store
            .create(sessions.path(), "payments", "one", None)
            .unwrap();

        // The conversation moves on after the checkpoint.
        write_session(sessions.path(), "payments", 4);
        let live_before = std::fs::read_to_string(sessions.path().join("payments.jsonl")).unwrap();

        let restored = store
            .restore(sessions.path(), "payments", &checkpoint)
            .unwrap();
        assert_eq!(restored.file_name().unwrap(), "payments-rewind.jsonl");

        let rewound = Session::load(&restored).unwrap();
        assert_eq!(rewound.messages.len(), 2, "the snapshot's messages");
        let live_after = std::fs::read_to_string(sessions.path().join("payments.jsonl")).unwrap();
        assert_eq!(live_before, live_after, "the live session is untouched");
    }

    #[test]
    fn latest_session_is_the_newest_file() {
        let sessions = tempfile::tempdir().unwrap();
        write_session(sessions.path(), "older", 1);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_session(sessions.path(), "newer", 1);
        assert_eq!(latest_session(sessions.path()).unwrap(), "newer");
    }
}

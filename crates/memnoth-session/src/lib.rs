//! Turn log and persistence — the session file format, with no runtime in it.
//!
//! A session is an append-only list of messages written as one JSON object per line under
//! `.memnoth/sessions/<id>.jsonl`. JSONL rather than a database on purpose: a crashed
//! session loses at most the last line, `tail -1` is a debugging tool, and `/resume` needs
//! no migration story.
//!
//! `save` rewrites the whole file. At session sizes (thousands of lines, not millions) that
//! is simpler and safer than seeking, and it makes a torn write detectable on load.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who produced a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// One message in the turn log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Milliseconds since the epoch, so a resumed session can show when each turn happened
    /// without a second file.
    pub at_ms: i64,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            at_ms: now_ms(),
        }
    }
}

/// The on-disk header. A separate line from the messages so a reader can identify a file
/// before parsing anything that could fail.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    pub id: String,
    pub model: String,
    pub created_ms: i64,
}

/// One turn's context receipt, as written to the session log.
///
/// This is a *record*, not the assembler's live receipt: that type borrows a `&'static str`
/// slot per piece and would need its whole shape serialised to be useful on disk. What a user asks
/// later is narrower — how many tokens, did the prefix change, what did the cache serve, what did
/// it cost — so that is what is stored, with the hash rendered as hex so `grep` works on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnReceipt {
    pub at_ms: i64,
    pub total_tokens: usize,
    pub prefix_hash: String,
    /// Per-layer token accounting (Global, Project, Session, Turn). Sums to `total_tokens`.
    pub layer_tokens: [usize; 4],
    pub pieces: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// `None` when the model has no configured price. Never zero-by-default: zero would claim
    /// the turn was free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
    /// The turn index this receipt belongs to (for correlation with writes and checkpoints).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<usize>,
}

/// A checkpoint in the session, marking a point for /rewind and /revert.
///
/// Checkpoints are created at turn boundaries and allow the session to be
/// rewound to a specific turn, or forked from that point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub at_ms: i64,
    /// The turn index this checkpoint corresponds to.
    pub turn_id: usize,
    /// A human-readable label for the checkpoint.
    pub label: String,
}

/// One file edit, with both sides kept.
///
/// A hash of the change is enough to *show* that something happened, and useless the moment a
/// person wants to accept or undo part of it: a diff needs both texts, and a revert needs the old
/// one byte for byte. So the receipt keeps them.
///
/// Two things follow, and both are deliberate:
///
/// * **Both sides are verbatim.** `edit_file` reads a file as text and writes text back, so
///   restoring `before` is exact — including line endings and a missing final newline, which the
///   renderer normalises away and therefore cannot be rebuilt from.
/// * **The body is bounded already.** An edit can only touch a file `edit_file` agreed to read
///   (`read_limit`, 256 KB), so a receipt cannot be arbitrarily large and the log gets no new cap
///   of its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WriteReceipt {
    pub at_ms: i64,
    /// The file the edit was made to, resolved against the workspace when it happened.
    ///
    /// Resolved rather than as typed: a revert has to be able to open the file without being
    /// handed the workspace the turn ran in, and a relative path would need exactly that.
    pub path: String,
    /// The file before the edit.
    pub before: String,
    /// The file after it. `revert` refuses unless the file still holds exactly this, so a Receipt
    /// can never overwrite an edit somebody made later.
    pub after: String,
    /// The turn index this edit belongs to (for correlation with receipts and checkpoints).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<usize>,
}
/// The version this build writes, and the newest it can read.
///
/// 1 → messages only, 2 → `TurnReceipt` rows, 3 → `WriteReceipt` rows. Reading is additive: an
/// older file has fewer row kinds and needs no migration. A *newer* file is refused by name rather
/// than by a parse error halfway through, which is the difference between "upgrade" and "corrupt".
pub const FORMAT_VERSION: u32 = 4;

/// A conversation with the model, plus the two counters the status bar shows.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub model: String,
    pub created_ms: i64,
    pub messages: Vec<Message>,
    /// One entry per completed turn, in order. Older files have none.
    pub receipts: Vec<TurnReceipt>,
    /// One entry per file edit, in the order they happened. Files older than version 3 have none.
    pub writes: Vec<WriteReceipt>,
    /// Checkpoints for /rewind and /revert — one per turn boundary.
    pub checkpoints: Vec<Checkpoint>,
    /// Totals reported by the provider; zero until a transport is wired.
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Session {
    /// A new session id. Millisecond timestamp plus pid: unique enough on one machine, and
    /// sortable by name, which is what the session picker needs.
    pub fn new(model: impl Into<String>) -> Self {
        let created_ms = now_ms();
        Self {
            id: format!("{created_ms}-{}", std::process::id()),
            model: model.into(),
            created_ms,
            messages: Vec::new(),
            receipts: Vec::new(),
            writes: Vec::new(),
            checkpoints: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Record one completed file edit.
    pub fn record_write(&mut self, write: WriteReceipt) {
        self.writes.push(write);
    }

    /// Record one turn's receipt.
    pub fn record_receipt(&mut self, receipt: TurnReceipt) {
        self.receipts.push(receipt);
    }

    /// The most recent receipt, which is what `/context` shows.
    pub fn last_receipt(&self) -> Option<&TurnReceipt> {
        self.receipts.last()
    }

    /// Total known spend, and how many turns it covers. A turn with no price is excluded rather
    /// than counted as free, so `(micros, priced_turns)` cannot silently understate a bill.
    pub fn spend(&self) -> (u64, usize) {
        let mut total = 0;
        let mut priced = 0;
        for receipt in &self.receipts {
            if let Some(micros) = receipt.cost_micros {
                total += micros;
                priced += 1;
            }
        }
        (total, priced)
    }

    /// Append a message. Returns `()` on purpose: an earlier version returned
    /// `&Message` and reached for it with `expect("just pushed")`, which is a
    /// panic path in a recorder whose entire job is to not lose the transcript.
    /// No caller wanted the reference.
    pub fn push(&mut self, role: Role, content: impl Into<String>) {
        self.messages.push(Message::new(role, content));
    }

    pub fn user(&mut self, content: impl Into<String>) {
        self.push(Role::User, content);
    }

    pub fn assistant(&mut self, content: impl Into<String>) {
        self.push(Role::Assistant, content);
    }

    /// Record provider accounting for one turn.
    pub fn account(&mut self, input: u64, output: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
    }

    /// Build a session from `(role, text)` rows, for persistence.
    ///
    /// Two vocabularies exist because they answer different questions: [`Message`] is what is
    /// written to disk (one row per line, tolerant of a truncated tail), the wire's chat message
    /// is what the provider sees (tool ids and call lists included). This crate deliberately
    /// knows only the first; the adapter in `memnoth-runtime` owns the conversion, so the format
    /// does not depend on any one provider's wire shape.
    pub fn from_rows<I>(id: impl Into<String>, model: impl Into<String>, rows: I) -> Self
    where
        I: IntoIterator<Item = (Role, String)>,
    {
        let created_ms = now_ms();
        Self {
            id: id.into(),
            model: model.into(),
            created_ms,
            messages: rows
                .into_iter()
                .map(|(role, content)| Message {
                    role,
                    content,
                    at_ms: created_ms,
                })
                .collect(),
            receipts: Vec::new(),
            writes: Vec::new(),
            checkpoints: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// The conversation as `(role, text)` rows, tool rows dropped.
    ///
    /// A file written before the "no tool rows" rule existed can still contain them, and a
    /// resumed session must not put an orphan `tool_result` in front of the model — so they are
    /// filtered on the way out as well as on the way in.
    pub fn rows(&self) -> impl Iterator<Item = (Role, &str)> {
        self.messages
            .iter()
            .filter(|message| message.role != Role::Tool)
            .map(|message| (message.role, message.content.as_str()))
    }

    /// Give each turn the instant it was actually asked.
    ///
    /// [`Session::from_chat`] cannot know when a turn happened — a `ChatMessage` carries no
    /// timestamp — so every row it builds is stamped with the moment of the save. A caller that
    /// *does* know (the UI records the instant at submit) hands the values over here, which is
    /// what lets a rewind anchor a fork to a real point in the vault's history instead of to
    /// "whenever the file was written".
    ///
    /// Missing entries are left alone and extra ones ignored: a resumed session has fewer
    /// known times than the branch it came from, and neither case is an error.
    pub fn stamp_turns(&mut self, at_ms: &[i64]) {
        let mut times = at_ms.iter();
        for message in self.messages.iter_mut() {
            if message.role != Role::User {
                continue;
            }
            match times.next() {
                Some(at) => message.at_ms = *at,
                None => break,
            }
        }
    }

    /// Indices of the messages that started a turn. What `/rewind` lists.
    pub fn turns(&self) -> Vec<usize> {
        self.messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.role == Role::User)
            .map(|(index, _)| index)
            .collect()
    }

    /// Cut the conversation back to just before turn `turn` (0-based), keeping the user's
    /// prompt at that index. Returns whether anything was cut.
    ///
    /// Rewinding to a turn and keeping the prompt is the useful shape: you re-ask the question
    /// differently, you do not delete it and retype it.
    pub fn rewind_to_turn(&mut self, turn: usize) -> bool {
        let Some(start) = self.turns().get(turn).copied() else {
            return false;
        };
        if start + 1 >= self.messages.len() {
            return false;
        }
        self.messages.truncate(start + 1);
        // The ledger follows the conversation: a fork keeps only the turns it keeps, or its
        // spend would bill answers that are no longer in it.
        self.receipts.truncate(turn);
        // Truncate writes and checkpoints that occurred after the rewind point.
        // We keep writes from turns <= turn, and remove writes from turns > turn.
        let write_cutoff = self
            .writes
            .iter()
            .position(|w| w.turn_id.is_some_and(|id| id > turn))
            .unwrap_or(self.writes.len());
        self.writes.truncate(write_cutoff);
        // Checkpoints are one per turn boundary, so we keep checkpoints for turns < turn
        self.checkpoints.retain(|cp| cp.turn_id < turn);
        true
    }

    /// Add a checkpoint at the current turn boundary.
    pub fn add_checkpoint(&mut self, turn_id: usize, label: String) {
        self.checkpoints.push(Checkpoint {
            at_ms: now_ms(),
            turn_id,
            label,
        });
    }

    /// A copy under a new id, so a rewind can branch instead of destroying the original.
    pub fn fork(&self, tag: &str) -> Self {
        Self {
            id: format!("{}-fork-{tag}", self.id),
            created_ms: now_ms(),
            ..self.clone()
        }
    }

    /// Where this session lives under a workspace.
    pub fn path(&self, sessions_dir: &Path) -> PathBuf {
        sessions_dir.join(format!("{}.jsonl", self.id))
    }

    /// Write the session, creating the directory if needed. Returns the path written.
    pub fn save(&self, sessions_dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(sessions_dir)
            .with_context(|| format!("cannot create {}", sessions_dir.display()))?;
        let path = self.path(sessions_dir);
        let mut out = String::new();
        out.push_str(&serde_json::to_string(&Header {
            // 4 added checkpoints. A reader accepts 1, 2, 3: `load` classifies each line by shape,
            // so an older file keeps loading — and a *newer* one is refused by version instead of
            // being mistaken for a corrupt one.
            version: FORMAT_VERSION,
            id: self.id.clone(),
            model: self.model.clone(),
            created_ms: self.created_ms,
        })?);
        out.push('\n');
        for message in &self.messages {
            out.push_str(&serde_json::to_string(message)?);
            out.push('\n');
        }
        for receipt in &self.receipts {
            out.push_str(&serde_json::to_string(receipt)?);
            out.push('\n');
        }
        for write in &self.writes {
            out.push_str(&serde_json::to_string(write)?);
            out.push('\n');
        }
        for checkpoint in &self.checkpoints {
            out.push_str(&serde_json::to_string(checkpoint)?);
            out.push('\n');
        }
        // Write-then-rename: a crash mid-save leaves the previous session intact rather
        // than a half-written file that reads as a short conversation.
        let tmp = path.with_extension("jsonl.tmp");
        {
            let mut file = fs::File::create(&tmp)
                .with_context(|| format!("cannot write {}", tmp.display()))?;
            file.write_all(out.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Read a session back. A file whose header line is missing is an error, not an empty
    /// session: silently returning nothing is how a corrupted log looks like a new one.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header: Header = serde_json::from_str(lines.next().context("session file is empty")?)
            .context("session header is not valid JSON")?;
        // A file this build cannot know the shape of is refused here, before any line is parsed:
        // "upgrade memnoth" is actionable, "malformed message line" is not.
        if header.version > FORMAT_VERSION {
            anyhow::bail!(
                "{} was written by a newer memnoth (format {}); this build reads up to {}",
                path.display(),
                header.version,
                FORMAT_VERSION
            );
        }
        let mut messages = Vec::new();
        let mut receipts: Vec<TurnReceipt> = Vec::new();
        let mut writes = Vec::new();
        let mut checkpoints = Vec::new();
        for line in lines {
            // Four row kinds, told apart by shape rather than by a `kind` field: a message has a
            // `role`, a turn receipt has a `prefix_hash`, a write receipt has both sides, a
            // checkpoint has `turn_id` and `label`. That is what keeps files written before either
            // receipt existed readable without a migration.
            let value: Value = serde_json::from_str(line)
                .with_context(|| format!("malformed line in {}", path.display()))?;
            if value.get("before").is_some() && value.get("after").is_some() {
                writes.push(serde_json::from_value(value).with_context(|| {
                    format!("malformed write receipt line in {}", path.display())
                })?);
            } else if value.get("prefix_hash").is_some() && value.get("role").is_none() {
                receipts
                    .push(serde_json::from_value(value).with_context(|| {
                        format!("malformed receipt line in {}", path.display())
                    })?);
            } else if value.get("turn_id").is_some() && value.get("label").is_some() {
                checkpoints.push(
                    serde_json::from_value(value).with_context(|| {
                        format!("malformed checkpoint line in {}", path.display())
                    })?,
                );
            } else {
                messages
                    .push(serde_json::from_value(value).with_context(|| {
                        format!("malformed message line in {}", path.display())
                    })?);
            }
        }
        // The totals are the receipts' own numbers, not extra state: a resumed
        // session has to show the same spend it showed before it was closed,
        // and a file from before receipts existed simply has none.
        let input_tokens = receipts.iter().map(|receipt| receipt.input_tokens).sum();
        let output_tokens = receipts.iter().map(|receipt| receipt.output_tokens).sum();
        Ok(Self {
            id: header.id,
            model: header.model,
            created_ms: header.created_ms,
            messages,
            receipts,
            writes,
            checkpoints,
            input_tokens,
            output_tokens,
        })
    }
}

/// Milliseconds since the epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receipt_round_trips_and_is_priced() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new("m");
        session.user("hi");
        session.record_receipt(TurnReceipt {
            at_ms: 1_000,
            total_tokens: 120,
            prefix_hash: "7e1ce76298c3ac7e".into(),
            layer_tokens: [10, 20, 30, 60],
            pieces: 3,
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 80,
            cache_write_tokens: 5,
            cost_micros: Some(42),
            turn_id: None,
        });
        let path = session.save(dir.path()).unwrap();
        let back = Session::load(&path).unwrap();
        assert_eq!(back.receipts, session.receipts);
        assert_eq!(
            back.last_receipt().map(|r| r.prefix_hash.as_str()),
            Some("7e1ce76298c3ac7e")
        );
        assert_eq!(back.spend(), (42, 1));
        assert_eq!(
            (back.input_tokens, back.output_tokens),
            (100, 20),
            "a resumed session keeps the token totals its receipts recorded"
        );
    }

    fn write(path: &str, before: &str, after: &str) -> WriteReceipt {
        WriteReceipt {
            at_ms: 1_000,
            path: path.into(),
            before: before.into(),
            after: after.into(),
            turn_id: None,
        }
    }

    #[test]
    fn a_write_receipt_round_trips_with_both_sides_exact() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new("m");
        session.user("edit it");
        // CRLF and no final newline: the two things a renderer normalises away, and therefore the
        // two things a revert rebuilt from rendered lines would lose.
        let receipt = write("src/a.rs", "one\r\ntwo", "one\r\n2\r\nthree");
        session.record_write(receipt.clone());
        let path = session.save(dir.path()).unwrap();
        let back = Session::load(&path).unwrap();
        assert_eq!(back.writes, vec![receipt]);
        assert_eq!(back.writes[0].before, "one\r\ntwo");
        assert_eq!(back.writes[0].after, "one\r\n2\r\nthree");
    }

    #[test]
    fn a_file_written_before_writes_existed_loads_with_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.jsonl");
        std::fs::write(
            &path,
            "{\"version\":2,\"id\":\"old\",\"model\":\"m\",\"created_ms\":1}\n\
             {\"role\":\"user\",\"content\":\"hi\",\"at_ms\":1}\n\
             {\"at_ms\":2,\"total_tokens\":10,\"prefix_hash\":\"0\",\"layer_tokens\":[0,0,0,10],\
             \"pieces\":1,\"input_tokens\":5,\"output_tokens\":5,\"cache_read_tokens\":0,\
             \"cache_write_tokens\":0}\n",
        )
        .unwrap();
        let session = Session::load(&path).unwrap();
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.receipts.len(), 1, "v2 rows still classify");
        assert!(session.writes.is_empty());
    }

    #[test]
    fn a_newer_format_is_refused_by_name_not_as_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.jsonl");
        std::fs::write(
            &path,
            "{\"version\":5,\"id\":\"f\",\"model\":\"m\",\"created_ms\":1}\n",
        )
        .unwrap();
        let error = format!("{:#}", Session::load(&path).unwrap_err());
        assert!(error.contains("newer memnoth"), "{error}");
        assert!(error.contains("format 5"), "{error}");
    }

    #[test]
    fn an_unpriced_receipt_is_not_counted_as_free() {
        let mut session = Session::new("m");
        session.record_receipt(TurnReceipt {
            at_ms: 1,
            total_tokens: 10,
            prefix_hash: "0".into(),
            layer_tokens: [0, 0, 0, 10],
            pieces: 1,
            input_tokens: 5,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_micros: None,
            turn_id: None,
        });
        assert_eq!(session.spend(), (0, 0));
    }

    #[test]
    fn a_file_written_before_receipts_existed_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.jsonl");
        std::fs::write(
            &path,
            "{\"version\":1,\"id\":\"old\",\"model\":\"m\",\"created_ms\":1}\n\
             {\"role\":\"user\",\"content\":\"hi\",\"at_ms\":1}\n",
        )
        .unwrap();
        let session = Session::load(&path).unwrap();
        assert_eq!(session.messages.len(), 1);
        assert!(session.receipts.is_empty());
        assert!(session.last_receipt().is_none());
    }

    #[test]
    fn a_session_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("claude-sonnet-4");
        s.user("why is refresh failing");
        s.assistant("because the lease is cached");
        s.account(120, 34);
        let path = s.save(dir.path()).unwrap();
        assert!(path.exists());

        let back = Session::load(&path).unwrap();
        assert_eq!(back.id, s.id);
        assert_eq!(back.model, "claude-sonnet-4");
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[0].role, Role::User);
        assert_eq!(back.messages[1].content, "because the lease is cached");
    }

    #[test]
    fn saving_twice_replaces_the_file_instead_of_appending_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("m");
        s.user("one");
        let path = s.save(dir.path()).unwrap();
        s.user("two");
        s.save(dir.path()).unwrap();
        let back = Session::load(&path).unwrap();
        assert_eq!(back.messages.len(), 2);
    }

    #[test]
    fn an_empty_file_is_an_error_not_an_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        fs::write(&path, "").unwrap();
        let err = Session::load(&path).unwrap_err();
        assert!(format!("{err:#}").contains("empty"), "{err:#}");
    }

    #[test]
    fn ids_sort_by_creation() {
        let a = Session::new("m");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = Session::new("m");
        assert!(b.id > a.id, "{} should sort after {}", b.id, a.id);
    }

    #[test]
    fn stamping_gives_each_turn_the_instant_it_was_asked() {
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");
        s.user("second");
        s.stamp_turns(&[1_000, 2_000]);
        assert_eq!(s.messages[0].at_ms, 1_000);
        assert_eq!(s.messages[2].at_ms, 2_000);
        // Fewer times than turns leaves the rest at the save time rather than at zero.
        let mut short = Session::new("m");
        short.user("a");
        short.user("b");
        short.stamp_turns(&[7]);
        assert_eq!(short.messages[0].at_ms, 7);
        assert_ne!(short.messages[1].at_ms, 0);
    }

    #[test]
    fn turns_are_the_user_messages_and_rewind_keeps_the_prompt() {
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");
        s.user("second");
        s.assistant("two");
        assert_eq!(s.turns(), vec![0, 2]);

        assert!(s.rewind_to_turn(1));
        assert_eq!(s.messages.len(), 3, "the second prompt survives");
        assert_eq!(s.messages[2].content, "second");
    }

    fn receipt(at_ms: i64, cost_micros: Option<u64>) -> TurnReceipt {
        TurnReceipt {
            at_ms,
            total_tokens: 10,
            prefix_hash: "0000000000000000".into(),
            layer_tokens: [0, 0, 0, 10],
            pieces: 1,
            input_tokens: 5,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_micros,
            turn_id: None,
        }
    }

    #[test]
    fn a_rewind_drops_the_receipts_after_the_cut() {
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");
        s.user("second");
        s.assistant("two");
        s.record_receipt(receipt(1, Some(10)));
        s.record_receipt(receipt(2, Some(30)));
        assert_eq!(s.spend(), (40, 2));

        assert!(s.rewind_to_turn(1));
        assert_eq!(
            s.receipts.len(),
            1,
            "the fork must not carry the spend of a turn it cut"
        );
        assert_eq!(s.spend(), (10, 1));
    }

    #[test]
    fn a_rewind_to_the_last_turn_has_nothing_to_cut() {
        let mut s = Session::new("m");
        s.user("only");
        s.assistant("answer");
        s.user("pasted again");
        assert!(!s.rewind_to_turn(1), "the prompt is the last row");
        assert!(!s.rewind_to_turn(9), "out of range is false, not a panic");
    }

    #[test]
    fn a_fork_is_a_new_id_over_the_same_conversation() {
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");
        let forked = s.fork("t1");
        assert_ne!(forked.id, s.id);
        assert!(forked.id.ends_with("-fork-t1"));
        assert_eq!(forked.messages, s.messages);
    }

    #[test]
    fn a_rewound_fork_saves_under_its_own_name_and_leaves_the_original_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");
        s.user("second");
        s.assistant("two");
        let original = s.save(dir.path()).unwrap();

        let mut forked = s.fork("t2");
        assert!(forked.rewind_to_turn(1));
        let branch = forked.save(dir.path()).unwrap();

        assert_ne!(original, branch);
        assert_eq!(Session::load(&original).unwrap().messages.len(), 4);
        assert_eq!(Session::load(&branch).unwrap().messages.len(), 3);
    }

    #[test]
    fn checkpoints_are_created_at_turn_boundaries() {
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");

        // Add a checkpoint after turn 0
        s.add_checkpoint(0, "turn 0".to_string());
        assert_eq!(s.checkpoints.len(), 1);
        assert_eq!(s.checkpoints[0].turn_id, 0);
        assert_eq!(s.checkpoints[0].label, "turn 0");

        s.user("second");
        s.assistant("two");

        // Add a checkpoint after turn 1
        s.add_checkpoint(1, "turn 1".to_string());
        assert_eq!(s.checkpoints.len(), 2);
        assert_eq!(s.checkpoints[1].turn_id, 1);
        assert_eq!(s.checkpoints[1].label, "turn 1");
    }

    #[test]
    fn a_write_receipt_includes_turn_id() {
        let mut s = Session::new("m");
        s.user("edit it");

        let receipt = WriteReceipt {
            at_ms: 1_000,
            path: "src/a.rs".into(),
            before: "one\n".into(),
            after: "two\n".into(),
            turn_id: Some(0),
        };
        s.record_write(receipt.clone());

        assert_eq!(s.writes.len(), 1);
        assert_eq!(s.writes[0].turn_id, Some(0));
        assert_eq!(s.writes[0].path, "src/a.rs");
        assert_eq!(s.writes[0].before, "one\n");
        assert_eq!(s.writes[0].after, "two\n");
    }

    #[test]
    fn a_rewind_truncates_writes_and_checkpoints() {
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");
        s.user("second");
        s.assistant("two");

        // Add checkpoint after turn 0
        s.add_checkpoint(0, "turn 0".to_string());

        // Add a write receipt for turn 0
        let write_0 = WriteReceipt {
            at_ms: 1_000,
            path: "src/a.rs".into(),
            before: "one\n".into(),
            after: "two\n".into(),
            turn_id: Some(0),
        };
        s.record_write(write_0);

        // Add a write receipt for turn 1
        let write_1 = WriteReceipt {
            at_ms: 2_000,
            path: "src/b.rs".into(),
            before: "three\n".into(),
            after: "four\n".into(),
            turn_id: Some(1),
        };
        s.record_write(write_1);

        assert_eq!(s.writes.len(), 2);
        assert_eq!(s.checkpoints.len(), 1);

        // Rewind to turn 0 (keep turn 0, cut turn 1)
        assert!(s.rewind_to_turn(0));

        // Should keep only writes from turn 0
        assert_eq!(s.writes.len(), 1);
        assert_eq!(s.writes[0].turn_id, Some(0));

        // Should truncate checkpoints
        assert_eq!(s.checkpoints.len(), 0);
    }

    #[test]
    fn checkpoints_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("m");
        s.user("first");
        s.assistant("one");
        s.add_checkpoint(0, "turn 0".to_string());
        s.user("second");
        s.assistant("two");
        s.add_checkpoint(1, "turn 1".to_string());

        let path = s.save(dir.path()).unwrap();
        let back = Session::load(&path).unwrap();

        assert_eq!(back.checkpoints.len(), 2);
        assert_eq!(back.checkpoints[0].turn_id, 0);
        assert_eq!(back.checkpoints[0].label, "turn 0");
        assert_eq!(back.checkpoints[1].turn_id, 1);
        assert_eq!(back.checkpoints[1].label, "turn 1");
    }

    #[test]
    fn write_receipts_with_turn_id_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new("m");
        s.user("edit it");

        let receipt = WriteReceipt {
            at_ms: 1_000,
            path: "src/a.rs".into(),
            before: "one\n".into(),
            after: "two\n".into(),
            turn_id: Some(0),
        };
        s.record_write(receipt.clone());

        let path = s.save(dir.path()).unwrap();
        let back = Session::load(&path).unwrap();

        assert_eq!(back.writes.len(), 1);
        assert_eq!(back.writes[0].turn_id, Some(0));
        assert_eq!(back.writes[0].path, "src/a.rs");
        assert_eq!(back.writes[0].before, "one\n");
        assert_eq!(back.writes[0].after, "two\n");
    }
}

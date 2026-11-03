//! Durability: what survives a process that dies mid-write.
//!
//! # The gap this closes
//!
//! [`WorkflowJournal`] documents itself as "a crashed coordinator can replay the
//! exact sequence" — but it lives in memory, so a crashed coordinator replays
//! nothing. The same shape of gap applies to a vault: WAL keeps a running
//! database consistent, and does nothing for a barrier whose workers were
//! halfway through when the machine went down.
//!
//! This module persists barrier transitions to an append-only JSONL journal and
//! rebuilds the barrier from it:
//!
//! - **Torn tail tolerated.** A crash truncates the last line mid-write. That
//!   line is dropped and reported (`torn_tail`), because refusing to recover
//!   from exactly the failure the journal exists for would be backwards. A
//!   corrupt line *anywhere else* is an error: a hole in the middle is data
//!   loss, not a crash.
//! - **Sequence continues.** [`DurableJournal::open`] resumes at the last
//!   durable sequence number, so a restart does not overwrite history.
//! - **Barrier rebuild.** [`rebuild`] replays the journal through the public
//!   [`WorkflowJournal`] API, so the recovered barrier is the same object the
//!   live coordinator would have had — and [`pending_after_crash`] names the
//!   workers that were still in flight.
//!
//! [`WorkflowJournal`]: crate::workflow::WorkflowJournal

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::workflow::WorkflowJournal;

/// Worker lifecycle transitions the barrier journals.
pub const REGISTERED: &str = "registered";
/// A worker completed its leg.
pub const CHECKED_IN: &str = "checked_in";
/// A worker failed (still releases the barrier, per the barrier contract).
pub const FAILED: &str = "failed";

/// One durable barrier transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRecord {
    /// 1-based position in the journal.
    pub seq: u64,
    /// When it happened, ms since the epoch (caller-supplied, so tests are
    /// deterministic).
    pub at: i64,
    /// One of [`REGISTERED`], [`CHECKED_IN`], [`FAILED`].
    pub event: String,
    /// Worker the transition belongs to.
    pub worker: String,
}

/// Result of reading a journal off disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayOutcome {
    /// Records that parsed, in order.
    pub records: Vec<JournalRecord>,
    /// `true` when the final line was incomplete — the signature of a crash
    /// during the last append.
    pub torn_tail: bool,
}

/// Append-only barrier journal backed by a file.
///
/// Every append is flushed, so an acknowledged transition is on disk before the
/// coordinator acts on it.
pub struct DurableJournal {
    path: PathBuf,
    file: File,
    seq: u64,
}

impl DurableJournal {
    /// Open (creating if needed) a journal, resuming after any existing records.
    ///
    /// A torn tail from a previous crash is dropped rather than carried forward:
    /// the next appended record takes the sequence number the torn one would
    /// have had.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create journal dir {}", parent.display()))?;
        }
        let previous = replay(path)?;
        // Drop the torn *fragment*, not merely its sequence number. `replay`
        // reports the tear and stops, but the bytes stay on disk and the file is
        // opened in append mode below — so without this the next record is
        // written directly against the fragment and the two together form a line
        // that parses as neither, turning a recoverable crash into an unreadable
        // journal. Rewritten in the same shape `append` writes, so a reopened
        // journal is byte-for-byte indistinguishable from one that never tore.
        if previous.torn_tail {
            let mut repaired = String::new();
            for record in &previous.records {
                repaired.push_str(&serde_json::to_string(record)?);
                repaired.push('\n');
            }
            std::fs::write(path, repaired)
                .with_context(|| format!("repair the torn tail in {}", path.display()))?;
        }
        let seq = previous.records.last().map(|r| r.seq).unwrap_or(0);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open journal {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            seq,
        })
    }

    /// Where this journal writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Highest durable sequence number (`0` before the first record).
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Append one transition and flush it.
    pub fn append(&mut self, event: &str, worker: &str, at: i64) -> Result<JournalRecord> {
        let record = JournalRecord {
            seq: self.seq + 1,
            at,
            event: event.to_string(),
            worker: worker.to_string(),
        };
        writeln!(self.file, "{}", serde_json::to_string(&record)?)?;
        self.file.flush()?;
        self.seq = record.seq;
        Ok(record)
    }

    /// Journal a worker registration.
    pub fn register(&mut self, worker: &str, at: i64) -> Result<JournalRecord> {
        self.append(REGISTERED, worker, at)
    }

    /// Journal a worker check-in.
    pub fn checked_in(&mut self, worker: &str, at: i64) -> Result<JournalRecord> {
        self.append(CHECKED_IN, worker, at)
    }

    /// Journal a worker failure.
    pub fn failed(&mut self, worker: &str, at: i64) -> Result<JournalRecord> {
        self.append(FAILED, worker, at)
    }
}

/// Read a journal from disk.
///
/// The last line may be incomplete (a crash mid-append): it is dropped and
/// flagged. Any other unparsable line is an error, and so is a sequence gap.
pub fn replay(path: &Path) -> Result<ReplayOutcome> {
    let mut out = ReplayOutcome::default();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("read journal {}", path.display())),
    };
    let ends_with_newline = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    let last_index = lines.len().saturating_sub(1);

    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalRecord>(line) {
            Ok(record) => {
                let expected = out
                    .records
                    .last()
                    .map(|r: &JournalRecord| r.seq + 1)
                    .unwrap_or(1);
                if record.seq != expected {
                    anyhow::bail!(
                        "journal {} line {}: expected seq {expected}, found {}",
                        path.display(),
                        i + 1,
                        record.seq
                    );
                }
                out.records.push(record);
            }
            Err(e) => {
                // Only the final line may be damaged, and only as the result of
                // a crash truncating an append.
                if i == last_index && !ends_with_newline {
                    out.torn_tail = true;
                    break;
                }
                anyhow::bail!(
                    "journal {} line {} is corrupt (not a torn tail): {e}",
                    path.display(),
                    i + 1
                );
            }
        }
    }
    Ok(out)
}

/// Rebuild the barrier from a journal: the same object the live coordinator had.
pub fn rebuild(path: &Path) -> Result<(WorkflowJournal, ReplayOutcome)> {
    let outcome = replay(path)?;
    let mut journal = WorkflowJournal::new();
    for record in &outcome.records {
        match record.event.as_str() {
            REGISTERED => journal.register(&record.worker),
            CHECKED_IN => {
                journal.check_in(&record.worker);
            }
            FAILED => {
                journal.mark_failed(&record.worker);
            }
            other => anyhow::bail!("unknown journal event {other:?} at seq {}", record.seq),
        }
    }
    Ok((journal, outcome))
}

/// Workers that were registered and never reported — the ones a recovery sweep
/// has to deal with.
pub fn pending_after_crash(path: &Path) -> Result<Vec<String>> {
    let (journal, _) = rebuild(path)?;
    Ok(journal.pending())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntityKind;
    use crate::namespace::open_namespace;
    use crate::store::VaultOptions;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("memnoth-dur-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_barrier_rebuilds_from_disk_after_the_coordinator_dies() {
        let dir = tmp("rebuild");
        let path = dir.join("barrier.jsonl");
        {
            let mut journal = DurableJournal::open(&path).expect("open");
            journal.register("a", 1).expect("reg");
            journal.register("b", 1).expect("reg");
            journal.checked_in("a", 2).expect("in");
        }
        // Process is gone; a new one recovers the barrier state.
        assert_eq!(
            pending_after_crash(&path).expect("pending"),
            vec!["b".to_string()]
        );
        let mut journal = DurableJournal::open(&path).expect("reopen");
        assert_eq!(
            journal.seq(),
            3,
            "sequence continues, history is not rewritten"
        );
        journal.checked_in("b", 3).expect("in");
        let (barrier, outcome) = rebuild(&path).expect("rebuild");
        assert!(barrier.released());
        assert!(barrier.pending().is_empty());
        assert!(!outcome.torn_tail);
        assert_eq!(outcome.records.len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_tail_is_dropped_and_reported() {
        let dir = tmp("torn");
        let path = dir.join("barrier.jsonl");
        {
            let mut journal = DurableJournal::open(&path).expect("open");
            journal.register("a", 1).expect("reg");
            journal.register("b", 1).expect("reg");
        }
        // Simulate a crash in the middle of the third append.
        {
            let mut f = OpenOptions::new().append(true).open(&path).expect("append");
            write!(f, "{{\"seq\":3,\"at\":2,\"event\":\"chec").expect("partial");
            f.flush().expect("flush");
        }
        let outcome = replay(&path).expect("torn tail is recoverable");
        assert_eq!(outcome.records.len(), 2);
        assert!(outcome.torn_tail);

        let mut journal = DurableJournal::open(&path).expect("reopen");
        assert_eq!(journal.seq(), 2);
        journal.checked_in("a", 5).expect("append after tear");
        let outcome = replay(&path).expect("replay");
        assert!(!outcome.torn_tail);
        assert_eq!(outcome.records.len(), 3);
        assert_eq!(outcome.records[2].seq, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_line_in_the_middle_is_an_error_not_a_silent_skip() {
        let dir = tmp("interior");
        let path = dir.join("barrier.jsonl");
        {
            let mut journal = DurableJournal::open(&path).expect("open");
            journal.register("a", 1).expect("reg");
            journal.checked_in("a", 2).expect("in");
            journal.register("b", 3).expect("reg");
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let mut lines: Vec<&str> = text.lines().collect();
        lines[1] = "{not json}";
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write");
        assert!(replay(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sequence_gap_is_reported_rather_than_papered_over() {
        let dir = tmp("gap");
        let path = dir.join("barrier.jsonl");
        {
            let mut journal = DurableJournal::open(&path).expect("open");
            journal.register("a", 1).expect("reg");
            journal.register("b", 1).expect("reg");
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(&path, format!("{}\n", lines[1])).expect("write");
        assert!(replay(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_writers_share_one_vault_file_without_losing_a_write() {
        let dir = tmp("concurrent");
        let path = dir.join("vault.db");
        let now = 1_000_000_000_000_i64;
        let options = VaultOptions {
            busy_timeout_ms: Some(10_000),
        };
        let per_writer = 40;

        let handles: Vec<_> = (0..2)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let engine = crate::store::MemoryEngine::open_with_options(&path, options)
                        .expect("open");
                    for i in 0..per_writer {
                        engine
                            .add_entity(EntityKind::Concept, &format!("w{w}-e{i}"), 0.9, now + i)
                            .expect("concurrent write");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer thread");
        }

        let engine = crate::store::MemoryEngine::open_with_options(&path, options).expect("open");
        assert_eq!(
            engine.stats().expect("stats").entities_current,
            2 * per_writer,
            "no write lost to a lock"
        );
        assert_eq!(engine.integrity_check().expect("integrity"), "ok");
        assert_eq!(engine.foreign_key_violations().expect("fks"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoint_folds_the_write_ahead_log_so_a_single_file_backup_is_complete() {
        let dir = tmp("checkpoint");
        let engine = open_namespace(&dir, "backup", VaultOptions::default()).expect("open");
        for i in 0..25 {
            engine
                .add_entity(EntityKind::Concept, &format!("e{i}"), 0.9, 1000 + i)
                .expect("write");
        }
        engine.checkpoint().expect("checkpoint");
        let wal = dir.join("vault-backup.db-wal");
        let len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert_eq!(
            len, 0,
            "a truncated WAL means the .db alone is a full backup"
        );
        assert_eq!(engine.integrity_check().expect("integrity"), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! A1 continuity bundle (lane-agentic): task-lensed agent-state briefs compiled
//! from committed room state — never from live mutable reads at prompt time.
//!
//! # Contract
//!
//! - [`Coord::export_checkpoint`] snapshots one session's room state into an
//!   archivable [`Checkpoint`] (JSON). The checkpoint — not shared mutable
//!   state — is what crosses `clear` boundaries between episodes and actors.
//! - [`compile_brief`] renders a [`Checkpoint`] into a compact, scored,
//!   file-grounded text brief. Every line cites an ID (session, task, message,
//!   claim); there are no lossy summaries. Same checkpoint + same config →
//!   byte-identical brief (brief-determinism golden).
//! - Budgets ([`BriefConfig::max_chars`] and per-section caps) are enforced by
//!   the compiler, outside the agent. Truncation is always counted (`…(N
//!   omitted)` markers) — never silent.
//! - [`Coord::guard_write`] is the orphan-guard: a write path is admitted only
//!   when the session holds a live claim covering it, otherwise rejected.
//! - [`Coord::status_line`] is the always-on status: one line, no hydration.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{Coord, MessageRow, SessionStatus, TaskRow};

/// Budget-tracked brief configuration, enforced outside the agent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BriefConfig {
    /// Hard cap on brief length in chars. The compiler truncates whole lines
    /// from the lowest-priority section first and records the omission count.
    pub max_chars: usize,
    /// Max task rows rendered (by score order). The rest are counted omitted.
    pub max_tasks: usize,
    /// Max inbox rows rendered (newest last, by message id).
    pub max_messages: usize,
    /// Max claim globs rendered per session.
    pub max_claims: usize,
}

impl Default for BriefConfig {
    fn default() -> Self {
        Self {
            max_chars: 4000,
            max_tasks: 20,
            max_messages: 10,
            max_claims: 16,
        }
    }
}

/// Archivable per-session checkpoint. `exported_at_ms` is archival metadata
/// only: it is serialized but deliberately excluded from [`compile_brief`] so
/// two exports of identical room state compile to identical briefs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Committed revision the state was compiled from (e.g. git HEAD sha).
    /// Briefs are keyed on this: a dirty tree never gets a brief.
    pub revision: String,
    /// Session this checkpoint was opened for.
    pub session_id: String,
    /// Display name of the session at export time.
    pub session_name: String,
    /// Working directory of the session at export time.
    pub project_root: String,
    /// Export wall-clock (ms since epoch). Archival only, not briefed.
    pub exported_at_ms: i64,
    /// Live claim globs held by this session, sorted.
    pub claims: Vec<String>,
    /// Full task board, sorted by task id (deterministic; the live
    /// `task_list` order is recency-based and therefore not stable).
    pub tasks: Vec<TaskRow>,
    /// This session's inbox (direct + broadcast), ordered by message id.
    pub inbox: Vec<MessageRow>,
    /// Room status, sorted by session id.
    pub room: Vec<SessionStatus>,
}

impl Checkpoint {
    /// Revision-keyed cache key: identical state under the same committed
    /// revision maps to one brief. No wall-clock inside.
    pub fn fingerprint(&self) -> String {
        let mut task_bits: Vec<String> = self
            .tasks
            .iter()
            .map(|t| {
                format!(
                    "{}:{}:{}",
                    t.id,
                    t.status.as_str(),
                    t.claimed_by.as_deref().unwrap_or("-")
                )
            })
            .collect();
        task_bits.sort();
        let inbox_bits: Vec<String> = self.inbox.iter().map(|m| m.id.to_string()).collect();
        format!(
            "{}/{}/claims={}/tasks=[{}]/inbox=[{}]/room={}",
            self.revision,
            self.session_id,
            self.claims.join(","),
            task_bits.join(","),
            inbox_bits.join(","),
            self.room.len()
        )
    }

    /// Serialize to archivable JSON (what crosses `clear` boundaries).
    pub fn export_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Validate and restore an exported checkpoint. The revision must be
    /// non-empty: a checkpoint without a committed revision is refused.
    pub fn import_json(raw: &str) -> Result<Self> {
        let cp: Checkpoint = serde_json::from_str(raw)?;
        if cp.revision.trim().is_empty() {
            bail!("checkpoint refused: empty revision (compile from a committed revision only)");
        }
        if cp.session_id.trim().is_empty() {
            bail!("checkpoint refused: empty session_id");
        }
        Ok(cp)
    }
}

/// Chars/4 token proxy, same estimator the bench uses for brief budgets.
pub fn brief_token_cost(brief: &str) -> usize {
    brief.len() / 4
}

/// True when the brief fits the configured char budget.
pub fn fits_budget(brief: &str, cfg: &BriefConfig) -> bool {
    brief.len() <= cfg.max_chars
}

fn task_score(t: &TaskRow, session_id: &str) -> u8 {
    if t.claimed_by.as_deref() == Some(session_id) {
        0
    } else if t.claimed_by.is_none() {
        1
    } else {
        2
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= max {
        flat
    } else {
        format!("{}…", &flat[..max.min(flat.len())])
    }
}

/// Compile a checkpoint into a deterministic, budget-enforced brief.
///
/// Determinism: every section is emitted in a fixed order over sorted inputs;
/// no timestamps, no map iteration, no recency ordering. Omission is counted
/// per section (`…(N omitted)`) and the global `max_chars` cap truncates
/// whole trailing lines with a final counted marker.
pub fn compile_brief(cp: &Checkpoint, cfg: &BriefConfig) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# continuity-brief rev:{} session:{}",
        cp.revision, cp.session_id
    ));

    let stale = cp.room.iter().filter(|s| s.stale).count();
    let room_claims: usize = cp.room.iter().map(|s| s.active_claims.len()).sum();
    let (p, i, b, d) = cp.tasks.iter().fold((0, 0, 0, 0), |(p, i, b, d), t| {
        use crate::TaskStatus::*;
        match t.status {
            Pending => (p + 1, i, b, d),
            InProgress => (p, i + 1, b, d),
            Blocked => (p, i, b + 1, d),
            Done => (p, i, b, d + 1),
        }
    });
    lines.push(format!(
        "status: sessions={} stale={} claims={} tasks={} pending={} in_progress={} blocked={} done={}",
        cp.room.len(),
        stale,
        room_claims,
        cp.tasks.len(),
        p,
        i,
        b,
        d
    ));

    // Own claims first: the writer's lock footprint, every glob cited.
    let shown_claims = cp.claims.iter().take(cfg.max_claims).count();
    for g in cp.claims.iter().take(cfg.max_claims) {
        lines.push(format!("claim: {g}"));
    }
    if cp.claims.len() > shown_claims {
        lines.push(format!(
            "…({} claims omitted)",
            cp.claims.len() - shown_claims
        ));
    }

    // Tasks, scored: own-claimed > unclaimed > others', ties by task id.
    let mut scored: Vec<&TaskRow> = cp.tasks.iter().collect();
    scored.sort_by(|a, b| {
        task_score(a, &cp.session_id)
            .cmp(&task_score(b, &cp.session_id))
            .then_with(|| a.id.cmp(&b.id))
    });
    let shown_tasks = scored.iter().take(cfg.max_tasks).count();
    for t in scored.iter().take(cfg.max_tasks) {
        lines.push(format!(
            "task [{} {} claimed_by={}] {}",
            t.id,
            t.status.as_str(),
            t.claimed_by.as_deref().unwrap_or("-"),
            one_line(&t.title, 80)
        ));
    }
    if scored.len() > shown_tasks {
        lines.push(format!("…({} tasks omitted)", scored.len() - shown_tasks));
    }

    // Inbox tail, oldest first so the brief reads forward; bodies capped.
    let start = cp.inbox.len().saturating_sub(cfg.max_messages);
    if start > 0 {
        lines.push(format!("…({start} messages omitted)"));
    }
    for m in cp.inbox.iter().skip(start) {
        let to = m.to_session.as_deref().unwrap_or("*");
        lines.push(format!(
            "msg [{} from={} to={}] {}",
            m.id,
            m.from_session,
            to,
            one_line(&m.body, 120)
        ));
    }

    // Room roster, sorted by session id (Checkpoint guarantees the order).
    for s in &cp.room {
        let shown = s.active_claims.iter().take(cfg.max_claims).count();
        let mut line = format!(
            "session [{} {} stale={}] claims=",
            s.session_id, s.name, s.stale
        );
        line.push_str(
            &s.active_claims
                .iter()
                .take(cfg.max_claims)
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        );
        if s.active_claims.len() > shown {
            line.push_str(&format!("…(+{} claims)", s.active_claims.len() - shown));
        }
        lines.push(line);
    }

    // Global char budget: drop whole trailing lines (lowest priority first),
    // recount the marker each time, until the brief fits. The cut is always
    // visible via the counted marker — never silent.
    let mut out = lines.join("\n");
    if out.len() > cfg.max_chars {
        let mut kept: Vec<String> = lines.iter().skip(1).map(|s| s.to_string()).collect();
        loop {
            let dropped = lines.len() - 1 - kept.len();
            let mut v = vec![lines[0].clone()];
            v.extend(kept.iter().cloned());
            v.push(format!(
                "…({dropped} lines cut by max_chars={})",
                cfg.max_chars
            ));
            out = v.join("\n");
            if out.len() <= cfg.max_chars || kept.is_empty() {
                break;
            }
            kept.pop();
        }
    }
    out.push('\n');
    out
}

impl Coord {
    /// Open a session checkpoint: snapshot this session's room state under a
    /// committed `revision`. Fails for unknown sessions and empty revisions.
    pub fn export_checkpoint(&self, session_id: &str, revision: &str) -> Result<Checkpoint> {
        if revision.trim().is_empty() {
            bail!("checkpoint refused: compile from a committed revision only");
        }
        let room_all = self.status()?;
        let me = room_all
            .iter()
            .find(|s| s.session_id == session_id)
            .ok_or_else(|| anyhow::anyhow!("unknown session {session_id}"))?;
        let session_name = me.name.clone();
        let project_root = me.project_root.clone();
        let mut claims = me.active_claims.clone();
        claims.sort();
        let mut tasks = self.task_list()?;
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        let inbox = self.inbox(session_id, true)?;
        let mut room = room_all;
        room.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        for s in &mut room {
            s.active_claims.sort();
        }
        Ok(Checkpoint {
            revision: revision.to_string(),
            session_id: session_id.to_string(),
            session_name,
            project_root,
            exported_at_ms: crate::now_ms(),
            claims,
            tasks,
            inbox,
            room,
        })
    }

    /// Always-on status: one line, no hydration. The smallest profile carries
    /// this; a full brief is compiled only on episode start.
    pub fn status_line(&self) -> Result<String> {
        let room = self.status()?;
        let stale = room.iter().filter(|s| s.stale).count();
        let claims: usize = room.iter().map(|s| s.active_claims.len()).sum();
        let tasks = self.task_list()?;
        let (p, i, b, d) = tasks.iter().fold((0, 0, 0, 0), |(p, i, b, d), t| {
            use crate::TaskStatus::*;
            match t.status {
                Pending => (p + 1, i, b, d),
                InProgress => (p, i + 1, b, d),
                Blocked => (p, i, b + 1, d),
                Done => (p, i, b, d + 1),
            }
        });
        Ok(format!(
            "room sessions={} stale={} claims={} tasks={} pending={} in_progress={} blocked={} done={}",
            room.len(),
            stale,
            claims,
            tasks.len(),
            p,
            i,
            b,
            d
        ))
    }

    /// Orphan-guard on write: admit `path` only when `session_id` holds a
    /// live claim whose literal prefix covers it. Anything else is an orphan
    /// write and is rejected — the writer must claim first.
    pub fn guard_write(&self, session_id: &str, path: &str) -> Result<()> {
        let room = self.status()?;
        let me = room
            .iter()
            .find(|s| s.session_id == session_id)
            .ok_or_else(|| anyhow::anyhow!("unknown session {session_id}"))?;
        let want = path.to_lowercase();
        for g in &me.active_claims {
            // Claim prefixes carry the separator before the wildcard
            // (`src/a/*` → `src/a/`); trim it so concrete paths compare
            // against the directory the claim actually covers.
            let p = crate::glob_prefix(g);
            let p = p.trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            if want == p || want.starts_with(&format!("{p}/")) {
                return Ok(());
            }
        }
        bail!("orphan write rejected: session {session_id} holds no live claim covering {path}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> (Coord, String, String) {
        let c = Coord::open_in_memory().unwrap();
        let a = c.join("alpha", 11, "/w").unwrap();
        let b = c.join("beta", 22, "/w").unwrap();
        c.claim(&a, "src/a/*", 3600).unwrap();
        c.claim(&b, "src/b/*", 3600).unwrap();
        let t1 = c.task_create("fix leak", "plug the fd leak", None).unwrap();
        let _t2 = c
            .task_create("write docs", "document the fix", Some(t1.as_str()))
            .unwrap();
        c.task_claim(&t1, &a).unwrap();
        c.send_message(&b, Some(a.as_str()), "heads up: touching src/b")
            .unwrap();
        c.send_message(&a, None, "broadcast: starting work")
            .unwrap();
        (c, a, b)
    }

    #[test]
    fn brief_determinism_golden() {
        let (c, a, _) = seed();
        let cp1 = c.export_checkpoint(&a, "rev-abc").unwrap();
        let cp2 = c.export_checkpoint(&a, "rev-abc").unwrap();
        // Different wall-clock exports…
        let b1 = compile_brief(&cp1, &BriefConfig::default());
        let b2 = compile_brief(&cp2, &BriefConfig::default());
        // …compile to byte-identical briefs.
        assert_eq!(b1, b2);
        // Every line cites an ID or a counted marker.
        assert!(b1.contains("rev:rev-abc"));
        assert!(b1.contains(&a));
        assert!(b1.contains("claim: src/a/*"));
        assert!(b1.contains("in_progress"));
        assert!(b1.contains("heads up: touching src/b"));
        assert!(fits_budget(&b1, &BriefConfig::default()));
        // Fingerprint is wall-clock free too.
        assert_eq!(cp1.fingerprint(), cp2.fingerprint());
    }

    #[test]
    fn orphan_write_rejected() {
        let (c, a, b) = seed();
        // Covered by own claim.
        assert!(c.guard_write(&a, "src/a/x.rs").is_ok());
        // Other session's claim does not cover.
        assert!(c.guard_write(&a, "src/b/y.rs").is_err());
        assert!(c.guard_write(&b, "src/b/y.rs").is_ok());
        // Unclaimed path is an orphan.
        assert!(c.guard_write(&a, "src/z/q.rs").is_err());
        // Unknown session is refused, not admitted.
        assert!(c.guard_write("nope", "src/a/x.rs").is_err());
        // After release the write is an orphan again.
        c.release(&a, "src/a/*").unwrap();
        assert!(c.guard_write(&a, "src/a/x.rs").is_err());
    }

    #[test]
    fn budget_enforced_and_counted() {
        let (c, a, _) = seed();
        for n in 0..30 {
            c.task_create(&format!("chore {n}"), "filler", None)
                .unwrap();
        }
        let cp = c.export_checkpoint(&a, "rev-budget").unwrap();
        let cfg = BriefConfig {
            max_chars: 600,
            max_tasks: 5,
            max_messages: 2,
            max_claims: 4,
        };
        let b1 = compile_brief(&cp, &cfg);
        let b2 = compile_brief(&cp, &cfg);
        assert_eq!(b1, b2);
        assert!(
            b1.len() <= cfg.max_chars,
            "brief len {} > {}",
            b1.len(),
            cfg.max_chars
        );
        assert!(b1.contains("omitted") || b1.contains("cut by max_chars"));
        let _ = brief_token_cost(&b1);
    }

    #[test]
    fn checkpoint_survives_clear() {
        // Exported JSON — not shared mutable state — carries continuity
        // across a `clear` boundary (fresh room, nothing shared).
        let (c, a, _) = seed();
        let cp = c.export_checkpoint(&a, "rev-clear").unwrap();
        let raw = cp.export_json().unwrap();
        let fresh = Coord::open_in_memory().unwrap();
        let fa = fresh.join("alpha-2", 33, "/w2").unwrap();
        let imported = Checkpoint::import_json(&raw).unwrap();
        let b_old = compile_brief(&imported, &BriefConfig::default());
        // The new room knows nothing, yet the imported brief is intact and
        // cites the original IDs.
        assert!(b_old.contains(&a));
        assert!(b_old.contains("rev:rev-clear"));
        assert!(fresh.export_checkpoint(&fa, "rev-clear").is_ok());
        // Revision-less checkpoints are refused at both doors.
        assert!(Checkpoint::import_json(r#"{"revision":"","session_id":"x","session_name":"n","project_root":"/","exported_at_ms":0,"claims":[],"tasks":[],"inbox":[],"room":[]}"#).is_err());
        assert!(c.export_checkpoint(&a, "  ").is_err());
    }

    #[test]
    fn status_line_is_one_line() {
        let (c, _, _) = seed();
        let line = c.status_line().unwrap();
        assert!(!line.contains('\n'));
        assert!(line.contains("sessions=2"));
        assert!(line.contains("claims=2"));
        assert!(line.contains("in_progress=1"));
    }
}

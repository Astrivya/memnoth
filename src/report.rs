//! Report this machine's sessions to the control plane.
//!
//! The console at `/app` has a Sessions page and a usage chart, and both are
//! built from `POST /api/v1/sessions`: one row per session, upserted by its id,
//! rolled into `usage_events` once the row closes. The reporting that existed
//! before this module lived in the MCP server, so it only fired for a hosted
//! `mcp serve` session — a person could work in the TUI for an hour and their
//! dashboard stayed empty. This is the TUI's half of that contract.
//!
//! Three rules, and each one is a decision rather than an implementation note:
//!
//! * **Never block the screen.** A report is handed to one background thread and
//!   the keystroke that caused it returns at once. A slow plane costs a line in
//!   a log file, never a frame.
//! * **Never fail a session.** Every error is appended to `~/.memnoth/report.log`
//!   instead of being raised or printed: the session file on this machine is the
//!   record of truth, the terminal is in raw mode, and a failed report must not
//!   be able to corrupt either.
//! * **Never send what privacy mode withholds.** `metadata-only` sends counts and
//!   never text; the transcript summary (the console's transcript tab) is
//!   included only for `balanced` and `open`, which is what those modes mean.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};

use memnoth_cloud_client::{HeartbeatRequest, SessionReport};
use memnoth_session::{Role, Session};
use serde_json::{Value, json};

use crate::context::AppContext;
use crate::sync::SyncPrivacyMode;

/// The build a heartbeat names.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How much of one message the transcript summary carries. The console renders a
/// quotation, not a document, and the full text stays in the session file here.
const QUOTE_LIMIT: usize = 2_000;

/// One point in a session's life that the plane is told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    /// The screen opened (or resurfaced) a conversation.
    Started,
    /// A turn finished, so the totals moved.
    Updated,
    /// The screen is closing.
    Finished,
}

/// One queued report: everything needed to build the body, resolved on the
/// worker thread so the caller only pays for a clone.
struct Job {
    ctx: AppContext,
    name: String,
    session: Session,
    event: Event,
}

/// Report that a conversation has been opened.
///
/// Called once per screen, after the session file has been read or created: the
/// id the plane stores is this file's id, so a resumed conversation lands on the
/// row it already has instead of a second one.
pub fn started(ctx: &AppContext, name: &str, session: &Session) {
    enqueue(ctx, name, session, Event::Started);
}

/// Report that a turn finished, so the row's tokens, files and cost are current.
///
/// Reports are cumulative totals and the plane upserts, so an update can never
/// double-count: only the close rolls a session into the usage ledger.
pub fn updated(ctx: &AppContext, name: &str, session: &Session) {
    enqueue(ctx, name, session, Event::Updated);
}

/// Report that the screen is closing.
///
/// This is the report that matters most: `ended_at` is what makes the plane roll
/// the session into usage, and a session that is only ever "running" is a
/// session the dashboard shows and the charts never count.
pub fn finished(ctx: &AppContext, name: &str, session: &Session) {
    enqueue(ctx, name, session, Event::Finished);
}

/// Hand one report to the worker, or drop it when this machine cannot report.
///
/// Dropped rather than queued for later: the plane has no offline story for
/// these rows (`mn sync` is how a session gets there after the fact), and a
/// screen that grows an unbounded queue for a signed-out machine is a screen
/// with a memory leak in it.
fn enqueue(ctx: &AppContext, name: &str, session: &Session, event: Event) {
    if !signed_in(ctx) {
        return;
    }
    let job = Job {
        ctx: ctx.clone(),
        name: name.to_string(),
        session: session.clone(),
        event,
    };
    // A send to a worker that failed to start is a dropped report, which is the
    // same outcome as a plane that refused it: nothing to raise, nothing to say.
    let _ = worker().send(job);
}

/// The one worker thread, started on first use.
///
/// One thread rather than one per report: a long session sends three reports a
/// turn, and a thread each would be a thread per keystroke's worth of work
/// whenever the plane is slow. Serialising them also keeps the plane's view of a
/// session ordered — a close can never overtake the update that preceded it.
fn worker() -> &'static Sender<Job> {
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx): (Sender<Job>, Receiver<Job>) = channel();
        let spawned = std::thread::Builder::new()
            .name("memnoth-report".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    send(&job);
                }
            });
        if let Err(error) = spawned {
            // Kept as a log line rather than a panic: reporting is optional.
            append_log(None, &format!("cannot start the report thread: {error}"));
        }
        tx
    })
}

/// Build and post one report. Runs on the worker thread.
fn send(job: &Job) {
    let client = match crate::sync::get_cloud_client(&job.ctx) {
        Ok(client) => client,
        Err(error) => {
            append_log(Some(&job.ctx), &format!("no client: {error}"));
            return;
        }
    };
    let mode = crate::sync::get_sync_privacy_mode(&job.ctx);
    let report = body(&job.ctx.root, mode, &job.name, &job.session, job.event);
    if let Err(error) = client.record_session(&report) {
        append_log(
            Some(&job.ctx),
            &format!("POST /api/v1/sessions for {} failed: {error}", report.id),
        );
    }

    // Liveness, alongside the row: the plane counts an instance only because it
    // beat, so a session that reports and never beats is a session the status
    // page shows as history from a machine that does not exist.
    let beat = HeartbeatRequest {
        instance_id: instance_id(),
        surface: "tui".to_string(),
        version: VERSION.to_string(),
        sessions_live: i32::from(job.event != Event::Finished),
    };
    if let Err(error) = client.heartbeat(&beat) {
        append_log(
            Some(&job.ctx),
            &format!("POST /api/v1/instances/heartbeat failed: {error}"),
        );
    }
}

/// The row this event describes.
///
/// Takes the workspace root and the privacy mode rather than the whole context,
/// so the row a session becomes can be checked without a machine to read one
/// from.
fn body(
    root: &std::path::Path,
    mode: SyncPrivacyMode,
    name: &str,
    session: &Session,
    event: Event,
) -> SessionReport {
    let (cost_micros, _) = session.spend();
    let mut files: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for write in &session.writes {
        files.insert(write.path.as_str());
    }
    let messages: Vec<(Role, String)> = session
        .messages
        .iter()
        .map(|message| (message.role, message.content.clone()))
        .collect();

    SessionReport {
        id: session.id.clone(),
        name: name.to_string(),
        repo: String::new(),
        branch: String::new(),
        model: session.model.clone(),
        status: match event {
            Event::Finished => "finished".to_string(),
            Event::Started | Event::Updated => "running".to_string(),
        },
        tokens: session.input_tokens.saturating_add(session.output_tokens) as i64,
        files: files.len() as i64,
        cost_micros: cost_micros as i64,
        workspace: root.to_string_lossy().to_string(),
        project_id: project_id(),
        control_url: None,
        events: transcript(mode, &messages),
        started_at: Some(rfc3339(session.created_ms)),
        ended_at: match event {
            Event::Finished => Some(rfc3339(memnoth_session::now_ms())),
            Event::Started | Event::Updated => None,
        },
    }
}

/// The bounded transcript summary the console's transcript tab reads.
///
/// `None` in `metadata-only` mode, and `None` before the first turn: the plane
/// renders absence as "not reported", which is honest, while four empty strings
/// would look like a session that said nothing.
pub(crate) fn transcript(mode: SyncPrivacyMode, messages: &[(Role, String)]) -> Option<Value> {
    if mode == SyncPrivacyMode::MetadataOnly || messages.is_empty() {
        return None;
    }
    let count = |role: Role| messages.iter().filter(|(r, _)| *r == role).count() as u64;
    let first_user = messages
        .iter()
        .find(|(role, _)| *role == Role::User)
        .map(|(_, text)| text.as_str())
        .unwrap_or_default();
    let last_assistant = messages
        .iter()
        .rev()
        .find(|(role, _)| *role == Role::Assistant)
        .map(|(_, text)| text.as_str())
        .unwrap_or_default();

    Some(json!({
        // The session's opening ask, reduced to one line: the console renders
        // this as what was asked, never as prose about what happened.
        "summary": one_line(first_user),
        "message_counts": {
            "user": count(Role::User),
            "assistant": count(Role::Assistant),
            "tool_calls": count(Role::Tool),
        },
        "first_user_msg": bounded(first_user),
        "last_assistant_msg": bounded(last_assistant),
    }))
}

/// The first line of `text`, trimmed and capped: a summary is a label.
fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= 120 {
        return line.to_string();
    }
    let cut: String = line.chars().take(119).collect();
    format!("{cut}…")
}

/// `text`, flattened to one line and capped at [`QUOTE_LIMIT`] characters.
fn bounded(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= QUOTE_LIMIT {
        return flat;
    }
    let cut: String = flat.chars().take(QUOTE_LIMIT - 1).collect();
    format!("{cut}…")
}

/// The cloud project every session from this machine is filed under, when
/// configured.
///
/// Read per call rather than cached: the variable is a setting, and the plane
/// makes the final decision anyway (a project-scoped key supplies its own).
pub(crate) fn project_id() -> Option<String> {
    std::env::var("MEMNOTH_PROJECT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// A stable identity for this machine, for the heartbeat.
///
/// The hostname: the plane's instance row is about "which machine", and a random
/// id per run would make every session a new instance and every count a new
/// number. A machine with no hostname reports as `memnoth-cli`, which at least
/// keeps the counts stable.
pub(crate) fn instance_id() -> String {
    for var in ["MEMNOTH_INSTANCE_ID", "HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(var)
            && !value.trim().is_empty()
        {
            return value.trim().to_string();
        }
    }
    "memnoth-cli".to_string()
}

/// Whether this machine holds a credential that could report.
///
/// Env first, so an exported key wins, then the stored account key that `/login`
/// writes — the same order every other cloud command uses.
fn signed_in(ctx: &AppContext) -> bool {
    if ["MEMNOTH_API_KEY", "MEMNOTH_MCP_TOKEN"].iter().any(|var| {
        std::env::var(var)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    }) {
        return true;
    }
    ctx.auth_token().is_some()
}

/// A millisecond epoch timestamp as RFC 3339, which is the only shape the plane
/// parses for `started_at` / `ended_at`.
pub(crate) fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Append one line to `~/.memnoth/report.log`.
///
/// A file rather than stderr: the TUI owns the terminal while this runs, and a
/// line printed under an alternate screen is a line that corrupts the frame it
/// lands on. Nothing here can fail loudly — a log that cannot be written is the
/// end of the attempt, not an error to report.
fn append_log(ctx: Option<&AppContext>, line: &str) {
    let Some(path) = ctx.map(|ctx| ctx.home.join("report.log")) else {
        return;
    };
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {line}", rfc3339(memnoth_session::now_ms()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_session::TurnReceipt;

    /// The row a session becomes: cumulative while it runs, closed when it ends.
    ///
    /// `ended_at` is the field that decides whether the plane ever counts the
    /// session's tokens, so "finished" and "has an end" have to arrive together —
    /// a closed row without one is a session that renders on the page and is
    /// never rolled into usage.
    #[test]
    fn a_session_runs_then_closes_and_only_the_close_has_an_end() {
        let mut session = Session::new("memnoth/auto");
        session.id = "s-test".to_string();
        session.user("fix the login redirect");
        session.account(100, 50);
        session.record_receipt(TurnReceipt {
            at_ms: 1,
            total_tokens: 150,
            prefix_hash: String::new(),
            layer_tokens: [0; 4],
            pieces: 0,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_micros: Some(2_000),
            turn_id: Some(1),
        });

        let running = body(
            std::path::Path::new("/w"),
            SyncPrivacyMode::Balanced,
            "session",
            &session,
            Event::Updated,
        );
        assert_eq!(running.id, "s-test");
        assert_eq!(running.status, "running");
        assert_eq!(running.tokens, 150);
        assert_eq!(running.cost_micros, 2_000);
        assert!(
            running.ended_at.is_none(),
            "a running session has not ended"
        );
        assert!(running.events.is_some(), "balanced mode reports a summary");

        let closed = body(
            std::path::Path::new("/w"),
            SyncPrivacyMode::Balanced,
            "session",
            &session,
            Event::Finished,
        );
        assert_eq!(closed.status, "finished");
        assert!(
            closed.ended_at.is_some(),
            "the close is what rolls up usage"
        );

        // And the one mode that withholds text withholds it here too.
        let quiet = body(
            std::path::Path::new("/w"),
            SyncPrivacyMode::MetadataOnly,
            "session",
            &session,
            Event::Finished,
        );
        assert!(quiet.events.is_none());
    }

    fn messages() -> Vec<(Role, String)> {
        vec![
            (Role::User, "fix the login redirect\nit loops".to_string()),
            (Role::Assistant, "the guard compares  hosts".to_string()),
            (Role::Tool, "ok".to_string()),
            (Role::Assistant, "done".to_string()),
        ]
    }

    /// `metadata-only` is the default mode, and it must not send a single word of
    /// the conversation.
    #[test]
    fn metadata_only_sends_no_text() {
        assert!(transcript(SyncPrivacyMode::MetadataOnly, &messages()).is_none());
    }

    /// The summary is the opening ask as a person would recognise it: one line,
    /// with the answer count kept beside it.
    #[test]
    fn the_summary_is_the_first_line_of_the_opening_ask() {
        let summary = transcript(SyncPrivacyMode::Balanced, &messages()).expect("summary");
        assert_eq!(summary["summary"], "fix the login redirect");
        assert_eq!(summary["message_counts"]["user"], 1);
        assert_eq!(summary["message_counts"]["assistant"], 2);
        assert_eq!(summary["message_counts"]["tool_calls"], 1);
        assert_eq!(summary["last_assistant_msg"], "done");
    }

    /// A session with no turns yet reports no transcript at all, so the console
    /// can say "not reported" instead of drawing empty quotes.
    #[test]
    fn no_messages_means_no_summary() {
        assert!(transcript(SyncPrivacyMode::Full, &[]).is_none());
    }

    /// A very long ask is cut, not sent whole: the row is a label, and the
    /// transcript stays on the machine that ran it.
    #[test]
    fn long_text_is_bounded() {
        let long = "x".repeat(QUOTE_LIMIT * 2);
        let summary = transcript(SyncPrivacyMode::Full, &[(Role::User, long)]).expect("summary");
        assert_eq!(
            summary["first_user_msg"].as_str().unwrap().chars().count(),
            QUOTE_LIMIT
        );
    }
}

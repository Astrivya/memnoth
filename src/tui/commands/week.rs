//! The local "this week" numbers, and what each one means.
//!
//! Three rules, and they are the whole design:
//!
//! * **Every number is a count of something on disk.** Sessions written in the
//!   window, the messages inside them, the edits and checkpoints those sessions
//!   recorded, and receipt files touched in the window. Nothing is estimated and
//!   nothing is converted into hours — "hours saved" is a claim this product
//!   cannot measure, so it never appears.
//! * **A small sample is not a statistic.** Below [`MIN_SESSIONS`] the totals are
//!   withheld and the line says how many sessions were seen and how many are
//!   needed. Showing "1 session, 2 messages" as a weekly summary teaches people
//!   to ignore the number.
//! * **The window is stated, not implied.** [`WEEK_MS`] is seven days against the
//!   session's own modification time, and the definition line says so.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::context::AppContext;
use crate::session::SessionStore;
use memnoth_session::Session;

/// The window: seven days, in milliseconds.
pub const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Below this many sessions in the window, no totals are shown.
pub const MIN_SESSIONS: usize = 3;

/// How many sessions are opened to read their own counters.
///
/// A weekly summary that walks a thousand conversation files to print five
/// numbers is a summary nobody waits for; the newest fifty are the ones a week
/// can contain in practice, and the count of sessions is taken from the listing
/// rather than from this scan.
pub const MAX_SCANNED: usize = 50;

/// One session's contribution, as read from its file.
pub struct SessionWeek {
    pub modified_ms: u64,
    pub messages: u64,
    pub writes: usize,
    pub checkpoints: usize,
    pub receipts: usize,
}

/// What the week came to, or why it is not being reported.
#[derive(Debug, PartialEq, Eq)]
pub enum Week {
    Enough {
        sessions: usize,
        messages: u64,
        writes: usize,
        checkpoints: usize,
        receipts: usize,
    },
    NotEnough {
        sessions: usize,
    },
}

impl Week {
    /// The one-line definition printed beside the numbers.
    pub fn definition() -> &'static str {
        "a session counts when its file was written in the last 7 days; messages, edits and \
         checkpoints are that session's own counters; receipts are files under .memnoth/receipts \
         modified in the same window. Nothing here is an estimate or an hours-saved figure."
    }
}

/// Turn per-session counters into the week's answer.
///
/// `now_ms` is passed in rather than read here so a test can stand at a fixed
/// moment; the window is half-open, so a session modified exactly seven days ago
/// is out.
pub fn summarize(now_ms: u64, all: &[SessionWeek]) -> Week {
    let recent: Vec<&SessionWeek> = all
        .iter()
        .filter(|session| now_ms.saturating_sub(session.modified_ms) < WEEK_MS)
        .collect();
    if recent.len() < MIN_SESSIONS {
        return Week::NotEnough {
            sessions: recent.len(),
        };
    }
    let scanned = &recent[..recent.len().min(MAX_SCANNED)];
    Week::Enough {
        sessions: recent.len(),
        messages: scanned.iter().map(|session| session.messages).sum(),
        writes: scanned.iter().map(|session| session.writes).sum(),
        checkpoints: scanned.iter().map(|session| session.checkpoints).sum(),
        receipts: scanned.iter().map(|session| session.receipts).sum(),
    }
}

/// Milliseconds since the epoch, or zero when the clock is before it.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Read the sessions on disk into their counters.
pub fn collect(ctx: &AppContext) -> Vec<SessionWeek> {
    SessionStore::new(&ctx.sessions_dir)
        .list()
        .into_iter()
        .take(MAX_SCANNED)
        .map(|summary| {
            let loaded = Session::load(Path::new(&summary.path)).ok();
            SessionWeek {
                modified_ms: u64::try_from(summary.modified_ms).unwrap_or(0),
                messages: u64::try_from(summary.messages).unwrap_or(0),
                writes: loaded.as_ref().map_or(0, |session| session.writes.len()),
                checkpoints: loaded
                    .as_ref()
                    .map_or(0, |session| session.checkpoints.len()),
                receipts: loaded.as_ref().map_or(0, |session| session.receipts.len()),
            }
        })
        .collect()
}

/// The lines `/brief` prints, and the same facts as data.
pub fn render(ctx: &AppContext) -> (Vec<String>, Value) {
    let week = summarize(now_ms(), &collect(ctx));
    match &week {
        Week::Enough {
            sessions,
            messages,
            writes,
            checkpoints,
            receipts,
        } => (
            vec![
                format!(
                    "  this week: {sessions} sessions · {messages} messages · {writes} edits · \
                     {checkpoints} checkpoints · {receipts} receipts"
                ),
                format!("    definitions: {}", Week::definition()),
            ],
            json!({
                "enough": true,
                "window_days": WEEK_MS / (24 * 60 * 60 * 1000),
                "min_sessions": MIN_SESSIONS,
                "sessions": sessions,
                "messages": messages,
                "edits": writes,
                "checkpoints": checkpoints,
                "receipts": receipts,
                "definition": Week::definition(),
            }),
        ),
        Week::NotEnough { sessions } => (
            vec![
                format!(
                    "  this week: {sessions} sessions in the last 7 days — below the minimum of \
                     {MIN_SESSIONS}, so no totals are shown"
                ),
                format!("    definitions: {}", Week::definition()),
            ],
            json!({
                "enough": false,
                "window_days": WEEK_MS / (24 * 60 * 60 * 1000),
                "min_sessions": MIN_SESSIONS,
                "sessions": sessions,
                "definition": Week::definition(),
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60 * 1000;

    fn session(days_ago: u64, messages: u64) -> SessionWeek {
        SessionWeek {
            modified_ms: 100 * DAY - days_ago * DAY,
            messages,
            writes: 1,
            checkpoints: 1,
            receipts: 0,
        }
    }

    #[test]
    fn a_small_sample_withholds_the_totals_and_says_why() {
        let week = summarize(100 * DAY, &[session(1, 10), session(2, 20)]);
        assert_eq!(week, Week::NotEnough { sessions: 2 });
    }

    #[test]
    fn three_sessions_are_enough_to_count() {
        let week = summarize(100 * DAY, &[session(1, 10), session(2, 20), session(3, 30)]);
        assert_eq!(
            week,
            Week::Enough {
                sessions: 3,
                messages: 60,
                writes: 3,
                checkpoints: 3,
                receipts: 0,
            }
        );
    }

    #[test]
    fn the_window_is_seven_days_and_half_open() {
        // Seven days old is out; six days and a bit is in.
        let week = summarize(
            100 * DAY,
            &[session(7, 1), session(6, 1), session(8, 1), session(0, 1)],
        );
        assert_eq!(
            week,
            Week::NotEnough { sessions: 2 },
            "exactly seven days old is outside the window"
        );
    }

    #[test]
    fn a_future_timestamp_does_not_underflow_into_the_past() {
        let mut ahead = session(0, 5);
        ahead.modified_ms = 200 * DAY;
        let week = summarize(100 * DAY, &[ahead, session(1, 1), session(2, 1)]);
        assert_eq!(
            week,
            Week::Enough {
                sessions: 3,
                messages: 7,
                writes: 3,
                checkpoints: 3,
                receipts: 0,
            },
            "a clock that jumped forward still counts the session"
        );
    }

    #[test]
    fn the_definition_names_the_window_and_refuses_hours() {
        let definition = Week::definition();
        assert!(definition.contains("7 days"), "{definition}");
        assert!(definition.contains("hours"), "it says what it does not do");
        assert!(definition.contains("estimate"), "{definition}");
    }
}

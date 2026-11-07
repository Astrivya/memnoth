//! Windows for the session, worktree, model, MCP, provider, config, team and
//! spend answers.
//!
//! Every row here is a field the verb already put in its `data`; nothing is
//! re-derived from the printed table, so the window and the pipe cannot drift.
//! Row actions are slash lines the TUI parser really accepts — `/worktree diff
//! <branch>` for a worktree, `/rewind` for the session list, `/mcp test` for a
//! client — and a family with no per-row command (a session, a config key, a
//! task) says so in its note rather than offering a chip that does nothing.

use serde_json::Value;

use crate::tui::overlays::popover::{
    PopoverButton, PopoverRow, PopoverSpec, follow_buttons, list, present, string,
};

pub(crate) fn spec(command: &str, data: &Value) -> Option<PopoverSpec> {
    match command {
        "session.list" => sessions(data),
        "worktree.list" => worktrees(data),
        "models.list" => models(data),
        "mcp.list" => mcp_clients(data),
        "connect.list" => providers(data),
        "config.list" => config_entries(data),
        "usage.show" | "stats.show" => spend(data, command),
        "team.tasks" => tasks(data),
        "team.inbox" => inbox(data),
        // Deliberately absent: `mcp.test` is one probe, `cloud.credits` and
        // `cloud.billing` are one balance, `auth.status` is one identity. A
        // window around a single line is a modal for no reason, so they stay the
        // sentences their verbs printed.
        _ => None,
    }
}

/// A millisecond timestamp as a UTC stamp, or `unknown`.
///
/// The same shape `session list` prints: a window whose dates read differently
/// from the line in the session file is a second truth.
fn stamp(millis: Option<&Value>) -> String {
    let Some(millis) = millis.and_then(Value::as_i64) else {
        return "unknown".to_string();
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Attach a verb's own follow-ups as footer chips.
fn with_follows(mut spec: PopoverSpec, data: &Value) -> PopoverSpec {
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

fn sessions(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "sessions")
        .iter()
        .map(|session| {
            let mut detail = Vec::new();
            let messages = session.get("messages").and_then(Value::as_i64);
            if let Some(messages) = messages {
                detail.push(format!("{messages} messages"));
            }
            detail.push(stamp(session.get("created_ms")));
            let model = present(session, "model");
            if let Some(model) = model {
                detail.push(model);
            }
            PopoverRow::new(string(session, "name"), detail.join(" · "))
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(format!("Sessions ({})", rows.len()), "from session.list")
        .rows(rows)
        .note("resume with `memnoth --resume <name>`: a running session cannot swap itself")
        // The Rewind overlay is the same store, browsable with keys.
        .button(PopoverButton::run('w', "rewind", "/rewind"));

    Some(with_follows(spec, data))
}

/// `worktree list` answers with a bare array, so this is the one spec that reads
/// the payload itself rather than a named field.
fn worktrees(data: &Value) -> Option<PopoverSpec> {
    let items = data.as_array()?;
    let rows = items
        .iter()
        .filter_map(|entry| {
            if !entry.is_object() {
                return None;
            }
            let path = string(entry, "path");
            let name = path
                .rsplit(['/', '\\'])
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(&path)
                .to_string();
            let branch = string(entry, "branch");
            let commit = present(entry, "commit");
            let mut detail = Vec::new();
            if !branch.is_empty() {
                detail.push(branch.clone());
            }
            if let Some(commit) = commit {
                detail.push(commit);
            }
            if entry
                .get("is_main")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                detail.push("main".to_string());
            }
            let mut row = PopoverRow::new(name, detail.join(" · "));
            if !branch.is_empty() {
                row = row.running(format!("/worktree diff {branch}"));
            }
            Some(row)
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(
        format!("Worktrees ({})", rows.len()),
        "from worktree.list — enter diffs a branch",
    )
    .rows(rows)
    .button(PopoverButton::run('l', "list", "/worktree list"));

    Some(with_follows(spec, data))
}

fn models(data: &Value) -> Option<PopoverSpec> {
    let connected: Vec<String> = data
        .get("connected_providers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let rows = list(data, "models")
        .iter()
        .map(|model| {
            let provider = string(model, "provider");
            let mut detail = Vec::new();
            if !provider.is_empty() {
                detail.push(provider.clone());
            }
            if let Some(description) = present(model, "description") {
                detail.push(description);
            }
            let mut row = PopoverRow::new(string(model, "name"), detail.join(" · "));
            if !provider.is_empty() {
                row.tag = Some(if connected.iter().any(|name| name == &provider) {
                    "connected".to_string()
                } else {
                    "no key".to_string()
                });
            }
            row
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let mut spec = PopoverSpec::new(
        format!("Models ({})", rows.len()),
        "from models.list — the model browser is where one is chosen",
    )
    .rows(rows)
    .button(PopoverButton::run('m', "models", "/models"));

    if connected.is_empty() {
        spec = spec.note("no provider has a key yet — /connect adds one");
    }
    Some(with_follows(spec, data))
}

fn mcp_clients(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "clients")
        .iter()
        .map(|client| {
            let registered = client
                .get("registered")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // Named `installed`, not `present`: `present` is the field helper, and
            // shadowing it here made the path lookup below a type error.
            let installed = client
                .get("present")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut detail = Vec::new();
            if let Some(path) = present(client, "path") {
                detail.push(path);
            }
            if let Some(note) = present(client, "detail") {
                detail.push(note);
            }
            let mut row = PopoverRow::new(string(client, "client"), detail.join(" · "));
            row.tag = Some(match (installed, registered) {
                (false, _) => "not installed".to_string(),
                (true, true) => "registered".to_string(),
                (true, false) => "no entry".to_string(),
            });
            row
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let mut spec = PopoverSpec::new(
        format!("MCP clients ({})", rows.len()),
        "from mcp.list — `t` probes the server, `d` checks the doctor",
    )
    .rows(rows)
    .button(PopoverButton::run('t', "test", "/mcp test"))
    .button(PopoverButton::run('d', "doctor", "/mcp doctor"));

    if let Some(url) = present(data, "url") {
        spec = spec.note(format!("server: {url}"));
    }
    Some(with_follows(spec, data))
}

fn providers(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "providers")
        .iter()
        .map(|provider| {
            let mut detail = Vec::new();
            let kind = string(provider, "kind");
            if !kind.is_empty() {
                detail.push(kind);
            }
            for key in ["model", "base_url"] {
                if let Some(value) = present(provider, key) {
                    detail.push(value);
                }
            }
            let mut row = PopoverRow::new(
                present(provider, "display_name").unwrap_or_else(|| string(provider, "name")),
                detail.join(" · "),
            );
            if provider
                .get("is_active")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                row.tag = Some("active".to_string());
            }
            row
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(
        format!("Providers ({})", rows.len()),
        "from connect.list — /connect is where one is added or removed",
    )
    .rows(rows)
    .button(PopoverButton::run('c', "connect", "/connect"));

    Some(with_follows(spec, data))
}

fn config_entries(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "entries")
        .iter()
        .map(|entry| {
            let mut detail = Vec::new();
            if let Some(value) = present(entry, "value") {
                detail.push(value);
            }
            if let Some(source) = present(entry, "source") {
                detail.push(format!("[{source}]"));
            }
            if let Some(description) = present(entry, "description") {
                detail.push(description);
            }
            PopoverRow::new(string(entry, "key"), detail.join(" · "))
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let mut spec = PopoverSpec::new(format!("Config ({})", rows.len()), "from config.list")
        .rows(rows)
        .note("set one with `memnoth config set <key> <value>` — no slash verb writes config");

    if let Some(path) = present(data, "config_path") {
        spec = spec.note(format!(
            "{path} — enter a key's value with `memnoth config set`"
        ));
    }
    Some(with_follows(spec, data))
}

/// `usage` and `stats` answer with the same shape, so they get the same window.
///
/// The payload carries three lists, and the window used to draw whichever one
/// was non-empty first: a by-model table hid the leaderboard, which is the only
/// other place a person appears by name. They are sections here — models, then
/// people, then the days when both are quiet — because they answer different
/// questions about the same spend.
fn spend(data: &Value, command: &str) -> Option<PopoverSpec> {
    let models = list(data, "models");
    let people = list(data, "leaderboard");
    let days = list(data, "series");

    let mut rows: Vec<PopoverRow> = Vec::new();
    if !models.is_empty() {
        rows.push(PopoverRow::section(format!("by model ({})", models.len())));
        rows.extend(models.iter().filter_map(model_row));
    }
    if !people.is_empty() {
        rows.push(PopoverRow::section(format!("by person ({})", people.len())));
        rows.extend(people.iter().filter_map(person_row));
    }
    // Both quiet is still a window worth opening: the daily series is what the
    // console's chart is drawn from, and it is the one list left to walk.
    if models.is_empty() && people.is_empty() && !days.is_empty() {
        rows.push(PopoverRow::section(format!("by day ({})", days.len())));
        rows.extend(days.iter().filter_map(day_row));
    }

    if rows.iter().all(|row| row.header) {
        return None;
    }

    let mut spec = PopoverSpec::new("Usage", format!("from {command}"))
        .rows(rows)
        .button(PopoverButton::open(
            'u',
            "console",
            crate::context::console_url("usage"),
        ));

    if let Some(totals) = data.get("totals") {
        let mut parts = Vec::new();
        if let Some(cost) = totals.get("cost_usd").and_then(Value::as_f64) {
            parts.push(format!("${cost:.2}"));
        }
        if let Some(tasks) = totals.get("tasks").and_then(Value::as_i64) {
            parts.push(format!("{tasks} tasks"));
        }
        if let Some(range) = present(data, "range") {
            parts.push(range);
        }
        if data.get("demo").and_then(Value::as_bool).unwrap_or(false) {
            parts.push("demo data".to_string());
        }
        if !parts.is_empty() {
            spec = spec.note(parts.join(" · "));
        }
    }

    Some(with_follows(spec, data))
}

/// One model: what it cost and how much of the window it served.
fn model_row(model: &Value) -> Option<PopoverRow> {
    let name = string(model, "model");
    if name.is_empty() {
        return None;
    }
    let mut detail = Vec::new();
    if let Some(cost) = model.get("cost_usd").and_then(Value::as_f64) {
        detail.push(format!("${cost:.2}"));
    }
    if let Some(tokens) = model.get("tokens").and_then(Value::as_i64) {
        detail.push(format!("{tokens} tokens"));
    }
    let provider = string(model, "provider");
    if !provider.is_empty() {
        detail.push(provider);
    }
    Some(PopoverRow::new(name, detail.join(" · ")))
}

/// One person: the org's own leaderboard row, address first because that is
/// what an administrator acts on.
fn person_row(person: &Value) -> Option<PopoverRow> {
    let who = present(person, "email").or_else(|| present(person, "name"))?;
    let mut detail = Vec::new();
    if let Some(cost) = person.get("spend_usd").and_then(Value::as_f64) {
        detail.push(format!("${cost:.2}"));
    }
    if let Some(tokens) = person.get("tokens").and_then(Value::as_i64) {
        detail.push(format!("{tokens} tokens"));
    }
    if let Some(tasks) = person.get("tasks").and_then(Value::as_i64) {
        detail.push(format!("{tasks} tasks"));
    }
    Some(PopoverRow::new(who, detail.join(" · ")))
}

/// One day of the series: the same numbers the console charts.
fn day_row(day: &Value) -> Option<PopoverRow> {
    let stamp = present(day, "day")?;
    let mut detail = Vec::new();
    let tokens_in = day.get("tokens_in").and_then(Value::as_i64);
    let tokens_out = day.get("tokens_out").and_then(Value::as_i64);
    if tokens_in.is_some() || tokens_out.is_some() {
        detail.push(format!(
            "{} in · {} out",
            tokens_in.unwrap_or(0),
            tokens_out.unwrap_or(0)
        ));
    }
    if let Some(cost) = day.get("cost_usd").and_then(Value::as_f64) {
        detail.push(format!("${cost:.2}"));
    }
    if let Some(tasks) = day.get("tasks").and_then(Value::as_i64) {
        detail.push(format!("{tasks} tasks"));
    }
    Some(PopoverRow::new(stamp, detail.join(" · ")))
}

fn tasks(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "tasks")
        .iter()
        .map(|task| {
            let status = string(task, "status");
            let mut detail = Vec::new();
            if let Some(claimed) = present(task, "claimed_by") {
                detail.push(format!("claimed by {claimed}"));
            }
            if let Some(description) = present(task, "description") {
                detail.push(description);
            }
            let mut row = PopoverRow::new(string(task, "title"), detail.join(" · "));
            if !status.is_empty() {
                row.tag = Some(status);
            }
            row
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(format!("Tasks ({})", rows.len()), "from team.tasks")
        .rows(rows)
        .note("claim one with `memnoth team claim <id>` — no slash verb claims a task");

    Some(with_follows(spec, data))
}

fn inbox(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "messages")
        .iter()
        .map(|message| {
            let from = string(message, "from_session");
            let mut detail = Vec::new();
            if let Some(stamp_text) = present(message, "created_at") {
                detail.push(stamp_text);
            }
            let mut row = PopoverRow::new(
                present(message, "body").unwrap_or_default(),
                detail.join(" · "),
            );
            if !from.is_empty() {
                row.title = format!("{from}: {}", row.title);
            }
            if message
                .get("acked")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                row.tag = Some("acked".to_string());
            }
            row
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(format!("Inbox ({})", rows.len()), "from team.inbox")
        .rows(rows)
        .note("acknowledge with `memnoth team inbox --ack`");

    Some(with_follows(spec, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_session_row_carries_its_turns_and_its_stamp() {
        let data = json!({
            "sessions": [
                {
                    "name": "payments",
                    "model": "claude-sonnet-4-5",
                    "created_ms": 1_700_000_000_000i64,
                    "messages": 12,
                    "path": "sessions/payments.json"
                }
            ],
            "count": 1
        });

        let spec = spec("session.list", &data).expect("a window");
        assert_eq!(spec.title, "Sessions (1)");
        assert_eq!(spec.rows[0].title, "payments");
        assert!(
            spec.rows[0].detail.contains("12 messages"),
            "{:?}",
            spec.rows[0]
        );
        assert!(
            spec.rows[0].detail.contains("2023-11-14"),
            "{:?}",
            spec.rows[0]
        );
        // Nothing in the TUI can resume a session in place, so the row says so
        // instead of offering a line the parser would reject.
        assert!(spec.rows[0].action.is_none());
        assert!(spec.buttons.iter().any(|button| button.key == 'w'));
    }

    #[test]
    fn an_empty_list_is_no_window_at_all() {
        assert!(spec("session.list", &json!({ "sessions": [], "count": 0 })).is_none());
        assert!(spec("worktree.list", &json!([])).is_none());
    }

    #[test]
    fn a_worktree_row_diffs_its_branch() {
        let data = json!([
            { "path": "/repo/.memnoth/worktrees/payments", "branch": "worktree/payments", "commit": "abc1234", "is_main": false },
            { "path": "/repo", "branch": "main", "commit": "def5678", "is_main": true }
        ]);

        let spec = spec("worktree.list", &data).expect("a window");
        assert_eq!(spec.title, "Worktrees (2)");
        assert_eq!(spec.rows[0].title, "payments");
        assert_eq!(
            spec.rows[0].action.as_ref().map(|action| action.describe()),
            Some("/worktree diff worktree/payments".to_string())
        );
        assert!(spec.rows[1].detail.contains("main"), "{:?}", spec.rows[1]);
    }

    #[test]
    fn an_mcp_client_is_tagged_by_what_was_found() {
        let data = json!({
            "url": "http://127.0.0.1:8787/mcp",
            "token_present": true,
            "clients": [
                { "client": "zed", "path": "settings.json", "present": true, "registered": true, "found": true, "ok": true, "detail": "registered" }
            ]
        });

        let spec = spec("mcp.list", &data).expect("a window");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("registered"));
        assert_eq!(
            spec.note.as_deref(),
            Some("server: http://127.0.0.1:8787/mcp")
        );
        let keys: Vec<char> = spec.buttons.iter().map(|button| button.key).collect();
        assert!(keys.contains(&'t') && keys.contains(&'d'), "{keys:?}");
    }

    /// One answer carries both lists; a window that drew either hid half of it.
    #[test]
    fn spend_walks_the_models_and_the_people_in_one_window() {
        let data = json!({
            "range": "30d",
            "demo": false,
            "totals": { "cost_usd": 12.5, "tasks": 40 },
            "series": [],
            "leaderboard": [{ "email": "a@b.c", "name": "a", "spend_usd": 1.0, "tokens": 10, "tasks": 1 }],
            "models": [{ "provider": "anthropic", "model": "opus", "tokens": 1000, "cost_usd": 12.5 }]
        });

        let spec = spec("usage.show", &data).expect("a window");
        assert_eq!(spec.title, "Usage");
        let sections: Vec<&str> = spec
            .rows
            .iter()
            .filter(|row| row.header)
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(
            sections,
            vec!["by model (1)", "by person (1)"],
            "{sections:?}"
        );

        let model = spec
            .rows
            .iter()
            .find(|row| row.title == "opus")
            .expect("the model row");
        assert!(model.detail.contains("$12.50"), "{}", model.detail);
        let person = spec
            .rows
            .iter()
            .find(|row| row.title == "a@b.c")
            .expect("the person row");
        assert!(person.detail.contains("1 tasks"), "{}", person.detail);
        assert!(spec.note.as_deref().unwrap_or_default().contains("$12.50"));
        assert!(spec.buttons.iter().any(|button| button.key == 'u'));
    }

    /// A window with no models and no people still has the days to walk.
    #[test]
    fn a_quiet_spend_window_falls_back_to_the_daily_series() {
        let data = json!({
            "range": "7d",
            "demo": false,
            "totals": { "cost_usd": 0.5, "tasks": 3 },
            "series": [{ "day": "2026-11-03", "tokens_in": 1200, "tokens_out": 400, "cost_usd": 0.5, "tasks": 3 }],
            "leaderboard": [],
            "models": []
        });

        let spec = spec("stats.show", &data).expect("a window");
        assert_eq!(spec.rows[0].title, "by day (1)");
        assert_eq!(spec.rows[1].title, "2026-11-03");
        assert!(
            spec.rows[1].detail.contains("1200 in · 400 out"),
            "{}",
            spec.rows[1].detail
        );
    }

    #[test]
    fn a_command_this_module_does_not_own_is_left_to_the_next_spec() {
        assert!(spec("kg.refs", &json!({ "references": [] })).is_none());
        assert!(spec("cloud.credits", &json!({ "balance_micros": 1 })).is_none());
    }
}

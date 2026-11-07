//! Windows for the operational answers: a batch of workers, a parallel sweep, a
//! team audit, installed hooks, a CI index run and the doctor's verdicts.
//!
//! These verbs print a table that is worth walking rather than scrolling, and
//! every row here is a field the verb already put in its `data` — a worker's own
//! exit code, not one re-derived from the printed line. Row actions are slash
//! lines the dropdown really offers (`/worktree diff <branch>`, `/mcp test`), and
//! a family with no per-row command says so in its note instead of drawing a chip
//! that runs nothing.

use serde_json::Value;

use crate::tui::overlays::popover::{
    PopoverButton, PopoverRow, PopoverSpec, follow_buttons, list, present, string,
};

pub(crate) fn spec(command: &str, data: &Value) -> Option<PopoverSpec> {
    match command {
        "batch" => batch(data),
        "parallel" => parallel(data),
        "team.audit" => audit(data),
        "team.presence" => presence(data),
        "hooks.install" => hooks(data),
        "ci.index" => ci_index(data),
        "mcp.doctor" => doctor(data),
        // `team.review` answers with one request — who asked, who was asked, and
        // the message. A window around three fields is a modal for no reason, so
        // it stays the sentence the verb printed.
        _ => None,
    }
}

/// Attach a verb's own follow-ups as footer chips.
fn with_follows(mut spec: PopoverSpec, data: &Value) -> PopoverSpec {
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

/// A value that can be pasted into a slash line.
///
/// A word with a space in it would become two arguments and run a different
/// command than the row promised, so a row refuses the action rather than
/// offering one that cannot work.
fn word(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    Some(trimmed.to_string())
}

/// `exit 0`, `exit 2`, or the reason there is no code at all.
fn exit_status(entry: &Value) -> (String, bool) {
    match entry.get("exit_code").and_then(Value::as_i64) {
        Some(0) => ("exit 0".to_string(), true),
        Some(code) => (format!("exit {code}"), false),
        None => ("no exit status".to_string(), false),
    }
}

/// The first line of a worker's output: enough to recognise the run, not a wall.
fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// One row per worker, shared by `batch` and `parallel` because the answer is the
/// same shape from the same crate.
fn worker_row(entry: &Value, title: &str) -> PopoverRow {
    let (status, ok) = exit_status(entry);
    let mut detail = vec![status];
    if let Some(ms) = entry.get("duration_ms").and_then(Value::as_i64) {
        detail.push(format!("{ms}ms"));
    }
    if let Some(error) = present(entry, "error") {
        detail.push(error);
    } else if let Some(line) = present(entry, "stdout").and_then(|out| first_line(&out)) {
        detail.push(line);
    }

    let mut row = PopoverRow::new(title, detail.join(" · "));
    row.tag = Some(if ok {
        "ok".to_string()
    } else {
        "failed".to_string()
    });
    row
}

/// `batch` answers with a bare array of `BatchResult`.
fn batch(data: &Value) -> Option<PopoverSpec> {
    let results = data.as_array()?;
    let rows: Vec<PopoverRow> = results
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.is_object())
        .map(|(index, entry)| {
            let id = present(entry, "worker_id").unwrap_or_else(|| format!("worker {}", index + 1));
            // A batch worker has no branch in its answer, so there is nothing to
            // diff. If a future payload names one, the row learns to open it.
            let branch = present(entry, "branch").and_then(|branch| word(&branch));
            let row = worker_row(entry, &id);
            match branch {
                Some(branch) => row.running(format!("/worktree diff {branch}")),
                None => row,
            }
        })
        .collect();

    if rows.is_empty() {
        return None;
    }

    let total = rows.len();
    let failed = rows
        .iter()
        .filter(|row| row.tag.as_deref() == Some("failed"))
        .count();
    let mut spec = PopoverSpec::new(
        format!("Batch ({total})"),
        "one row per worker — the exit code is the worker's own",
    )
    .rows(rows)
    .button(PopoverButton::run('l', "worktrees", "/worktree list"));

    if failed > 0 {
        spec = spec.note(format!("{failed} of {total} worker(s) failed"));
    }
    Some(with_follows(spec, data))
}

/// `parallel` answers with a bare array of `ParallelResult`.
fn parallel(data: &Value) -> Option<PopoverSpec> {
    let results = data.as_array()?;
    let rows: Vec<PopoverRow> = results
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.is_object())
        .map(|(index, entry)| {
            let target =
                present(entry, "target").unwrap_or_else(|| format!("target {}", index + 1));
            let row = worker_row(entry, &target);
            // A target is a worktree or branch name; `worktree diff` takes one.
            match word(&target) {
                Some(target) => row.running(format!("/worktree diff {target}")),
                None => row,
            }
        })
        .collect();

    if rows.is_empty() {
        return None;
    }

    let total = rows.len();
    let failed = rows
        .iter()
        .filter(|row| row.tag.as_deref() == Some("failed"))
        .count();
    let mut spec = PopoverSpec::new(
        format!("Parallel ({total})"),
        "one row per target, in the order they finished",
    )
    .rows(rows);

    if failed > 0 {
        spec = spec.note(format!("{failed} of {total} target(s) failed"));
    }
    Some(with_follows(spec, data))
}

/// `team audit` answers with sessions, tasks, workers, claims and messages.
///
/// Workers are the rows worth walking — a worker is a run with a status and a
/// result. With none recorded the task list is the next best list of things, and
/// both are real keys rather than a fallback guessed from the text.
fn audit(data: &Value) -> Option<PopoverSpec> {
    let mut rows: Vec<PopoverRow> = list(data, "workers")
        .iter()
        .map(|worker| {
            let id = string(worker, "id");
            let title = present(worker, "name").unwrap_or_else(|| id.clone());
            let mut detail = Vec::new();
            if let Some(task) = present(worker, "task") {
                detail.push(task);
            }
            if let Some(session) = present(worker, "session_id") {
                detail.push(format!("session {session}"));
            }
            if let Some(error) = present(worker, "error") {
                detail.push(error);
            }
            let status = string(worker, "status");
            let mut row = PopoverRow::new(title, detail.join(" · "));
            if !status.is_empty() {
                row.tag = Some(status);
            }
            row
        })
        .collect();

    if rows.is_empty() {
        rows = list(data, "tasks")
            .iter()
            .map(|task| {
                let mut detail = Vec::new();
                if let Some(claimed) = present(task, "claimed_by") {
                    detail.push(format!("claimed by {claimed}"));
                }
                let status = string(task, "status");
                let mut row = PopoverRow::new(string(task, "title"), detail.join(" · "));
                if !status.is_empty() {
                    row.tag = Some(status);
                }
                row
            })
            .collect();
    }

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(format!("Team audit ({})", rows.len()), "from team.audit")
        .rows(rows)
        .note(format!(
            "{} session(s) · {} task(s) · {} message(s)",
            list(data, "sessions").len(),
            list(data, "tasks").len(),
            list(data, "messages").len()
        ))
        .button(PopoverButton::run('p', "presence", "/team presence"));

    Some(with_follows(spec, data))
}

/// `team presence` answers with local sessions and, when synced, cloud members.
///
/// The two lists are drawn in one window and tagged by where they came from: a
/// local row is this machine's heartbeat, a cloud row is somebody else's.
fn presence(data: &Value) -> Option<PopoverSpec> {
    let mut rows: Vec<PopoverRow> = Vec::new();

    let local = list(data, "local_sessions");
    for session in local {
        let name = present(session, "name").unwrap_or_else(|| string(session, "session_id"));
        let mut detail = Vec::new();
        if let Some(age) = session.get("heartbeat_age_ms").and_then(Value::as_i64) {
            detail.push(format!("{}s ago", age / 1000));
        }
        if let Some(claims) = session.get("active_claims").and_then(Value::as_i64)
            && claims > 0
        {
            detail.push(format!("{claims} claim(s)"));
        }
        let mut row = PopoverRow::new(name, detail.join(" · "));
        row.tag = Some(
            if session
                .get("stale")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "stale".to_string()
            } else {
                "here".to_string()
            },
        );
        rows.push(row);
    }

    let cloud = list(data, "cloud_members");
    for member in cloud {
        let name = present(member, "name").unwrap_or_else(|| string(member, "user_id"));
        let mut row = PopoverRow::new(name, present(member, "user_id").unwrap_or_default());
        row.tag = Some(
            if member
                .get("online")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "online".to_string()
            } else {
                "offline".to_string()
            },
        );
        rows.push(row);
    }

    if rows.is_empty() {
        return None;
    }

    let synced = data
        .get("cloud_synced")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut spec = PopoverSpec::new(format!("Presence ({})", rows.len()), "from team.presence")
        .rows(rows)
        .button(PopoverButton::run('a', "audit", "/team audit"));

    if !synced {
        spec = spec.note("local only — the cloud roster was not read for this answer");
    }
    Some(with_follows(spec, data))
}

/// `hooks install` answers with the files it touched, or with the reason it left
/// an existing hook alone.
fn hooks(data: &Value) -> Option<PopoverSpec> {
    let installed = data
        .get("installed")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut rows = Vec::new();
    for (key, label) in [
        ("hook_path", "pre-commit"),
        ("hooks_yaml_path", ".pre-commit-hooks.yaml"),
    ] {
        if let Some(path) = present(data, key) {
            let mut row = PopoverRow::new(label, path);
            row.tag = Some(if installed {
                "written".to_string()
            } else {
                "unchanged".to_string()
            });
            rows.push(row);
        }
    }

    // Nothing was written, but the path that already held a hook is still the
    // useful fact: it is the file a person has to look at.
    if !installed
        && rows.is_empty()
        && let Some(path) = present(data, "path")
    {
        rows.push(
            PopoverRow::new("pre-commit", path)
                .tagged("already present")
                .running("/hooks install --force"),
        );
    }

    if rows.is_empty() {
        return None;
    }

    let subtitle = if installed {
        "from hooks.install"
    } else {
        "from hooks.install — nothing was overwritten"
    };
    Some(
        PopoverSpec::new(format!("Hooks ({})", rows.len()), subtitle)
            .rows(rows)
            .note("git runs these before a commit; `--force` replaces an existing one"),
    )
}

/// `ci index` answers with the CI it detected and the index run's own result.
///
/// Only an indexed run has rows to show; the "not in CI" answer is one sentence
/// and stays one sentence.
fn ci_index(data: &Value) -> Option<PopoverSpec> {
    if !data
        .get("indexed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }

    // Owned, because `&Value::Null` in this position is a temporary that the
    // borrow below would outlive.
    let empty = Value::Null;
    let kg = data.get("kg_result").unwrap_or(&empty);
    let rows: Vec<PopoverRow> = list(kg, "languages")
        .iter()
        .map(|entry| {
            let files = entry.get("files").and_then(Value::as_i64).unwrap_or(0);
            let symbols = entry.get("symbols").and_then(Value::as_i64).unwrap_or(0);
            PopoverRow::new(
                string(entry, "language"),
                format!("{files} file(s) · {symbols} symbol(s)"),
            )
        })
        .collect();

    if rows.is_empty() {
        return None;
    }

    let mut note = format!(
        "{} file(s) · {} symbol(s)",
        kg.get("files").and_then(Value::as_i64).unwrap_or(0),
        kg.get("symbols").and_then(Value::as_i64).unwrap_or(0)
    );
    if let Some(name) = data
        .get("ci_info")
        .and_then(|info| present(info, "ci_name"))
    {
        note.push_str(&format!(" · {name}"));
    }

    Some(
        PopoverSpec::new(
            format!("CI index ({})", rows.len()),
            "from ci.index — the same breakdown `kg index` reports",
        )
        .rows(rows)
        .note(note)
        .button(PopoverButton::run('s', "status", "/kg status")),
    )
}

/// `mcp doctor` answers with the server's verdict, then one row per client.
///
/// The first row is the server, because a doctor whose server cannot run makes
/// every row under it meaningless.
fn doctor(data: &Value) -> Option<PopoverSpec> {
    let mut rows: Vec<PopoverRow> = Vec::new();

    if data.get("server").is_some() || data.get("server_ok").is_some() {
        let mut detail = Vec::new();
        if let Some(path) = present(data, "server") {
            detail.push(path);
        }
        if let Some(note) = present(data, "server_detail") {
            detail.push(note);
        }
        let mut row = PopoverRow::new(
            "server",
            if detail.is_empty() {
                "no detail".to_string()
            } else {
                detail.join(" · ")
            },
        );
        row.tag = Some(match data.get("server_ok").and_then(Value::as_bool) {
            Some(true) => "ok".to_string(),
            Some(false) => "failed".to_string(),
            None => "not run".to_string(),
        });
        rows.push(row);
    }

    for client in list(data, "clients") {
        let registered = client
            .get("registered")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
        rows.push(row);
    }

    if rows.is_empty() {
        return None;
    }

    let mut spec = PopoverSpec::new(format!("Doctor ({})", rows.len()), "from mcp.doctor")
        .rows(rows)
        .button(PopoverButton::run('l', "list", "/mcp list"))
        .button(PopoverButton::run('t', "test", "/mcp test"));

    let mut parts = Vec::new();
    if let Some(url) = present(data, "url") {
        parts.push(url);
    }
    if let Some(profile) = present(data, "profile") {
        let tools = data
            .get("profile_tools")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        parts.push(format!("profile {profile} · {tools} tools"));
    }
    if !data
        .get("token_present")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        parts.push("no token in the environment".to_string());
    }
    if !parts.is_empty() {
        spec = spec.note(parts.join(" · "));
    }
    Some(with_follows(spec, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_batch_row_is_the_workers_own_exit_code() {
        let data = json!([
            { "worker_id": "w1", "exit_code": 0, "stdout": "tests passed\n", "stderr": "",
              "duration_ms": 1200, "error": null },
            { "worker_id": "w2", "exit_code": 2, "stdout": "", "stderr": "boom",
              "duration_ms": 40, "error": null }
        ]);

        let spec = spec("batch", &data).expect("a window");
        assert_eq!(spec.title, "Batch (2)");
        assert_eq!(spec.rows[0].title, "w1");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("ok"));
        assert!(
            spec.rows[0].detail.contains("tests passed"),
            "{:?}",
            spec.rows[0]
        );
        assert_eq!(spec.rows[1].tag.as_deref(), Some("failed"));
        assert!(spec.rows[1].detail.contains("exit 2"), "{:?}", spec.rows[1]);
        assert!(spec.note.as_deref().unwrap_or_default().contains("1 of 2"));
    }

    #[test]
    fn a_parallel_row_opens_the_worktree_it_ran_in() {
        let data = json!([
            { "target": "feature/a", "exit_code": 0, "stdout": "ok", "stderr": "",
              "duration_ms": 10, "error": null }
        ]);

        let spec = spec("parallel", &data).expect("a window");
        assert_eq!(spec.rows[0].title, "feature/a");
        assert_eq!(
            spec.rows[0].action.as_ref().map(|action| action.describe()),
            Some("/worktree diff feature/a".to_string())
        );
    }

    #[test]
    fn a_target_with_a_space_gets_no_action_rather_than_a_broken_line() {
        let data = json!([
            { "target": "feature a", "exit_code": 0, "stdout": "", "stderr": "",
              "duration_ms": 1, "error": null }
        ]);

        let spec = spec("parallel", &data).expect("a window");
        assert!(spec.rows[0].action.is_none());
    }

    #[test]
    fn a_team_audit_walks_workers_and_counts_the_rest() {
        let data = json!({
            "workers": [
                { "id": "wr-1", "name": "indexer", "session_id": "s1", "task": "reindex",
                  "status": "done", "started_at": 1, "completed_at": 2, "result": null,
                  "error": null }
            ],
            "sessions": [{ "session_id": "s1", "name": "one" }],
            "tasks": [{ "id": "t1", "title": "reindex", "status": "done", "claimed_by": "s1" }],
            "claims": [],
            "messages": []
        });

        let spec = spec("team.audit", &data).expect("a window");
        assert_eq!(spec.title, "Team audit (1)");
        assert_eq!(spec.rows[0].title, "indexer");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("done"));
        assert!(
            spec.rows[0].detail.contains("reindex"),
            "{:?}",
            spec.rows[0]
        );
        assert!(
            spec.note
                .as_deref()
                .unwrap_or_default()
                .contains("1 session(s)"),
            "{:?}",
            spec.note
        );
        assert!(spec.buttons.iter().any(|button| button.key == 'p'));
    }

    #[test]
    fn presence_tags_a_local_heartbeat_and_a_cloud_member_differently() {
        let data = json!({
            "local_sessions": [
                { "session_id": "s1", "name": "payments", "pid": 12,
                  "project_root": "/repo", "heartbeat_age_ms": 4000, "stale": false,
                  "active_claims": 2 }
            ],
            "cloud_synced": true,
            "cloud_members": [
                { "name": "Dana", "user_id": "u1", "online": true }
            ]
        });

        let spec = spec("team.presence", &data).expect("a window");
        assert_eq!(spec.title, "Presence (2)");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("here"));
        assert!(spec.rows[0].detail.contains("4s ago"), "{:?}", spec.rows[0]);
        assert!(
            spec.rows[0].detail.contains("2 claim(s)"),
            "{:?}",
            spec.rows[0]
        );
        assert_eq!(spec.rows[1].title, "Dana");
        assert_eq!(spec.rows[1].tag.as_deref(), Some("online"));
    }

    #[test]
    fn an_unchanged_hook_names_the_file_it_left_alone() {
        let data = json!({
            "installed": false,
            "reason": "already_exists",
            "path": ".git/hooks/pre-commit"
        });

        let spec = spec("hooks.install", &data).expect("a window");
        assert_eq!(spec.rows[0].title, "pre-commit");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("already present"));
        assert_eq!(
            spec.rows[0].action.as_ref().map(|action| action.describe()),
            Some("/hooks install --force".to_string())
        );
    }

    #[test]
    fn a_ci_run_that_indexed_nothing_is_not_a_window() {
        let data = json!({ "indexed": false, "reason": "not_in_ci",
                           "ci_info": { "is_ci": false } });
        assert!(spec("ci.index", &data).is_none());
    }

    #[test]
    fn a_ci_run_shows_the_language_breakdown_the_index_reported() {
        let data = json!({
            "indexed": true,
            "ci_info": { "is_ci": true, "ci_name": "GitHub Actions", "detected": ["GITHUB_ACTIONS"] },
            "kg_result": {
                "languages": [{ "language": "rust", "files": 12, "symbols": 340 }],
                "files": 12,
                "symbols": 340,
                "references": 900
            }
        });

        let spec = spec("ci.index", &data).expect("a window");
        assert_eq!(spec.rows[0].title, "rust");
        assert!(
            spec.rows[0].detail.contains("12 file(s)"),
            "{:?}",
            spec.rows[0]
        );
        assert!(
            spec.note
                .as_deref()
                .unwrap_or_default()
                .contains("GitHub Actions"),
            "{:?}",
            spec.note
        );
    }

    #[test]
    fn the_doctor_puts_the_server_verdict_first() {
        let data = json!({
            "url": "https://memnoth.astrivya.ai/mcp",
            "profile": "full",
            "profile_tools": 12,
            "server": "/usr/local/bin/memnoth-mcp",
            "server_ok": false,
            "server_detail": "initialize timed out",
            "token_present": true,
            "clients": [
                { "client": "claude", "path": "~/.claude.json", "present": true,
                  "registered": true, "found": true, "ok": true, "detail": "registered" }
            ],
            "tool_counts": []
        });

        let spec = spec("mcp.doctor", &data).expect("a window");
        assert_eq!(spec.rows[0].title, "server");
        assert_eq!(spec.rows[0].tag.as_deref(), Some("failed"));
        assert!(spec.rows[0].detail.contains("initialize timed out"));
        assert_eq!(spec.rows[1].title, "claude");
        assert_eq!(spec.rows[1].tag.as_deref(), Some("registered"));
        assert!(
            spec.note
                .as_deref()
                .unwrap_or_default()
                .contains("12 tools"),
            "{:?}",
            spec.note
        );
    }

    #[test]
    fn a_command_this_module_does_not_own_is_left_to_the_next_spec() {
        assert!(spec("team.review", &json!({ "task_id": "t1" })).is_none());
        assert!(spec("kg.refs", &json!({ "references": [] })).is_none());
    }
}

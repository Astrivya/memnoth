//! Windows for `projects …` answers: the org's projects, and the queue of
//! sessions no project has claimed.
//!
//! The projects page lives on the console, but filing is something a person does
//! *while* they are in a session — which is why the list and the queue are drawn
//! here with the row's command on Enter instead of being a table to copy ids out
//! of. Every field is one the verb already put in its `data`, so the window and
//! the pipe cannot disagree.
//!
//! What stays text: `projects.use` answers which project this machine files
//! under, `projects.create`/`rename`/`delete`/`file` report the one thing that
//! changed. A modal around one line is a box that has to be closed before work
//! continues.

use serde_json::Value;

use crate::tui::overlays::popover::{
    PopoverButton, PopoverRow, PopoverSpec, follow_buttons, list, present, string,
};

pub(crate) fn spec(command: &str, data: &Value) -> Option<PopoverSpec> {
    match command {
        "projects.list" => projects(data),
        "projects.unassigned" => unassigned(data),
        _ => None,
    }
}

/// The org's projects, one row each, Enter files this machine under it.
fn projects(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "projects")
        .iter()
        .map(|project| {
            let id = string(project, "id");
            let mut detail = vec![format!("id {id}")];
            if let Some(visibility) = present(project, "visibility") {
                detail.push(format!("vis {visibility}"));
            }
            if let Some(access) = present(project, "access_level") {
                detail.push(format!("you: {access}"));
            }
            if let Some(repos) = project.get("repos").and_then(Value::as_i64) {
                detail.push(format!("{repos} repo(s)"));
            }
            if let Some(members) = project.get("member_count").and_then(Value::as_i64) {
                detail.push(format!("{members} member(s)"));
            }
            PopoverRow::new(string(project, "name"), detail.join(" · "))
                .running(format!("/projects use {id}"))
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(
        format!("Projects ({})", rows.len()),
        "from projects.list — enter files this machine's sessions under a project",
    )
    .rows(rows)
    .note("`/projects create <name>` adds one; `mn projects use --clear` stops filing")
    .button(PopoverButton::run(
        'u',
        "unassigned",
        "/projects unassigned",
    ));

    Some(with_follows(spec, data))
}

/// Sessions waiting for a project: Enter files the selected one where this
/// machine already reports, because the alternative — a form that asks for an
/// id — is the same copy-paste the console page exists to avoid.
fn unassigned(data: &Value) -> Option<PopoverSpec> {
    let rows = list(data, "sessions")
        .iter()
        .map(|session| {
            let id = string(session, "id");
            let mut detail = Vec::new();
            let model = string(session, "model");
            detail.push(if model.is_empty() {
                "unknown model".to_string()
            } else {
                model
            });
            if let Some(status) = present(session, "status") {
                detail.push(status);
            }
            if let Some(tokens) = session.get("tokens").and_then(Value::as_i64) {
                detail.push(format!("{tokens} tok"));
            }
            if let Some(started) = present(session, "started_at") {
                detail.push(started.chars().take(16).collect());
            }
            PopoverRow::new(short_id(&id), detail.join(" · "))
                .running(format!("/projects file {id}"))
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return None;
    }

    let spec = PopoverSpec::new(
        format!("Awaiting a project ({})", rows.len()),
        "from projects.unassigned — enter files the session under this machine's project",
    )
    .rows(rows)
    .note("no project set? `/projects use <id>` first, or `/projects file <id> --project <id>`")
    .button(PopoverButton::run('l', "projects", "/projects"));

    Some(with_follows(spec, data))
}

/// Enough of the id to recognise the row; the full one is on the command.
fn short_id(id: &str) -> String {
    let head: String = id.chars().take(8).collect();
    if head.chars().count() < id.chars().count() {
        format!("{head}…")
    } else {
        head
    }
}

/// Attach a verb's own follow-ups as footer chips.
fn with_follows(mut spec: PopoverSpec, data: &Value) -> PopoverSpec {
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::overlays::popover::PopoverAction;
    use serde_json::json;

    fn lines(command: &str, data: Value) -> PopoverSpec {
        spec(command, &data).expect("a window for this answer")
    }

    /// Every project is a row whose Enter is the verb that files under it.
    #[test]
    fn a_project_row_files_this_machine_under_it() {
        let spec = lines(
            "projects.list",
            json!({ "projects": [
                { "id": "3f2a", "name": "acme-api", "visibility": "org",
                  "access_level": "admin", "member_count": 4, "repos": 2 }
            ] }),
        );
        assert_eq!(spec.title, "Projects (1)");
        let row = &spec.rows[0];
        assert_eq!(row.title, "acme-api");
        assert!(row.detail.contains("id 3f2a"), "{}", row.detail);
        assert!(row.detail.contains("4 member(s)"), "{}", row.detail);
        assert_eq!(
            row.action,
            Some(PopoverAction::Run("/projects use 3f2a".into()))
        );
    }

    /// The queue is walkable, and Enter files the selected session instead of
    /// asking the person to retype its id.
    #[test]
    fn an_unassigned_row_files_the_session_it_names() {
        let spec = lines(
            "projects.unassigned",
            json!({ "count": 1, "sessions": [
                { "id": "sess-1234567890", "status": "finished", "model": "kilo-auto/free",
                  "tokens": 465, "started_at": "2026-11-01T09:20:00Z" }
            ] }),
        );
        assert_eq!(spec.title, "Awaiting a project (1)");
        let row = &spec.rows[0];
        assert_eq!(row.title, "sess-123…");
        assert!(row.detail.contains("465 tok"), "{}", row.detail);
        assert_eq!(
            row.action,
            Some(PopoverAction::Run("/projects file sess-1234567890".into()))
        );
    }

    /// An empty queue is a sentence, not a box that has to be dismissed.
    #[test]
    fn an_empty_queue_has_no_window() {
        assert!(
            spec(
                "projects.unassigned",
                &json!({ "count": 0, "sessions": [] })
            )
            .is_none()
        );
        assert!(spec("projects.list", &json!({ "projects": [] })).is_none());
    }
}

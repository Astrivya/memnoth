//! `mn projects` — the org's projects, as the console lists them.
//!
//! A project is the org-scoped container a session is filed under; the console
//! has a page for creating, renaming and deleting them, and until now the CLI
//! could only point at that page. The five read and write verbs speak the same
//! routes (`/api/v1/projects`, `/api/v1/projects/{id}`) so the two surfaces
//! cannot disagree about what a project is.
//!
//! `use` is the one that is not about the org's list but about this machine:
//! it stores the project id in `config.toml`, which is what every session row
//! reads when it is reported. Without it a session lands in the console's
//! unassigned queue, where someone has to file it by hand after the work is done.
//!
//! Two choices worth naming:
//!
//! * **The id is always printed.** Every other verb here takes an id, and a list
//!   that showed only names would make the reader go back to the console to find
//!   one.
//! * **A project is never looked up by name.** Names are not unique on the
//!   server, so resolving one locally could delete the wrong project. `delete`
//!   and `rename` take the id the list printed.
//!
//! Rename and delete name the project in their prompt, but a failed lookup is not
//! fatal: the label is decoration, the write is the operation.

use anyhow::Result;
use serde_json::json;

use crate::AppContext;
use crate::cloud::signed_in_client;
use crate::output::{Outcome, confirm};
use memnoth_cloud_client::{ProjectInfo, ProjectsResponse};

/// Execute a `mn projects` subcommand.
pub fn run(ctx: &AppContext, args: &crate::cli::ProjectsArgs) -> Result<Outcome> {
    match &args.action {
        crate::cli::ProjectsCommand::List(_) => list(ctx),
        crate::cli::ProjectsCommand::Create(create) => {
            create_project(ctx, &create.name, create.visibility.as_deref())
        }
        crate::cli::ProjectsCommand::Rename(rename) => {
            rename_project(ctx, &rename.id, &rename.name)
        }
        crate::cli::ProjectsCommand::Delete(delete) => delete_project(ctx, &delete.id, delete.yes),
        crate::cli::ProjectsCommand::Use(args) => use_project(ctx, args),
    }
}

/// `mn projects list` — every project this credential can see.
fn list(ctx: &AppContext) -> Result<Outcome> {
    let client = signed_in_client(ctx)?;
    let response = client.projects()?;

    let data = json!({
        "projects": response.projects.iter().map(project_json).collect::<Vec<_>>(),
    });

    Ok(Outcome::new("projects.list", render_list(&response), data))
}

/// `mn projects create <name> [--visibility]` — a new project.
fn create_project(ctx: &AppContext, name: &str, visibility: Option<&str>) -> Result<Outcome> {
    // The server rejects a blank name too, but it says `name` and nothing else;
    // here the message can name the argument that is wrong.
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("a project needs a name: `mn projects create <name>`");
    }

    let client = signed_in_client(ctx)?;
    let project = client.create_project(name, visibility)?;

    let mut text = format!("Created project {} ({})\n", project.name, project.id);
    text.push_str(&format!(
        "  visibility {} · you: {} · {} repo(s) · {} member(s)\n",
        project.visibility, project.access_level, project.repos, project.member_count
    ));
    text.push_str(&format!(
        "\nConsole: {}\n",
        crate::context::console_url("projects")
    ));

    Ok(Outcome::new(
        "projects.create",
        text,
        json!({ "project": project_json(&project) }),
    ))
}

/// `mn projects rename <id> <name>` — retitle one project.
fn rename_project(ctx: &AppContext, id: &str, name: &str) -> Result<Outcome> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("a project needs a name: `mn projects rename <id> <name>`");
    }

    let client = signed_in_client(ctx)?;

    // Best effort: the old name makes the answer readable ("renamed A to B"),
    // but a failed *read* must not stop the write the person actually asked for.
    let previous = client
        .projects()
        .ok()
        .and_then(|response| response.projects.into_iter().find(|p| p.id == id))
        .map(|project| project.name);

    let project = client.rename_project(id, name)?;

    let text = match previous {
        Some(previous) if previous != project.name => {
            format!(
                "Renamed \"{previous}\" to \"{}\" ({})\n",
                project.name, project.id
            )
        }
        Some(_) => format!(
            "Project \"{}\" already had that name ({}).\n",
            project.name, project.id
        ),
        None => format!("Renamed project {} to \"{}\".\n", project.id, project.name),
    };

    Ok(Outcome::new(
        "projects.rename",
        text,
        json!({ "project": project_json(&project) }),
    ))
}

/// `mn projects delete <id> [--yes]` — destructive, so it asks first.
fn delete_project(ctx: &AppContext, id: &str, yes: bool) -> Result<Outcome> {
    let client = signed_in_client(ctx)?;

    let label = match client
        .projects()
        .ok()
        .and_then(|response| response.projects.into_iter().find(|p| p.id == id))
    {
        Some(project) => format!("{} ({})", project.name, project.id),
        None => id.to_string(),
    };

    // Sessions outlive their project (`project_id` is set to NULL, not deleted),
    // which is the one thing a person about to delete should be told.
    if !confirm(
        &format!("Delete project {label}? Its sessions are kept, unassigned."),
        yes,
    ) {
        return Ok(cancelled());
    }

    client.delete_project(id)?;

    // A stored choice pointing at a project that no longer exists would refuse
    // every later report on this machine (the plane answers 404 for an unknown
    // project), so it is cleared together with the project it named.
    let unset = set_project(ctx, None)
        .ok()
        .flatten()
        .filter(|held| held == id);

    Ok(Outcome::new(
        "projects.delete",
        format!(
            "Deleted project {label}. Its sessions are now unassigned.{}\nConsole: {}\n",
            if unset.is_some() {
                "\nThis machine was filing under it; that choice is cleared too."
            } else {
                ""
            },
            crate::context::console_url("projects")
        ),
        json!({ "deleted": true, "id": id, "unset": unset.is_some() }),
    ))
}

/// `mn projects use <id> [--clear]` — file every session from this machine under
/// one project, or stop doing that.
///
/// The choice is written to `~/.memnoth/config.toml`, which [`crate::report`] and
/// [`crate::sync`] read for every row they send. The id is checked against the
/// org's list before it is stored: a typo here would otherwise surface only when
/// a finished session was refused, with the work already done and the row left
/// unassigned.
fn use_project(ctx: &AppContext, args: &crate::cli::ProjectsUseArgs) -> Result<Outcome> {
    if args.clear {
        let previous = set_project(ctx, None)?;
        let text = match previous {
            Some(previous) => format!(
                "Sessions from this machine are unassigned again (was {previous}).\n"
            ),
            None => "No project was set; sessions are still unassigned.\n".to_string(),
        };
        return Ok(Outcome::new(
            "projects.use",
            text,
            json!({ "project_id": null }),
        ));
    }

    // A bare `use` answers the question rather than changing anything: which
    // project is this machine reporting to. It reads only the local config, so it
    // works signed out.
    let Some(id) = args
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(current_project(ctx));
    };

    let client = signed_in_client(ctx)?;
    let response = client.projects()?;
    let Some(project) = response.projects.iter().find(|project| project.id == id) else {
        let known: Vec<String> = response
            .projects
            .iter()
            .map(|project| format!("  {}  {}", project.id, project.name))
            .collect();
        anyhow::bail!(
            "{id} is not a project this credential can see; nothing was changed.\n{}",
            if known.is_empty() {
                "There are no projects yet: `mn projects create <name>`.".to_string()
            } else {
                format!("Projects you can see:\n{}", known.join("\n"))
            }
        );
    };

    set_project(ctx, Some(project.id.clone()))?;

    let text = format!(
        "Sessions from this machine are now filed under {} ({}).\n\
         Applies to every session this config file is read for; MEMNOTH_PROJECT_ID still wins \
         over it for one run.\n",
        project.name, project.id
    );

    Ok(Outcome::new(
        "projects.use",
        text,
        json!({ "project_id": project.id, "project": project_json(project) }),
    ))
}

/// Which project this machine is filing under, from the config and the env.
fn current_project(ctx: &AppContext) -> Outcome {
    let stored = ctx
        .config()
        .project
        .clone()
        .filter(|id| !id.trim().is_empty());
    let env = std::env::var("MEMNOTH_PROJECT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let text = match (&stored, &env) {
        (_, Some(env)) => format!(
            "MEMNOTH_PROJECT_ID files reports under {env}, which overrides the stored choice{}.\n",
            stored
                .as_deref()
                .map(|id| format!(" ({id})"))
                .unwrap_or_default()
        ),
        (Some(id), None) => format!(
            "Sessions from this machine are filed under {id}.\n\
             `mn projects use --clear` stops that.\n"
        ),
        (None, None) => "No project set: sessions report as unassigned.\n\
                          `mn projects use <id>` files them under one.\n"
            .to_string(),
    };

    Outcome::new(
        "projects.use",
        text,
        json!({ "project_id": stored, "env_project_id": env }),
    )
}

/// Write (or clear) the stored project, returning what was there before.
fn set_project(ctx: &AppContext, project: Option<String>) -> Result<Option<String>> {
    let mut config = ctx.config_mut();
    let previous = config.project.take();
    config.project = project;
    config.save(&ctx.config_path)?;
    Ok(previous)
}

/// The answer when the person says no: not a failure, so the exit code is 0.
fn cancelled() -> Outcome {
    Outcome::new(
        "projects.delete",
        "Cancelled: nothing was deleted.\n",
        json!({ "cancelled": true }),
    )
}

/// The one project shape both the text and the envelope are built from.
fn project_json(project: &ProjectInfo) -> serde_json::Value {
    json!({
        "id": project.id,
        "name": project.name,
        "visibility": project.visibility,
        "access_level": project.access_level,
        "member_count": project.member_count,
        "repos": project.repos,
        "created_at": project.created_at,
    })
}

/// The list, or a sentence that tells the reader what to do next.
///
/// An empty table is indistinguishable from a broken one, so an org with no
/// projects gets prose and both ways to make one.
fn render_list(response: &ProjectsResponse) -> String {
    if response.projects.is_empty() {
        return format!(
            "No projects yet.\n\nCreate one with `mn projects create <name>`, or in the console: {}\n",
            crate::context::console_url("projects")
        );
    }

    let mut out = String::new();
    out.push_str(&format!("Projects ({})\n\n", response.projects.len()));
    for project in &response.projects {
        out.push_str(&render_row(project));
    }
    out.push_str(&format!(
        "\nNew project: `mn projects create <name>` · console: {}\n",
        crate::context::console_url("projects")
    ));
    out
}

/// One project: the name for reading, then the facts every other verb needs.
fn render_row(project: &ProjectInfo) -> String {
    format!(
        "{}\n  id {} · vis {} · you: {} · {} repo(s) · {} member(s)\n",
        project.name,
        project.id,
        project.visibility,
        project.access_level,
        project.repos,
        project.member_count
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, id: &str) -> ProjectInfo {
        ProjectInfo {
            id: id.into(),
            name: name.into(),
            visibility: "org".into(),
            access_level: "admin".into(),
            member_count: 4,
            repos: 2,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn an_empty_list_says_how_to_make_one() {
        let text = render_list(&ProjectsResponse { projects: vec![] });
        assert!(text.contains("No projects yet"), "{text}");
        assert!(text.contains("mn projects create"), "{text}");
        assert!(!text.contains("Projects (0)"), "{text}");
    }

    #[test]
    fn a_row_carries_the_name_and_the_id_every_other_verb_takes() {
        let response = ProjectsResponse {
            projects: vec![project("acme-api", "3f2a")],
        };
        let text = render_list(&response);
        assert!(text.contains("Projects (1)"), "{text}");
        assert!(text.contains("acme-api"), "{text}");
        assert!(text.contains("id 3f2a"), "{text}");
        assert!(text.contains("you: admin"), "{text}");
    }

    #[test]
    fn refusing_a_delete_is_not_a_failure() {
        let outcome = cancelled();
        assert_eq!(outcome.command, "projects.delete");
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.text.contains("Cancelled"), "{}", outcome.text);
        assert_eq!(outcome.data["cancelled"], json!(true));
    }

    #[test]
    fn the_envelope_carries_the_whole_project() {
        let value = project_json(&project("acme-api", "3f2a"));
        assert_eq!(value["id"], json!("3f2a"));
        assert_eq!(value["visibility"], json!("org"));
        assert_eq!(value["repos"], json!(2));
        assert_eq!(value["member_count"], json!(4));
    }
}

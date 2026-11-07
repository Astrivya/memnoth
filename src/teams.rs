//! `mn teams` — one org's teams, plus joining one by invite code.
//!
//! A **team** here is a group of people inside an org that decides who shares
//! work with whom (`/api/v1/teams`, the console's Teams page). It never grants a
//! role: the role an org member holds elsewhere is the `org_role` printed beside
//! them, and the permission to change a team is the server's own `can_manage`.
//!
//! That is a different noun from the **shared task board** `mn teams
//! claim|tasks|inbox` coordinate on: the board is work in flight, a team is
//! structure. Both live under `teams` because both are about a group of people,
//! but nothing here reads or writes board state, and the text says which is
//! which rather than leaving the reader to guess.
//!
//! Every row prints the team's id, because every other team command takes it.

use anyhow::{Context, Result};

use crate::context::AppContext;
use crate::output::Outcome;
use memnoth_cloud_client::{
    AcceptInviteResponse, CloudClient, Team, TeamDetail, TeamMember, TeamProject, TeamsResponse,
};
use serde_json::{Value, json};

/// Run the `mn teams join` command.
pub fn join(ctx: &AppContext, args: &crate::cli::TeamsJoinArgs) -> Result<Outcome> {
    let base_url = get_base_url(ctx)?;
    let mut client =
        CloudClient::with_base_url(&base_url).context("failed to create cloud client")?;

    // Get auth token from credential store (synchronous)
    let token = memnoth_auth::account_key_sync()
        .ok_or_else(|| anyhow::anyhow!("not authenticated; run `mn auth login` first"))?;

    client.set_auth_token(token);

    let response: AcceptInviteResponse = client
        .accept_invite(&args.code)
        .context("failed to accept invite")?;

    let text = format!(
        "Joined organization {} as {}",
        response.org_id, response.role
    );
    let data = json!({
        "org_id": response.org_id,
        "role": response.role,
    });

    Ok(Outcome::new("teams.join", text, data))
}

/// `mn teams list` — the org's teams, with what the caller may do with each.
pub fn list(ctx: &AppContext, _args: &crate::cli::TeamsListArgs) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;
    let response = client.teams()?;

    let data = json!({
        "teams": response.teams.iter().map(team_json).collect::<Vec<_>>(),
    });

    Ok(Outcome::new("teams.list", render_teams(&response), data))
}

/// `mn teams create <name>` — a new team, which the creator owns.
pub fn create(ctx: &AppContext, args: &crate::cli::TeamsCreateArgs) -> Result<Outcome> {
    // Refused here rather than at the server: the empty argument is on this
    // line, and the server's 400 names the field, not the command.
    let name = args.name.trim();
    anyhow::ensure!(
        !name.is_empty(),
        "a team needs a name: `mn teams create <name>`"
    );

    let client = crate::cloud::signed_in_client(ctx)?;
    let team = client.create_team(name)?;

    Ok(Outcome::new(
        "teams.create",
        render_created(&team),
        json!({ "team": team_json(&team) }),
    ))
}

/// `mn teams members <id>` — one team: its people, and the projects it reaches.
pub fn members(ctx: &AppContext, args: &crate::cli::TeamsMembersArgs) -> Result<Outcome> {
    let id = args.id.trim();
    anyhow::ensure!(
        !id.is_empty(),
        "a team id is required: `mn teams members <id>`"
    );

    let client = crate::cloud::signed_in_client(ctx)?;
    let detail = client.team(id)?;

    let data = json!({
        "team": team_json(&detail.team),
        "members": detail.members.iter().map(team_member_json).collect::<Vec<_>>(),
        "projects": detail.projects.iter().map(team_project_json).collect::<Vec<_>>(),
        "can_manage": detail.can_manage,
    });

    Ok(Outcome::new("teams.members", render_detail(&detail), data))
}

/* ---------------------------------- views ---------------------------------- */

/// The list, or a sentence that says what to do next.
///
/// An empty table is indistinguishable from a broken one, so an org with no
/// teams gets prose and both ways to make one.
fn render_teams(response: &TeamsResponse) -> String {
    if response.teams.is_empty() {
        return format!(
            "No teams in this org yet.\n\nCreate one with `mn teams create <name>`, or in the \
             console: {}\n",
            crate::context::console_url("teams")
        );
    }

    let mut out = format!("Teams ({})\n\n", response.teams.len());
    for team in &response.teams {
        out.push_str(&render_row(team));
    }

    out.push_str(&format!(
        "\nOne team's people: `mn teams members <id>` · new team: `mn teams create <name>`\n\
         Console: {}\n",
        crate::context::console_url("teams")
    ));
    out
}

/// One team: the name for reading, then the facts every other command needs.
fn render_row(team: &Team) -> String {
    let mut markers = Vec::new();
    if team.is_member {
        markers.push("yours");
    }
    if team.can_manage {
        markers.push("can manage");
    }
    // Only when true: a list of `not yours · cannot manage` on every row is
    // noise, and the absence of a marker is the ordinary case.
    let suffix = if markers.is_empty() {
        String::new()
    } else {
        format!(" · {}", markers.join(" · "))
    };

    format!(
        "{}\n  id {} · {} member(s) · {} project(s){suffix}\n",
        team.name, team.id, team.member_count, team.project_count
    )
}

/// The mint view: what was made, and where to go next.
fn render_created(team: &Team) -> String {
    format!(
        "Created team {} ({})\n  {} member(s) · {} project(s)\n\
         \nAdd people and grant projects in the console: {}\n",
        team.name,
        team.id,
        team.member_count,
        team.project_count,
        crate::context::console_url("teams")
    )
}

/// One team in full: who is in it, and what it can reach.
fn render_detail(detail: &TeamDetail) -> String {
    let team = &detail.team;
    let mut out = String::new();

    out.push_str(&format!("{}\n", team.name));
    out.push_str(&format!(
        "{}\n\n",
        "=".repeat(team.name.chars().count().max(1))
    ));

    out.push_str(&format!("Id: {}\n", team.id));
    out.push_str(&format!(
        "Members: {}   Projects: {}\n",
        team.member_count, team.project_count
    ));
    if team.is_member {
        out.push_str("You are in this team.\n");
    }

    // The server computed this; printing the org role instead would let the
    // text promise an action the API then refuses.
    if detail.can_manage {
        out.push_str(
            "This credential can change the team (rename, delete, members) in the console.\n",
        );
    } else {
        out.push_str("This credential cannot change the team — its creator or an org admin can.\n");
    }

    out.push_str("\nMembers\n");
    if detail.members.is_empty() {
        out.push_str("  none yet\n");
    }
    for member in &detail.members {
        out.push_str(&format!("  {}\n", member_line(member)));
    }

    // The projects a team reaches are its reason to exist, so an empty list is
    // stated rather than left as a heading with nothing under it.
    out.push_str("\nProjects\n");
    if detail.projects.is_empty() {
        out.push_str("  none — a team reaches a project only through a grant\n");
    }
    for project in &detail.projects {
        out.push_str(&format!("  {} ({})\n", project.name, project.level));
    }

    out.push_str(&format!(
        "\nConsole: {}\n",
        crate::context::console_url("teams")
    ));
    out
}

/// One member: the address for reading, the id the member routes take.
///
/// `org_role` is the role they hold in the *org*, which is what decides what
/// they may do outside this team; the team itself grants nothing.
fn member_line(member: &TeamMember) -> String {
    let name = if member.name.is_empty() {
        String::new()
    } else {
        format!("  ({})", member.name)
    };
    format!(
        "{email}  org role {role}  {id}{name}",
        email = member.email,
        role = member.org_role,
        id = member.user_id
    )
}

/* --------------------------------- shapes ---------------------------------- */

fn team_json(team: &Team) -> Value {
    json!({
        "id": team.id,
        "name": team.name,
        "project_count": team.project_count,
        "member_count": team.member_count,
        "is_member": team.is_member,
        "can_manage": team.can_manage,
        "created_by": team.created_by,
        "created_by_email": team.created_by_email,
        "created_at": team.created_at,
    })
}

fn team_member_json(member: &TeamMember) -> Value {
    json!({
        "user_id": member.user_id,
        "email": member.email,
        "name": member.name,
        "org_role": member.org_role,
    })
}

fn team_project_json(project: &TeamProject) -> Value {
    json!({
        "id": project.id,
        "name": project.name,
        "level": project.level,
    })
}

/// Get the base URL for the cloud API.
fn get_base_url(_ctx: &AppContext) -> Result<String> {
    Ok(std::env::var("MEMNOTH_CLOUD_URL")
        .unwrap_or_else(|_| "https://memnoth.astrivya.ai/api/v1".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(name: &str, member_count: i64, project_count: i64) -> Team {
        Team {
            id: "4f2a9c10-0000-4000-8000-000000000003".into(),
            name: name.into(),
            project_count,
            member_count,
            is_member: false,
            can_manage: false,
            created_by: Some("6f1c2b7a-0000-4000-8000-000000000001".into()),
            created_by_email: Some("owner@example.com".into()),
            created_at: "2026-03-01T00:00:00Z".into(),
        }
    }

    fn member(email: &str, org_role: &str) -> TeamMember {
        TeamMember {
            user_id: "6f1c2b7a-0000-4000-8000-000000000009".into(),
            email: email.into(),
            name: "Dev Example".into(),
            org_role: org_role.into(),
        }
    }

    #[test]
    fn an_empty_list_says_how_to_make_one() {
        let text = render_teams(&TeamsResponse {
            teams: vec![],
            demo: false,
        });
        assert!(text.contains("No teams in this org yet"), "{text}");
        assert!(text.contains("mn teams create"), "{text}");
        assert!(text.contains("/app/teams"), "{text}");
        assert!(!text.contains("Teams (0)"), "{text}");
    }

    #[test]
    fn a_row_carries_the_id_and_the_callers_markers() {
        let mut mine = team("platform", 3, 2);
        mine.is_member = true;
        mine.can_manage = true;

        let text = render_teams(&TeamsResponse {
            teams: vec![mine, team("design", 1, 0)],
            demo: false,
        });

        assert!(text.contains("Teams (2)"), "{text}");
        assert!(text.contains("platform"), "{text}");
        assert!(
            text.contains("id 4f2a9c10-0000-4000-8000-000000000003"),
            "every other command takes the id: {text}"
        );
        assert!(
            text.contains("3 member(s) · 2 project(s) · yours · can manage"),
            "{text}"
        );
        // The team the caller is not in must not borrow the markers.
        let design = text.find("design").expect("design row");
        assert!(
            !text[design..].contains("yours"),
            "markers belong to their own row: {text}"
        );
    }

    #[test]
    fn a_team_with_no_members_or_projects_states_both_rather_than_printing_headings() {
        let detail = TeamDetail {
            team: team("platform", 0, 0),
            members: vec![],
            projects: vec![],
            can_manage: false,
        };
        let text = render_detail(&detail);

        assert!(text.contains("Members: 0   Projects: 0"), "{text}");
        assert!(text.contains("none yet"), "{text}");
        assert!(
            text.contains("a team reaches a project only through a grant"),
            "{text}"
        );
        assert!(text.contains("cannot change the team"), "{text}");
    }

    #[test]
    fn the_detail_names_each_project_and_its_grant_level() {
        let detail = TeamDetail {
            team: team("platform", 1, 1),
            members: vec![member("dev@example.com", "developer")],
            projects: vec![TeamProject {
                id: "proj-1".into(),
                name: "payments".into(),
                level: "writer".into(),
            }],
            can_manage: true,
        };
        let text = render_detail(&detail);

        assert!(text.contains("payments (writer)"), "{text}");
        assert!(
            text.contains("dev@example.com  org role developer"),
            "{text}"
        );
        assert!(text.contains("can change the team"), "{text}");
        // The org role is not the team's permission: the server's answer decides.
        assert!(!text.contains("cannot change the team"), "{text}");
    }
}

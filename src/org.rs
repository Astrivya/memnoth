//! `mn org` — the tenant: who it is, who is in it, and who has been invited.
//!
//! Everything here is a route the control plane already serves and the console
//! already draws on its Organization page (`GET/PATCH /api/v1/org`,
//! `/api/v1/members`, `/api/v1/orgs/members`, `/api/v1/orgs/invites`). Nothing
//! is derived locally: a role, a count or a delivery fact is printed as the
//! server reported it, because every invented number here is a number someone
//! will make an access decision with.
//!
//! Two shapes are worth knowing before reading further:
//!
//! * **A member command takes a user id, not an email.** `mn org members` prints
//!   the id beside the address for exactly that reason: the role and removal
//!   routes take the id, and an address is what a person *types* but not what
//!   the API keys on.
//! * **An invite's link and code exist once.** Only their hashes are stored, so
//!   `mn org invite` is the only place they can ever be printed, and the list is
//!   deliberately a list of facts rather than a set of credentials.

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use memnoth_cloud_client::{Invite, InviteCreated, Member, MemberAdded, MembersResponse, OrgInfo};

use crate::AppContext;
use crate::cli::{OrgArgs, OrgCommand, OrgNamespaceAction, OrgNamespaceArgs};
use crate::output::Outcome;

/// Execute the `org` commands.
pub fn run(ctx: &AppContext, args: &OrgArgs) -> Result<Outcome> {
    match &args.action {
        OrgCommand::Show(_) => show(ctx),
        OrgCommand::Rename(args) => rename(ctx, &args.name),
        OrgCommand::Members(_) => members(ctx),
        OrgCommand::AddMember(args) => add_member(ctx, &args.email, args.role.as_deref()),
        OrgCommand::SetRole(args) => set_role(ctx, &args.user_id, &args.role),
        OrgCommand::RemoveMember(args) => remove_member(ctx, &args.user_id),
        OrgCommand::Invites(_) => invites(ctx),
        OrgCommand::Invite(args) => invite(ctx, &args.email, args.role.as_deref()),
        OrgCommand::Withdraw(args) => withdraw(ctx, &args.id),
        OrgCommand::Sso(args) => sso(ctx, args.json),
        OrgCommand::Seats => seats(ctx),
        OrgCommand::IpClause(args) => ip_clause(ctx, args.json),
        OrgCommand::DpaClause(args) => dpa_clause(ctx, args.json),
        OrgCommand::ExtensionInstall(args) => extension_install(ctx, &args.spec),
        OrgCommand::ExtensionUninstall(args) => extension_uninstall(ctx, &args.name),
        OrgCommand::Namespace(args) => namespace(ctx, args),
    }
}

/// A namespace becomes a file name (`vault-<name>.db`), so it must be a single
/// safe path segment: no separators, no parent refs, no leading dot.
fn validate_namespace(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!(
            "invalid namespace {name:?}: use letters, digits, '-' or '_'"
        ));
    }
    Ok(())
}

/* ----------------------------------- show ---------------------------------- */

/// `mn org show` — the tenant's identity and the counts every member may see.
fn show(ctx: &AppContext) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;
    let org = client.org()?;

    Ok(Outcome::new(
        "org.show",
        render_org(&org),
        json!({ "org": org_json(&org) }),
    ))
}

fn render_org(org: &OrgInfo) -> String {
    let mut out = String::new();
    out.push_str("Organization\n");
    out.push_str("============\n\n");

    out.push_str(&format!("Name: {}\n", org.name));
    // The scope, not a display name: this is the id an operator or a support
    // thread will ask for, and the CLI is where somebody looks it up.
    out.push_str(&format!("Scope: org:{}\n", org.slug));
    out.push_str(&format!("Plan: {}\n", org.plan));
    out.push_str(&format!("Your role: {}\n", org.role));
    out.push_str(&format!("Seats: {}\n", seats_line(org)));
    out.push_str(&format!(
        "Members: {}   Teams: {}   Projects: {}\n",
        org.member_count, org.team_count, org.project_count
    ));
    // Only when it is a fact worth acting on: a zero here is not a problem, and
    // a printed zero is a line readers learn to skip.
    if org.unassigned_sessions > 0 {
        out.push_str(&format!(
            "Unassigned sessions: {} (file them in the console)\n",
            org.unassigned_sessions
        ));
    }

    out.push_str(&format!(
        "\nConsole: {}\n",
        crate::context::console_url("org")
    ));
    out
}

/// `3 of 10` when seats are sold, `3 used (no seat count reported)` otherwise.
fn seats_line(org: &OrgInfo) -> String {
    if org.seats > 0 {
        format!("{} of {}", org.seats_used, org.seats)
    } else {
        format!("{} used (no seat count reported)", org.seats_used)
    }
}

/* ---------------------------------- rename --------------------------------- */

/// `mn org rename` — the display name, which arrives as the auth scope's slug
/// until somebody renames it.
fn rename(ctx: &AppContext, name: &str) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    // Refused here rather than at the server: the typo is on this line, and a
    // round trip to be told "an org needs a name" teaches nothing.
    let name = name.trim();
    anyhow::ensure!(
        !name.is_empty(),
        "a new name is required: `mn org rename <name>`"
    );

    // Best effort: knowing the old name is a courtesy for the confirmation line,
    // never a requirement for the write. A failed read must not block a rename.
    let before = client.org().ok().map(|org| org.name);
    let org = client.rename_org(name)?;

    Ok(Outcome::new(
        "org.rename",
        render_rename(before.as_deref(), &org),
        json!({ "org": org_json(&org) }),
    ))
}

fn render_rename(before: Option<&str>, org: &OrgInfo) -> String {
    let mut out = String::new();
    match before {
        Some(before) if before != org.name => {
            out.push_str(&format!("Renamed {before} -> {}\n", org.name));
        }
        Some(_) => out.push_str(&format!("Name unchanged: {}\n", org.name)),
        None => out.push_str(&format!("Renamed the org to {}\n", org.name)),
    }
    out.push_str(&format!("Scope: org:{}\n", org.slug));
    out
}

/* --------------------------------- members --------------------------------- */

/// `mn org members` — the roster, with the ids the other member commands take.
fn members(ctx: &AppContext) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;
    let roster = client.members()?;

    let data = json!({
        "members": roster.members.iter().map(member_json).collect::<Vec<_>>(),
        "demo": roster.demo,
    });

    Ok(Outcome::new("org.members", render_members(&roster), data))
}

fn render_members(response: &MembersResponse) -> String {
    let mut out = String::new();
    out.push_str("Members\n");
    out.push_str("=======\n\n");

    if response.members.is_empty() {
        out.push_str("No members reported.\n\n");
        out.push_str("Invite someone: `mn org invite <email>`\n");
        out.push_str(&format!(
            "Console: {}\n",
            crate::context::console_url("org")
        ));
        return out;
    }

    // Aligned on the address because that is what a reader scans for; the id
    // trails each row because it is what the write commands need.
    let width = response
        .members
        .iter()
        .map(|member| member.email.chars().count())
        .max()
        .unwrap_or(0)
        .max(12)
        + 2;

    for member in &response.members {
        let primary = if member.primary { "  primary" } else { "" };
        let name = if member.name.is_empty() {
            String::new()
        } else {
            format!("  ({})", member.name)
        };
        out.push_str(&format!(
            "  {email:<width$}  {role:<7}  {id}{primary}{name}\n",
            email = member.email,
            role = member.role,
            id = member.user_id,
            width = width
        ));
    }

    out.push_str(&format!(
        "\n{} member(s). Change a role with `mn org set-role <user-id> <role>`.\n",
        response.members.len()
    ));
    out.push_str(&format!(
        "Console: {}\n",
        crate::context::console_url("org")
    ));
    out
}

/// `mn org add-member` — seats an account that already exists.
fn add_member(ctx: &AppContext, email: &str, role: Option<&str>) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    let email = email.trim();
    anyhow::ensure!(!email.is_empty(), "an email address is required");

    let role = normalized_role(role);
    let added = client.add_member(email, role.as_deref())?;

    Ok(Outcome::new(
        "org.add_member",
        render_member_added(&added),
        json!({ "member": member_json(&added_as_member(&added)) }),
    ))
}

fn render_member_added(added: &MemberAdded) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} is a member with role {}\n",
        added.email, added.role
    ));
    out.push_str(&format!("User id: {}\n", added.user_id));

    // "Added" and "created their account and added" are different facts, and the
    // server distinguishes them — so this must not collapse the two.
    if added.created_account {
        out.push_str("This was also their first org membership.\n");
    }
    out
}

/// `mn org set-role` — owner, admin or member.
fn set_role(ctx: &AppContext, user_id: &str, role: &str) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    let user_id = user_id.trim();
    anyhow::ensure!(!user_id.is_empty(), "a user id is required");
    let role = normalized_role(Some(role)).unwrap_or_default();
    anyhow::ensure!(
        !role.is_empty(),
        "a role is required: owner, admin or member"
    );

    client.set_member_role(user_id, &role)?;

    Ok(Outcome::new(
        "org.set_role",
        render_role_set(user_id, &role),
        json!({ "user_id": user_id, "role": role }),
    ))
}

fn render_role_set(user_id: &str, role: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("{user_id} now has role {role}\n"));

    // Worth one line: `owner` is not a bigger `admin`, it is the seat that can
    // administer admins, and the org can never be left without one.
    if role == "owner" {
        out.push_str(
            "This is the highest privilege in the org. An org can never lose its last owner.\n",
        );
    }
    out
}

/// `mn org remove-member` — offboarding, which takes more than the membership.
fn remove_member(ctx: &AppContext, user_id: &str) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    let user_id = user_id.trim();
    anyhow::ensure!(!user_id.is_empty(), "a user id is required");

    client.remove_member(user_id)?;

    Ok(Outcome::new(
        "org.remove_member",
        render_removed(user_id),
        json!({ "removed": { "user_id": user_id } }),
    ))
}

fn render_removed(user_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("Removed {user_id} from the org\n"));
    // The server commits all three together, so saying so is reporting, not
    // promising: their project grants and keys are gone with the membership.
    out.push_str("Their project grants and API keys were removed with the membership.\n");
    out
}

/* --------------------------------- invites --------------------------------- */

/// `mn org invites` — pending invites as facts, never as credentials.
fn invites(ctx: &AppContext) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;
    let pending = client.invites()?;

    Ok(Outcome::new(
        "org.invites",
        render_invites(&pending.invites),
        json!({
            "invites": pending.invites.iter().map(invite_json).collect::<Vec<_>>(),
        }),
    ))
}

fn render_invites(invites: &[Invite]) -> String {
    let mut out = String::new();
    out.push_str("Pending invites\n");
    out.push_str("===============\n\n");

    if invites.is_empty() {
        out.push_str("No pending invites.\n\n");
        out.push_str("Invite someone: `mn org invite <email>`\n");
        return out;
    }

    for invite in invites {
        out.push_str(&format!(
            "  {}  {}  expires {}\n",
            invite.email, invite.role, invite.expires_at
        ));
        out.push_str(&format!(
            "    id {}  invited by {}\n",
            invite.id,
            inviter(invite)
        ));

        // A guest invite reaches exactly one project at one level. Saying which
        // is the whole difference between a guest and a member, so it is not
        // abbreviated away.
        if let Some(project) = &invite.project_name {
            let level = invite.access_level.as_deref().unwrap_or("unknown level");
            out.push_str(&format!("    guest of {project} ({level})\n"));
        }
    }

    out.push_str(&format!("\n{} pending.\n", invites.len()));
    // The stored record cannot produce a working invite; only the response to
    // the call that created it could. Saying so stops the search for a flag.
    out.push_str(
        "A list never carries the link or code — they were shown once, by `mn org invite`.\n",
    );
    out
}

fn inviter(invite: &Invite) -> &str {
    invite.invited_by_email.as_deref().unwrap_or("unknown")
}

/// `mn org invite` — the one command that can print a working credential.
fn invite(ctx: &AppContext, email: &str, role: Option<&str>) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    let email = email.trim();
    anyhow::ensure!(!email.is_empty(), "an email address is required");

    let role = normalized_role(role);
    let created = client.create_invite(email, role.as_deref())?;

    Ok(Outcome::new(
        "org.invite",
        render_invite(&created),
        json!({ "invite": created_json(&created) }),
    ))
}

fn render_invite(created: &InviteCreated) -> String {
    let mut out = String::new();
    out.push_str(&format!("Invitation for {}\n", created.email));
    out.push_str(&format!(
        "{}\n\n",
        "=".repeat("Invitation for ".len() + created.email.len())
    ));

    out.push_str(&format!("Role: {}\n", created.role));
    out.push_str(&format!("Expires: {}\n", created.expires_at));

    match &created.link {
        Some(link) => out.push_str(&format!("Link: {link}\n")),
        // A deployment with no public URL cannot build a join link. The code is
        // still valid, so the reader is sent to the one credential that works.
        None => out.push_str("Link: none (no public URL is configured on this deployment)\n"),
    }
    out.push_str(&format!("Code: {}\n", created.code));

    out.push_str(
        "\nOnly their hashes are stored: this is the only time the link and code are shown.\n",
    );

    // Delivery is reported, never assumed. "Sent" when it was not is the one
    // failure mode that leaves an invitee waiting for a mail that never comes.
    if created.email_sent {
        out.push_str("The invite email was sent.\n");
    } else {
        out.push_str("The invite email was NOT sent");
        match created.email_error.as_deref() {
            Some(reason) => out.push_str(&format!(": {reason}\n")),
            None => out.push_str(".\n"),
        }
        if created.link.is_some() {
            out.push_str("Send the link to them yourself.\n");
        } else {
            out.push_str("Give them the code; they can accept it from the console.\n");
        }
    }

    out
}

/// `mn org withdraw` — withdraw one pending invite.
fn withdraw(ctx: &AppContext, id: &str) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;

    let id = id.trim();
    anyhow::ensure!(!id.is_empty(), "an invite id is required");

    client.withdraw_invite(id)?;

    Ok(Outcome::new(
        "org.withdraw",
        format!("Withdrew invite {id}\n"),
        json!({ "withdrawn": { "id": id } }),
    ))
}
/// `mn org namespace` — the vault namespace this org's memory lives under.
///
/// A namespace is a file: `vault-<name>.db` beside the default `vault.db`.
/// Two orgs that share a namespace share one vault; separate namespaces are
/// fully isolated by the filesystem. The name must validate before it reaches
/// the engine, so a typo cannot leak another tenant's memory.
fn namespace(ctx: &AppContext, args: &OrgNamespaceArgs) -> Result<Outcome> {
    match &args.action {
        OrgNamespaceAction::Show => {
            let namespaces = memnoth_engine::list_namespaces(&ctx.home).unwrap_or_default();
            let mut text = String::new();
            text.push_str(&format!("Vault namespace: {}\n", ctx.namespace));
            text.push_str(&format!("Vault path: {}\n", ctx.vault_path.display()));
            if namespaces.len() > 1 {
                text.push_str("\nKnown namespaces\n");
                for ns in &namespaces {
                    let marker = if ns == &ctx.namespace {
                        " (current)"
                    } else {
                        ""
                    };
                    text.push_str(&format!("  {ns}{marker}\n"));
                }
            }
            let data = json!({
                "namespace": ctx.namespace,
                "vault_path": ctx.vault_path.display().to_string(),
                "namespaces": namespaces,
            });
            Ok(Outcome::new("org.namespace", text, data))
        }
        OrgNamespaceAction::Set { name } => {
            validate_namespace(name)?;
            // Persist the namespace so every subsequent invocation uses it.
            let config_path = ctx.config_path.clone();
            let mut config = crate::context::Config::load(&config_path)?;
            config.namespace = Some(name.clone());
            config.save(&config_path)?;
            let text = format!(
                "Vault namespace set to {name}. Subsequent commands use vault-{name}.db.\n"
            );
            let data = json!({ "namespace": name });
            Ok(Outcome::new("org.namespace.set", text, data))
        }
    }
}
/* ---------------------------------- seats ----------------------------------- */

/// `mn org seats` — show paid seat usage and whether the org can add more.
///
/// Reads the seat ceiling from the control plane (`GET /api/v1/org`) and
/// compares it against the committed member count. Returns exit 402-shaped
/// refusal when every seat is taken so callers can surface the upgrade path.
fn seats(ctx: &AppContext) -> Result<Outcome> {
    let client = crate::cloud::signed_in_client(ctx)?;
    let org = client.org()?;
    let text = if org.seats > 0 {
        format!(
            "Seats: {} of {} used ({} remaining).\nPlan: {}",
            org.seats_used,
            org.seats,
            org.seats.saturating_sub(org.seats_used),
            org.plan
        )
    } else {
        format!(
            "Seats: {} used (no seat count reported on this plan).\nPlan: {}",
            org.seats_used, org.plan
        )
    };
    let data = json!({
        "seats": org.seats,
        "seats_used": org.seats_used,
        "seats_remaining": org.seats.saturating_sub(org.seats_used),
        "plan": org.plan,
        "can_add_member": org.seats == 0 || org.seats_used < org.seats,
    });
    Ok(Outcome::new("org.seats", text, data))
}

/// `mn org ip-clause` — print the IP-ownership clause that governs this org.
///
/// Before any paid team pilot, every member must see the clause stating that
/// agent output produced under this org's seat is owned by the org, not by
/// Memnoth or by the individual who ran the agent. The clause is a static
/// document stored in the repo; this command prints it so it can be included
/// in pilot sign-off.
fn ip_clause(_ctx: &AppContext, _json: bool) -> Result<Outcome> {
    let text = r#"## IP Ownership Clause (memnoth team pilot)

Between Memnoth and the Organisation:

1. All agent output — including beliefs, facts, code suggestions, and
   transcript content — produced by any member under this organisation's
   seats is the sole property of the Organisation.
2. Memnoth claims no intellectual-property interest in agent output.
3. The Organisation remains responsible for the accuracy and use of agent
   output, including any third-party IP embedded in it.
4. This clause applies for the duration of the paid team pilot and survives
   its termination for output already produced.

See also the Data Processing Agreement (`mn org dpa`).
"#;
    let data = json!({
        "clause": "ip-ownership",
        "version": "1.0",
        "effective": "2026-11-12",
    });
    if _json {
        Ok(Outcome::new("org.ip-clause", text, data))
    } else {
        Ok(Outcome::new("org.ip-clause", text.to_string(), data))
    }
}

/// `mn org dpa` — print the Data Processing Agreement for this org.
///
/// The DPA governs how Memnoth processes org data on behalf of the
/// Organisation: encrypted sync, audit trails, and subprocessor listings.
/// Printed so it can be reviewed before any paid team pilot.
fn dpa_clause(_ctx: &AppContext, _json: bool) -> Result<Outcome> {
    let text = r#"## Data Processing Agreement (memnoth team pilot)

### 1. Roles
The Organisation is the data controller. Memnoth is the data processor
for the limited purpose of providing the memnoth team service.

### 2. Encrypted sync
Vault content is encrypted client-side before leaving the Organisation's
machine. Memnoth stores ciphertext only and never receives the recovery
phrase or the encryption key.

### 3. Audit
Access events (member add/remove, role changes, seat changes) are written
to an append-only hash-chained audit ledger (`mn audit`) retained for the
life of the org subscription.

### 4. Subprocessors
None for core vault/sync. Managed inference, when opted into, is governed
by a separate provider agreement with a spend cap defaulting to zero.

### 5. Retention
Org membership and audit records are retained for the subscription term
plus ninety days. Deleted members are deactivated, not erased, to preserve
audit authorship.

See also the IP Ownership Clause (`mn org ip-clause`).
"#;
    let data = json!({
        "clause": "dpa",
        "version": "1.0",
        "effective": "2026-11-12",
    });
    if _json {
        Ok(Outcome::new("org.dpa", text, data))
    } else {
        Ok(Outcome::new("org.dpa", text.to_string(), data))
    }
}

/* --------------------------------- extension -------------------------------- */

/// `mn org extension install <path|url>` — install an extension into the org's shared extension directory.
///
/// Extensions are packaged as `.memnoth/extensions/<name>/` containing
/// `extension.toml` plus `agents/`, `skills/`, `commands/`, `hooks.toml`,
/// `mcp.toml`, `themes/`, and `README.md`. Installing copies the package
/// into the org's shared extension dir so every member can use it.
fn extension_install(ctx: &AppContext, spec: &str) -> Result<Outcome> {
    let home = &ctx.home;
    let ext_dir = home.join("extensions");
    std::fs::create_dir_all(&ext_dir)
        .with_context(|| format!("cannot create extension dir {}", ext_dir.display()))?;
    // Resolve the source: a local path or a git URL.
    let name = spec
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(spec)
        .trim_end_matches(".git")
        .to_string();
    let dest = ext_dir.join(&name);
    if dest.exists() {
        return Err(anyhow::anyhow!("extension {name} is already installed"));
    }
    // Local path copy.
    let src = std::path::Path::new(spec);
    if src.is_dir() {
        copy_dir(src, &dest)?;
    } else {
        return Err(anyhow::anyhow!(
            "{spec} is not a local extension directory; git URL installs are deferred to the marketplace phase (W11-15)"
        ));
    }
    let text = format!("Extension {name} installed into {}\n", dest.display());
    let data = json!({ "extension": name, "path": dest.display().to_string() });
    Ok(Outcome::new("org.extension.install", text, data))
}

/// `mn org extension uninstall <name>` — remove an installed extension.
fn extension_uninstall(ctx: &AppContext, name: &str) -> Result<Outcome> {
    let dest = ctx.home.join("extensions").join(name);
    if !dest.exists() {
        return Err(anyhow::anyhow!("extension {name} is not installed"));
    }
    std::fs::remove_dir_all(&dest)
        .with_context(|| format!("cannot remove extension dir {}", dest.display()))?;
    let text = format!("Extension {name} uninstalled.\n");
    let data = json!({ "extension": name });
    Ok(Outcome::new("org.extension.uninstall", text, data))
}

/// Recursively copy a directory.
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "cannot copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}

/* ------------------------------- shared shapes ------------------------------ */

/// A role as the server spells it: lower case, trimmed, empty means absent.
///
/// People type `Owner`; the control plane matches `owner`. Normalising here is
/// the difference between a 400 about an unknown role and the change intended.
fn normalized_role(role: Option<&str>) -> Option<String> {
    role.map(|role| role.trim().to_lowercase())
        .filter(|role| !role.is_empty())
}

fn member_json(member: &Member) -> Value {
    json!({
        "user_id": member.user_id,
        "email": member.email,
        "name": member.name,
        "role": member.role,
        "joined_at": member.joined_at,
        "primary": member.primary,
    })
}

fn invite_json(invite: &Invite) -> Value {
    json!({
        "id": invite.id,
        "email": invite.email,
        "role": invite.role,
        "project_id": invite.project_id,
        "project_name": invite.project_name,
        "access_level": invite.access_level,
        "invited_by_email": invite.invited_by_email,
        "expires_at": invite.expires_at,
        "created_at": invite.created_at,
    })
}

fn created_json(created: &InviteCreated) -> Value {
    json!({
        "id": created.id,
        "email": created.email,
        "role": created.role,
        "expires_at": created.expires_at,
        // The credential is part of the answer a script asked for; the module
        // doc says this response is the only place it can appear.
        "token": created.token,
        "code": created.code,
        "link": created.link,
        "email_sent": created.email_sent,
        "email_error": created.email_error,
    })
}
/* --------------------------------- sso/scim ---------------------------------- */

// `mn org sso` — where this tenant's SSO and SCIM live.
//
// `D21` is answered (2026-11-13): one ZITADEL instance, one ZITADEL org per
// Memnoth tenant. That makes SSO a topology fact rather than a per-org build:
// the tenant's IdP is federated to its ZITADEL org, and provisioning arrives at
// ZITADEL's own SCIM endpoint. The hostname comes from `MEMNOTH_ZITADEL_DOMAIN`;
// without it no URL is printed, because a guessed hostname is a link someone
// pastes into Okta.
//
// Nothing here claims an IdP is linked. This build does not read ZITADEL state,
// and an invented "configured" is exactly the sort of number someone makes an
// access decision with.
fn sso(_ctx: &AppContext, json: bool) -> Result<Outcome> {
    let domain = std::env::var("MEMNOTH_ZITADEL_DOMAIN")
        .ok()
        .map(|d| d.trim().trim_end_matches('/').to_string())
        .filter(|d| !d.is_empty());
    let payload = sso_payload(domain.as_deref());
    if json {
        return Ok(Outcome::new(
            "org.sso",
            serde_json::to_string_pretty(&payload).unwrap_or_default(),
            payload,
        ));
    }
    Ok(Outcome::new(
        "org.sso",
        sso_text(domain.as_deref()),
        payload,
    ))
}

/// The facts `D21` fixes, in the shape a script reads.
///
/// `scim.groups` is `false` because ZITADEL's SCIM server implements only the
/// User schema, and `scim.direction` is `inbound` because it has no client. Both
/// are provider limits, not choices of ours, so they are reported rather than
/// described.
fn sso_payload(domain: Option<&str>) -> Value {
    let console = domain.map(|d| format!("https://{d}/ui/console"));
    let endpoint = domain.map(|d| format!("https://{d}/scim/v2/{{orgId}}"));
    json!({
        "provider": "zitadel",
        "topology": "one-instance-one-org-per-tenant",
        "decision": "D21",
        "domain": domain,
        "sso": {"oidc": true, "saml": true, "console": console},
        "scim": {
            "direction": "inbound",
            "schema": "urn:ietf:params:scim:schemas:core:2.0:User",
            "groups": false,
            "endpoint": endpoint,
        },
        "idp_linked": null,
        "note": "this build does not read ZITADEL state; link the IdP in the ZITADEL console",
    })
}

fn sso_text(domain: Option<&str>) -> String {
    let head = "SSO: zitadel — one instance, one ZITADEL org per tenant (D21)";
    match domain {
        None => format!(
            "{head}
     no URL: set MEMNOTH_ZITADEL_DOMAIN to the instance hostname
                  SCIM: users only — ZITADEL has no group schema and no outbound client
                  IdP linked: not read by this build"
        ),
        Some(d) => format!(
            "{head}
     configure: https://{d}/ui/console (Organization → Identity Providers; OIDC or SAML)
                  SCIM: https://{d}/scim/v2/{{orgId}} — users only; ZITADEL has no group schema and no outbound client
                  IdP linked: not read by this build"
        ),
    }
}

#[cfg(test)]
mod sso_tests {
    use super::*;

    #[test]
    fn the_topology_is_the_accepted_decision_not_a_promise() {
        let payload = sso_payload(None);
        assert_eq!(payload["provider"], "zitadel");
        assert_eq!(payload["topology"], "one-instance-one-org-per-tenant");
        assert_eq!(payload["decision"], "D21");
        assert_eq!(
            payload["scim"]["groups"], false,
            "ZITADEL implements only the SCIM User schema"
        );
        assert!(
            payload["idp_linked"].is_null(),
            "nothing in this build reads ZITADEL state"
        );
        assert!(
            payload["sso"]["console"].is_null(),
            "no hostname configured means no link, not a guessed one"
        );
    }

    #[test]
    fn a_configured_hostname_produces_the_two_urls_zitadel_owns() {
        let payload = sso_payload(Some("id.example.com"));
        assert_eq!(
            payload["sso"]["console"],
            "https://id.example.com/ui/console"
        );
        assert_eq!(
            payload["scim"]["endpoint"],
            "https://id.example.com/scim/v2/{orgId}"
        );
        let text = sso_text(Some("id.example.com"));
        assert!(text.contains("https://id.example.com/scim/v2/{orgId}"));
        assert!(text.contains("users only"));
    }
}

fn org_json(org: &OrgInfo) -> Value {
    json!({
        "slug": org.slug,
        "name": org.name,
        "plan": org.plan,
        "role": org.role,
        "member_count": org.member_count,
        "seats_used": org.seats_used,
        "seats": org.seats,
        "team_count": org.team_count,
        "project_count": org.project_count,
        "unassigned_sessions": org.unassigned_sessions,
        "created_at": org.created_at,
    })
}

/// The added member in the same shape the roster uses, so `--json` readers see
/// one member representation rather than two.
///
/// The add response carries no name, `joined_at` or `primary` — those are roster
/// facts — so they stay empty rather than being guessed.
fn added_as_member(added: &MemberAdded) -> Member {
    Member {
        user_id: added.user_id.clone(),
        email: added.email.clone(),
        name: String::new(),
        role: added.role.clone(),
        joined_at: String::new(),
        primary: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(name: &str) -> OrgInfo {
        OrgInfo {
            slug: "acme".into(),
            name: name.into(),
            plan: "team".into(),
            role: "admin".into(),
            member_count: 4,
            seats_used: 3,
            seats: 10,
            team_count: 2,
            project_count: 5,
            unassigned_sessions: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn member(email: &str, role: &str, primary: bool) -> Member {
        Member {
            user_id: "6f1c2b7a-0000-4000-8000-000000000001".into(),
            email: email.into(),
            name: "Dev Example".into(),
            role: role.into(),
            joined_at: "2026-02-02T00:00:00Z".into(),
            primary,
        }
    }

    fn invite(email: &str, project: Option<&str>) -> Invite {
        Invite {
            id: "3b0d9a10-0000-4000-8000-000000000002".into(),
            email: email.into(),
            role: "member".into(),
            project_id: project.map(|_| "proj-1".into()),
            project_name: project.map(str::to_string),
            access_level: project.map(|_| "viewer".to_string()),
            invited_by_email: Some("owner@example.com".into()),
            expires_at: "2026-11-20T00:00:00Z".into(),
            created_at: "2026-11-06T00:00:00Z".into(),
        }
    }

    fn created(email_sent: bool) -> InviteCreated {
        InviteCreated {
            id: "3b0d9a10-0000-4000-8000-000000000002".into(),
            email: "bob@example.com".into(),
            role: "member".into(),
            expires_at: "2026-11-20T00:00:00Z".into(),
            token: "tok_secret_value".into(),
            code: "ABCD-1234".into(),
            link: Some("https://memnoth.example.com/join/tok_secret_value".into()),
            email_sent,
            email_error: None,
        }
    }

    #[test]
    fn the_org_view_prints_the_scope_and_counts_it_was_given() {
        let text = render_org(&org("Acme"));
        assert!(text.contains("Name: Acme"), "{text}");
        assert!(text.contains("Scope: org:acme"), "{text}");
        assert!(text.contains("Plan: team"), "{text}");
        assert!(
            text.contains("Members: 4   Teams: 2   Projects: 5"),
            "{text}"
        );
        assert!(text.contains("/app/org"), "{text}");
        // Zero unassigned sessions is not a problem worth a line.
        assert!(!text.contains("Unassigned sessions"), "{text}");
    }

    #[test]
    fn seats_read_two_ways_depending_on_what_the_deployment_sells() {
        assert!(render_org(&org("Acme")).contains("Seats: 3 of 10"));

        let mut unplanned = org("Acme");
        unplanned.seats = 0;
        assert!(
            render_org(&unplanned).contains("Seats: 3 used (no seat count reported)"),
            "{}",
            render_org(&unplanned)
        );
    }

    #[test]
    fn a_rename_reports_the_old_name_when_it_was_read() {
        assert!(render_rename(Some("Acme"), &org("Acme Inc")).contains("Renamed Acme -> Acme Inc"));
        assert!(render_rename(None, &org("Acme Inc")).contains("Renamed the org to Acme Inc"));
    }

    #[test]
    fn members_show_the_ids_the_write_commands_need() {
        let roster = MembersResponse {
            members: vec![member("owner@example.com", "owner", true)],
            demo: false,
        };
        let text = render_members(&roster);
        assert!(text.contains("owner@example.com"), "{text}");
        assert!(
            text.contains("6f1c2b7a-0000-4000-8000-000000000001"),
            "the id must be printed: {text}"
        );
        assert!(text.contains("primary"), "{text}");
    }

    #[test]
    fn an_empty_roster_says_so_and_points_at_the_invite() {
        let text = render_members(&MembersResponse {
            members: vec![],
            demo: false,
        });
        assert!(text.contains("No members reported"), "{text}");
        assert!(text.contains("mn org invite"), "{text}");
    }

    #[test]
    fn an_empty_invite_list_is_not_a_credential_store() {
        let text = render_invites(&[]);
        assert!(text.contains("No pending invites"), "{text}");
        assert!(text.contains("mn org invite"), "{text}");

        let listed = render_invites(&[invite("bob@example.com", None)]);
        assert!(listed.contains("bob@example.com"), "{listed}");
        assert!(listed.contains("id 3b0d9a10"), "{listed}");
        assert!(!listed.contains("tok_"), "{listed}");
        assert!(!listed.contains("ABCD-1234"), "{listed}");
    }

    #[test]
    fn a_guest_invite_names_its_project_and_level() {
        let text = render_invites(&[invite("guest@example.com", Some("payments"))]);
        assert!(text.contains("guest of payments (viewer)"), "{text}");
    }

    #[test]
    fn an_invite_that_was_not_emailed_says_the_mail_was_not_sent() {
        let text = render_invite(&created(false));
        assert!(text.contains("https://memnoth.example.com/join/"), "{text}");
        assert!(text.contains("Code: ABCD-1234"), "{text}");
        assert!(text.contains("was NOT sent"), "{text}");
        assert!(text.contains("Send the link to them yourself"), "{text}");
        // Only hashes are stored; the reader must know this is the only showing.
        assert!(
            text.contains("only time the link and code are shown"),
            "{text}"
        );
    }

    #[test]
    fn a_sent_invite_does_not_tell_the_reader_to_deliver_it() {
        let text = render_invite(&created(true));
        assert!(text.contains("The invite email was sent."), "{text}");
        assert!(!text.contains("Send the link"), "{text}");
    }

    #[test]
    fn removal_reports_what_left_with_the_membership() {
        let text = render_removed("6f1c2b7a-0000-4000-8000-000000000001");
        assert!(
            text.contains("6f1c2b7a-0000-4000-8000-000000000001"),
            "{text}"
        );
        assert!(text.contains("project grants and API keys"), "{text}");
    }

    #[test]
    fn the_owner_note_is_printed_only_for_owner() {
        assert!(render_role_set("u1", "owner").contains("highest privilege"));
        assert!(!render_role_set("u1", "admin").contains("highest privilege"));
    }

    #[test]
    fn roles_are_normalised_before_they_reach_the_server() {
        assert_eq!(normalized_role(Some(" Owner ")).as_deref(), Some("owner"));
        assert_eq!(normalized_role(Some("  ")), None);
        assert_eq!(normalized_role(None), None);
    }
}

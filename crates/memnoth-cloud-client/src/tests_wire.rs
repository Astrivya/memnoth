//! Wire-shape regression tests: the client must parse what the control plane
//! actually sends.
//!
//! Every fixture below is a literal response body copied from a server type —
//! `memnoth-cloud`'s `src/types.rs` / `src/types/admin.rs` — not a prettified
//! example. That is the point: the field names, the value types and which
//! fields are nullable are the contract, so a rename on either side fails here
//! rather than at a user's first `mn credits` or `mn auth login`.
//!
//! The crate's *used* `UsageResponse` lives in `lib.rs`; `crate::types` still
//! carries an older `UsageResponse` (`period_start` / `period_end`) that nothing
//! constructs, so the usage shapes are imported from the crate root explicitly
//! instead of through a glob that would pick the stale one.

use crate::types::{
    ApiKeyInfo, ApprovedAccountKey, CreateKeyResponse, CreditGrant, CreditLedgerEntry,
    CreditsResponse, Invite, InviteCreated, InvitesResponse, KeysResponse, McpTokenResponse, Me,
    Member, MemberAdded, MembersResponse, OrgEnvelope, OrgInfo, OrganizationInfo,
    OrganizationsResponse, ProjectInfo, ProjectsResponse, Team, TeamDetail, TeamEnvelope,
    TeamsResponse,
};
use crate::{ModelRow, UsageResponse};

/// `POST /api/v1/auth/token` answers with an account key, not an OAuth envelope.
///
/// This is the sign-in completing: `access_token` / `token_type` were invented
/// by the client and never sent, so every approved device flow failed to parse.
#[test]
fn the_device_token_body_is_the_account_contract_not_an_oauth_envelope() {
    let body = r#"{
        "api_key": "mn_live_9f8e7d6c5b4a",
        "account_id": "3124567890",
        "email": "dev@example.com",
        "tier": "none",
        "status": "active"
    }"#;

    let key: ApprovedAccountKey =
        serde_json::from_str(body).expect("the server's ApprovedAccountKey must parse");

    assert_eq!(key.api_key, "mn_live_9f8e7d6c5b4a");
    assert_eq!(key.account_id, "3124567890");
    assert_eq!(key.email, "dev@example.com");
    assert_eq!(key.tier, "none");
    assert_eq!(key.status, "active");
}

/// `GET /api/v1/me` reports a subject, an org scope and roles — and nothing else.
///
/// A key with no subject and no org is a normal answer, so both must stay
/// optional; requiring them would refuse a valid deployment.
#[test]
fn me_carries_roles_and_tolerates_a_key_without_subject_or_org() {
    let full: Me = serde_json::from_str(
        r#"{"sub": "3124567890", "org": "org:acme", "role": "admin", "roles": ["admin", "member"]}"#,
    )
    .expect("/me with a subject and org must parse");
    assert_eq!(full.sub.as_deref(), Some("3124567890"));
    assert_eq!(full.org.as_deref(), Some("org:acme"));
    assert_eq!(full.role, "admin");
    assert_eq!(full.roles, vec!["admin".to_string(), "member".to_string()]);

    let bare: Me = serde_json::from_str(r#"{"role": "member", "roles": []}"#)
        .expect("/me without sub/org must parse");
    assert_eq!(bare.sub, None);
    assert_eq!(bare.org, None);
    assert_eq!(bare.role, "member");
    assert!(bare.roles.is_empty());
}

/// `GET /api/v1/org` is wrapped, and the org's identity is its `slug`.
#[test]
fn the_org_read_is_an_envelope_and_has_no_opaque_id() {
    let body = r#"{
        "org": {
            "slug": "acme",
            "name": "Acme Inc",
            "plan": "team",
            "role": "admin",
            "member_count": 4,
            "seats_used": 3,
            "seats": 5,
            "team_count": 2,
            "project_count": 6,
            "unassigned_sessions": 1,
            "created_at": "2026-10-01T09:30:00Z"
        }
    }"#;

    let envelope: OrgEnvelope = serde_json::from_str(body).expect("/org envelope must parse");
    let org: &OrgInfo = &envelope.org;

    assert_eq!(org.slug, "acme");
    assert_eq!(org.name, "Acme Inc");
    assert_eq!(org.plan, "team");
    assert_eq!(org.role, "admin");
    assert_eq!(org.member_count, 4);
    assert_eq!(org.seats_used, 3);
    assert_eq!(org.seats, 5);
    assert_eq!(org.team_count, 2);
    assert_eq!(org.project_count, 6);
    assert_eq!(org.unassigned_sessions, 1);
    assert_eq!(org.created_at, "2026-10-01T09:30:00Z");
}

/// `GET /api/v1/credits` is a bucket summary, not a balance plus auto-recharge.
///
/// The response carries a live grant and an append-only ledger (with a signed
/// debit), plus the expiry window the console shouts about.
#[test]
fn credits_report_buckets_grants_and_a_signed_ledger() {
    let body = r#"{
        "balance_micros": 4500000,
        "burned_micros": 500000,
        "window_days": 30,
        "grants": [
            {
                "id": "grant_01",
                "amount_micros": 5000000,
                "remaining_micros": 4500000,
                "source": "grant",
                "note": "launch credit",
                "granted_by": "ops@example.com",
                "expires_at": "2026-12-01T00:00:00Z",
                "created_at": "2026-10-01T00:00:00Z"
            }
        ],
        "ledger": [
            {
                "id": "ledger_02",
                "kind": "debit",
                "amount_micros": -500000,
                "balance_after_micros": 4500000,
                "reason": "managed inference",
                "ref_id": "turn:chatcmpl-abc",
                "actor": "system",
                "created_at": "2026-11-05T12:00:00Z"
            },
            {
                "id": "ledger_01",
                "kind": "grant",
                "amount_micros": 5000000,
                "balance_after_micros": 5000000,
                "reason": "launch credit",
                "ref_id": "",
                "actor": "ops@example.com",
                "created_at": "2026-10-01T00:00:00Z"
            }
        ],
        "unpriced_sessions": 2,
        "expiring_micros": 1000000,
        "expiring_grants": 1,
        "next_expiry_at": "2026-12-01T00:00:00Z",
        "expiry_warning_days": 7,
        "grants_total": 1,
        "ledger_total": 2,
        "demo": false
    }"#;

    let credits: CreditsResponse = serde_json::from_str(body).expect("/credits must parse");

    assert_eq!(credits.balance_micros, 4_500_000);
    assert_eq!(credits.burned_micros, 500_000);
    assert_eq!(credits.window_days, 30);
    assert_eq!(credits.expiry_warning_days, 7);
    assert_eq!(credits.expiring_micros, 1_000_000);
    assert_eq!(credits.expiring_grants, 1);
    assert_eq!(
        credits.next_expiry_at.as_deref(),
        Some("2026-12-01T00:00:00Z")
    );
    assert_eq!(credits.grants_total, 1);
    assert_eq!(credits.ledger_total, 2);
    assert_eq!(credits.unpriced_sessions, 2);
    assert!(!credits.demo);

    let grant: &CreditGrant = &credits.grants[0];
    assert_eq!(grant.id, "grant_01");
    assert_eq!(grant.amount_micros, 5_000_000);
    assert_eq!(grant.remaining_micros, 4_500_000);
    assert_eq!(grant.source, "grant");
    assert_eq!(grant.note, "launch credit");
    assert_eq!(grant.granted_by, "ops@example.com");
    assert_eq!(grant.expires_at.as_deref(), Some("2026-12-01T00:00:00Z"));

    let debit: &CreditLedgerEntry = &credits.ledger[0];
    assert_eq!(debit.kind, "debit");
    assert_eq!(debit.amount_micros, -500_000);
    assert_eq!(debit.balance_after_micros, Some(4_500_000));
    assert_eq!(debit.ref_id, "turn:chatcmpl-abc");

    // A grant that never expires is `null`, not a sentinel date.
    let never: CreditGrant = serde_json::from_str(
        r#"{"id":"g2","amount_micros":1,"remaining_micros":1,"source":"promo","note":"","granted_by":"","expires_at":null,"created_at":"2026-10-01T00:00:00Z"}"#,
    )
    .expect("a grant with no expiry must parse");
    assert_eq!(never.expires_at, None);
}

/// `GET /api/v1/orgs` lists memberships, one of which is `current`.
#[test]
fn the_org_switcher_rows_are_memberships_not_org_records() {
    let body = r#"{
        "orgs": [
            {"slug": "acme", "name": "Acme Inc", "role": "admin", "plan": "team", "member_count": 4, "current": true},
            {"slug": "labs", "name": "Acme Labs", "role": "member", "plan": "free", "member_count": 1, "current": false}
        ]
    }"#;

    let orgs: OrganizationsResponse = serde_json::from_str(body).expect("/orgs must parse");
    let first: &OrganizationInfo = &orgs.orgs[0];

    assert_eq!(orgs.orgs.len(), 2);
    assert_eq!(first.slug, "acme");
    assert_eq!(first.plan, "team");
    assert_eq!(first.member_count, 4);
    assert!(first.current);
    assert!(!orgs.orgs[1].current);
}

/// `GET /api/v1/projects` carries the caller's own access level.
#[test]
fn projects_carry_the_callers_access_level_not_a_settings_blob() {
    let body = r#"{
        "projects": [
            {
                "id": "11111111-2222-3333-4444-555555555555",
                "name": "payments",
                "visibility": "restricted",
                "access_level": "developer",
                "member_count": 3,
                "repos": 2,
                "created_at": "2026-10-02T08:00:00Z"
            }
        ]
    }"#;

    let projects: ProjectsResponse = serde_json::from_str(body).expect("/projects must parse");
    let project: &ProjectInfo = &projects.projects[0];

    assert_eq!(project.name, "payments");
    assert_eq!(project.visibility, "restricted");
    assert_eq!(project.access_level, "developer");
    assert_eq!(project.member_count, 3);
    assert_eq!(project.repos, 2);
    assert_eq!(project.created_at, "2026-10-02T08:00:00Z");
}

/// `GET /api/v1/api-keys` is a list of metadata; every nullable field is null
/// on a fresh, org-wide, never-used key.
#[test]
fn api_keys_never_carry_a_secret_and_allow_null_metadata() {
    let body = r#"{
        "keys": [
            {
                "id": "key_01",
                "name": "laptop",
                "prefix": "mn_live_1a2b3c4d",
                "scope": "org",
                "project_id": null,
                "created_at": "2026-10-03T10:00:00Z",
                "last_used_at": null,
                "revoked_at": null,
                "expires_at": null
            }
        ]
    }"#;

    let keys: KeysResponse = serde_json::from_str(body).expect("/api-keys must parse");
    let key: &ApiKeyInfo = &keys.keys[0];

    assert_eq!(key.id, "key_01");
    assert_eq!(key.name, "laptop");
    assert_eq!(key.prefix, "mn_live_1a2b3c4d");
    assert_eq!(key.scope, "org");
    assert_eq!(key.project_id, None);
    assert_eq!(key.last_used_at, None);
    assert_eq!(key.revoked_at, None);
    assert_eq!(key.expires_at, None);
}

/// `POST /api/v1/api-keys` is the one body that carries the plaintext.
#[test]
fn a_minted_key_response_carries_the_secret_exactly_once() {
    let body = r#"{
        "key": {
            "id": "key_02",
            "name": "ci",
            "prefix": "mn_live_9f8e7d6c",
            "scope": "org",
            "project_id": null,
            "created_at": "2026-11-06T09:00:00Z",
            "last_used_at": null,
            "revoked_at": null,
            "expires_at": null
        },
        "token": "mn_live_9f8e7d6c5b4a39281706f5e4d3c2b1a0"
    }"#;

    let created: CreateKeyResponse = serde_json::from_str(body).expect("created key must parse");

    assert_eq!(created.key.id, "key_02");
    assert_eq!(created.key.name, "ci");
    assert_eq!(created.token, "mn_live_9f8e7d6c5b4a39281706f5e4d3c2b1a0");
}

/// `POST /api/v1/mcp/token` names an endpoint and an expiry, not a URL and a TTL.
#[test]
fn an_mcp_token_is_endpoint_and_expiry() {
    let body = r#"{
        "token": "eyJhbGciOiJIUzI1NiJ9.payload.signature",
        "endpoint": "https://memnoth.astrivya.ai/mcp",
        "expires_at": "2026-11-06T21:00:00Z"
    }"#;

    let token: McpTokenResponse = serde_json::from_str(body).expect("/mcp/token must parse");

    assert_eq!(token.token, "eyJhbGciOiJIUzI1NiJ9.payload.signature");
    assert_eq!(token.endpoint, "https://memnoth.astrivya.ai/mcp");
    assert_eq!(token.expires_at, "2026-11-06T21:00:00Z");
}

/// `GET /api/v1/usage` is totals + series + leaderboard + per-model rows.
#[test]
fn usage_reports_totals_series_and_a_per_model_row() {
    let body = r#"{
        "range": "30d",
        "demo": false,
        "totals": {"tokens_in": 2610, "tokens_out": 790, "tasks": 2, "cost_usd": 0.0042},
        "series": [
            {"day": "2026-11-05", "tokens_in": 2610, "tokens_out": 790, "cost_usd": 0.0042, "tasks": 2}
        ],
        "leaderboard": [
            {"email": "dev@example.com", "name": "Dev", "spend_usd": 0.0042, "tokens": 3400, "tasks": 2}
        ],
        "models": [
            {"provider": "kilo", "model": "kilo-auto/free", "tokens": 3400, "cost_usd": 0.0}
        ]
    }"#;

    let usage: UsageResponse = serde_json::from_str(body).expect("/usage must parse");

    assert_eq!(usage.range, "30d");
    assert!(!usage.demo);
    assert_eq!(usage.totals.tokens_in, 2610);
    assert_eq!(usage.totals.tokens_out, 790);
    assert_eq!(usage.totals.tasks, 2);
    assert!((usage.totals.cost_usd - 0.0042).abs() < 1e-12);

    assert_eq!(usage.series.len(), 1);
    assert_eq!(usage.series[0].day, "2026-11-05");
    assert_eq!(usage.leaderboard[0].email, "dev@example.com");

    let row: &ModelRow = &usage.models[0];
    assert_eq!(row.provider, "kilo");
    assert_eq!(row.model, "kilo-auto/free");
    assert_eq!(row.tokens, 3400);
    assert!((row.cost_usd - 0.0).abs() < f64::EPSILON);
}

/// `GET /api/v1/members` is the admin roster: a `members` array plus `demo`.
///
/// `user_id` is the id every member write takes, so it must be a string here
/// even though the server stores a uuid — the CLI never parses it.
#[test]
fn the_member_roster_parses_with_the_ids_the_write_routes_take() {
    let body = r#"{
        "members": [
            {
                "user_id": "6f1c2b7a-0000-4000-8000-000000000001",
                "email": "owner@example.com",
                "name": "Owner Example",
                "role": "owner",
                "joined_at": "2026-01-04T12:00:00Z",
                "primary": true
            }
        ],
        "demo": false
    }"#;

    let roster: MembersResponse = serde_json::from_str(body).expect("/members must parse");
    let member: &Member = &roster.members[0];

    assert_eq!(member.user_id, "6f1c2b7a-0000-4000-8000-000000000001");
    assert_eq!(member.email, "owner@example.com");
    assert_eq!(member.name, "Owner Example");
    assert_eq!(member.role, "owner");
    assert_eq!(member.joined_at, "2026-01-04T12:00:00Z");
    assert!(member.primary);
    assert!(!roster.demo);
}

/// `POST /api/v1/orgs/members` reports four fields and no roster detail: the
/// account's name and join date are roster facts the add never sees.
#[test]
fn an_added_member_reports_the_role_and_whether_the_account_was_new() {
    let body = r#"{
        "user_id": "6f1c2b7a-0000-4000-8000-000000000001",
        "email": "dev@example.com",
        "role": "member",
        "created_account": true
    }"#;

    let added: MemberAdded = serde_json::from_str(body).expect("an added member must parse");

    assert_eq!(added.user_id, "6f1c2b7a-0000-4000-8000-000000000001");
    assert_eq!(added.email, "dev@example.com");
    assert_eq!(added.role, "member");
    assert!(added.created_account);
}

/// `GET /api/v1/orgs/invites` is facts about pending invites, never a token.
///
/// An org invite leaves the three project fields null; a guest invite fills all
/// three, which is the only thing that distinguishes them on this route.
#[test]
fn pending_invites_allow_null_project_fields_and_carry_no_credential() {
    let body = r#"{
        "invites": [
            {
                "id": "3b0d9a10-0000-4000-8000-000000000002",
                "email": "bob@example.com",
                "role": "member",
                "project_id": null,
                "project_name": null,
                "access_level": null,
                "invited_by_email": "owner@example.com",
                "expires_at": "2026-11-20T00:00:00Z",
                "created_at": "2026-11-06T00:00:00Z"
            },
            {
                "id": "3b0d9a10-0000-4000-8000-000000000003",
                "email": "guest@example.com",
                "role": "guest",
                "project_id": "proj-1",
                "project_name": "payments",
                "access_level": "viewer",
                "invited_by_email": null,
                "expires_at": "2026-11-20T00:00:00Z",
                "created_at": "2026-11-06T00:00:00Z"
            }
        ]
    }"#;

    let pending: InvitesResponse = serde_json::from_str(body).expect("/orgs/invites must parse");
    assert_eq!(pending.invites.len(), 2);

    let org_invite: &Invite = &pending.invites[0];
    assert_eq!(org_invite.role, "member");
    assert_eq!(org_invite.project_id, None);
    assert_eq!(org_invite.project_name, None);
    assert_eq!(org_invite.access_level, None);

    let guest: &Invite = &pending.invites[1];
    assert_eq!(guest.role, "guest");
    assert_eq!(guest.project_name.as_deref(), Some("payments"));
    assert_eq!(guest.access_level.as_deref(), Some("viewer"));
    assert_eq!(guest.invited_by_email, None);
}

/// `POST /api/v1/orgs/invites` is the one response with the token and the code,
/// and `link` is null on a deployment with no public URL while the code still
/// works — so the two are separate fields, not one credential.
#[test]
fn a_created_invite_carries_the_link_and_code_once_and_reports_delivery() {
    let body = r#"{
        "id": "3b0d9a10-0000-4000-8000-000000000002",
        "email": "bob@example.com",
        "role": "member",
        "expires_at": "2026-11-20T00:00:00Z",
        "token": "tok_9f8e7d6c5b4a",
        "code": "ABCD-1234",
        "link": null,
        "email_sent": false,
        "email_error": "MEMNOTH_CLOUD_PUBLIC_URL is unset, so the join link cannot be built and no email was sent",
        "note": "This is the only time the token and the code are shown"
    }"#;

    let created: InviteCreated = serde_json::from_str(body).expect("a created invite must parse");

    assert_eq!(created.id, "3b0d9a10-0000-4000-8000-000000000002");
    assert_eq!(created.email, "bob@example.com");
    assert_eq!(created.role, "member");
    assert_eq!(created.token, "tok_9f8e7d6c5b4a");
    assert_eq!(created.code, "ABCD-1234");
    assert_eq!(created.link, None);
    assert!(!created.email_sent);
    assert!(
        created
            .email_error
            .as_deref()
            .is_some_and(|reason| reason.contains("PUBLIC_URL")),
        "the delivery failure must survive parsing: {:?}",
        created.email_error
    );
}
/// `GET /api/v1/teams` — a row is the team plus the caller's two flags, and an
/// org with no teams is an empty list rather than an error.
#[test]
fn team_rows_carry_their_counts_and_the_callers_flags() {
    let body = r#"{
        "teams": [
            {
                "id": "4f2a9c10-0000-4000-8000-000000000003",
                "name": "platform",
                "project_count": 2,
                "member_count": 3,
                "is_member": true,
                "can_manage": true,
                "created_by": "6f1c2b7a-0000-4000-8000-000000000001",
                "created_by_email": "owner@example.com",
                "created_at": "2026-03-01T00:00:00Z"
            },
            {
                "id": "4f2a9c10-0000-4000-8000-000000000004",
                "name": "design",
                "project_count": 0,
                "member_count": 0,
                "is_member": false,
                "can_manage": false,
                "created_by": null,
                "created_by_email": null,
                "created_at": "2026-04-01T00:00:00Z"
            }
        ],
        "demo": false
    }"#;

    let response: TeamsResponse = serde_json::from_str(body).expect("/teams must parse");
    assert!(!response.demo);
    assert_eq!(response.teams.len(), 2);

    let mine: &Team = &response.teams[0];
    assert_eq!(mine.name, "platform");
    assert_eq!(mine.member_count, 3);
    assert_eq!(mine.project_count, 2);
    assert!(mine.is_member);
    assert!(mine.can_manage);

    // A team whose creator was deleted still lists: the two `created_by` fields
    // are optional because the account behind them may be gone.
    let orphan: &Team = &response.teams[1];
    assert_eq!(orphan.created_by, None);
    assert_eq!(orphan.created_by_email, None);
    assert!(!orphan.is_member);
}

/// `GET /api/v1/teams/{id}` — the team, its people and the grants that give it a
/// purpose. `can_manage` is on the detail as well, so the text a person reads and
/// the permission the write enforces are the server's single answer.
#[test]
fn a_team_detail_carries_its_members_the_org_roles_and_the_grants() {
    let body = r#"{
        "team": {
            "id": "4f2a9c10-0000-4000-8000-000000000003",
            "name": "platform",
            "project_count": 1,
            "member_count": 1,
            "is_member": false,
            "can_manage": false,
            "created_by": "6f1c2b7a-0000-4000-8000-000000000001",
            "created_by_email": "owner@example.com",
            "created_at": "2026-03-01T00:00:00Z"
        },
        "members": [
            {
                "user_id": "6f1c2b7a-0000-4000-8000-000000000009",
                "email": "dev@example.com",
                "name": "Dev Example",
                "org_role": "developer"
            }
        ],
        "projects": [
            { "id": "proj-1", "name": "payments", "level": "writer" }
        ],
        "can_manage": false
    }"#;

    let detail: TeamDetail = serde_json::from_str(body).expect("/teams/{id} must parse");

    assert_eq!(detail.team.name, "platform");
    assert!(!detail.can_manage);
    // The org role is the member's standing outside this team; a team grants none.
    assert_eq!(detail.members[0].org_role, "developer");
    assert_eq!(detail.members[0].email, "dev@example.com");
    assert_eq!(detail.projects[0].level, "writer");
}

/// `POST /api/v1/teams` answers with an envelope; the client reads `team` out of
/// it, so the wrapper — not a bare row — is the contract.
#[test]
fn a_created_team_is_wrapped_and_starts_owned_by_its_creator() {
    let body = r#"{
        "team": {
            "id": "4f2a9c10-0000-4000-8000-000000000005",
            "name": "platform",
            "project_count": 0,
            "member_count": 1,
            "is_member": true,
            "can_manage": true,
            "created_by": "6f1c2b7a-0000-4000-8000-000000000001",
            "created_by_email": "owner@example.com",
            "created_at": "2026-11-06T00:00:00Z"
        }
    }"#;

    let envelope: TeamEnvelope = serde_json::from_str(body).expect("a created team must parse");

    assert_eq!(envelope.team.name, "platform");
    assert!(
        envelope.team.can_manage,
        "the creator owns the team they just made"
    );
    assert!(envelope.team.is_member);
}

/// `POST /api/v1/sessions` is the dashboard's only source of sessions.
///
/// This is the shape the console's Sessions page and its usage rollup read, so
/// the numbers have to leave as numbers: a `tokens` that serialised as a string
/// would be a row the page renders as zero, and a missing `ended_at` would be a
/// session the usage charts never count. Written as a serialisation test rather
/// than a parse test because this direction is the one that was broken — the
/// client sent a shape the plane had no field for.
#[test]
fn the_session_row_is_the_shape_the_console_reads() {
    use crate::types::SessionReport;

    let report = SessionReport {
        id: "20261107-120000-4242".to_string(),
        name: "session".to_string(),
        model: "memnoth/auto".to_string(),
        status: "finished".to_string(),
        tokens: 1240,
        files: 3,
        cost_micros: 2500,
        workspace: "/home/dev/project".to_string(),
        started_at: Some("2026-11-07T12:00:00.000Z".to_string()),
        ended_at: Some("2026-11-07T12:30:00.000Z".to_string()),
        ..SessionReport::default()
    };

    let body = serde_json::to_value(&report).expect("a session report must serialise");

    assert_eq!(body["id"], "20261107-120000-4242");
    assert_eq!(body["status"], "finished");
    assert!(body["tokens"].is_i64(), "tokens is a number, not a string");
    assert!(body["files"].is_i64());
    assert!(body["cost_micros"].is_i64());
    assert!(
        body["ended_at"].as_str().unwrap().ends_with('Z'),
        "ended_at is RFC 3339, which is the only shape the plane parses"
    );
    // Empty strings and absent options stay out of the body: the plane reads a
    // missing field as \"not reported\", a present empty one as a claim.
    assert!(body.get("repo").is_none());
    assert!(body.get("control_url").is_none());
    assert!(body.get("project_id").is_none());
}

/// The ingest envelope is `events`, and a replay is reported as `duplicates`.
///
/// The client used to send `{"items": [...]}`, a body the plane's `/ingest/batch`
/// route has never had, so every `mn sync` was refused before it stored
/// anything — while the command still printed a cheerful count.
#[test]
fn the_ingest_batch_envelope_is_events_not_items() {
    use crate::types::{BatchIngestRequest, BatchIngestResponse, IngestEvent};

    let batch = BatchIngestRequest {
        events: vec![IngestEvent {
            event_id: "evt-1".to_string(),
            kind: "session_opened".to_string(),
            surface: "tui".to_string(),
            session_id: Some("sess-1".to_string()),
            at_ms: 1_760_000_000_000,
            extra: serde_json::Map::new(),
        }],
    };

    let body = serde_json::to_value(&batch).expect("a batch must serialise");
    assert!(
        body.get("items").is_none(),
        "the plane has no `items` field"
    );
    assert_eq!(body["events"][0]["event_id"], "evt-1");
    assert_eq!(body["events"][0]["kind"], "session_opened");
    assert_eq!(body["events"][0]["surface"], "tui");
    assert_eq!(body["events"][0]["at_ms"], 1_760_000_000_000_i64);

    let answer: BatchIngestResponse = serde_json::from_str(r#"{"accepted": 2, "duplicates": 1}"#)
        .expect("the plane's ingest result must parse");
    assert_eq!(answer.accepted, 2);
    assert_eq!(answer.duplicates, 1);
}

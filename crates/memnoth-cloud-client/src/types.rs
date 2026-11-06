//! Response types for the memnoth cloud API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Device authorization response (POST /auth/device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAuthResponse {
    /// The device code to display to the user.
    pub device_code: String,
    /// The user code to enter on the authorization page.
    pub user_code: String,
    /// The verification URI (e.g., "https://memnoth.astrivya.ai/device").
    pub verification_uri: String,
    /// The complete verification URI with user code pre-filled.
    pub verification_uri_complete: String,
    /// Lifetime of the device code in seconds.
    pub expires_in: u64,
    /// Polling interval in seconds.
    pub interval: u64,
}

/// approved key (POST /auth/token) — the account contract.
///
/// Not an OAuth token envelope: the cloud mints an **API key** and reports the
/// identity behind it, which is why `email` and `account_id` are here and there
/// is no refresh token. The CLI used to parse `{access_token, token_type, …}`,
/// a shape the control plane never sent, so every completed sign-in failed to
/// deserialize. This is the server's `ApprovedAccountKey`, field for field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovedAccountKey {
    /// The secret. Returned exactly once, here.
    pub api_key: String,
    /// Stable account identifier.
    pub account_id: String,
    pub email: String,
    /// Wire tier. `none` on a self-hosted deployment.
    pub tier: String,
    pub status: String,
}

/// Device approval response (POST /auth/device/approve).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceApproveResponse {
    /// Whether the device was approved.
    pub approved: bool,
    /// The access token if approved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// Token type if approved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    /// Expires in seconds if approved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    /// Refresh token if approved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

/// Current caller, as a token sees itself (GET /me).
///
/// Four fields, because that is what the control plane answers: the account
/// subject, the org scope, and the role(s). It carries no email or profile — a
/// key is not a user session — so nothing here may assume one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Me {
    /// The account subject (`None` on a key with no subject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    /// The org scope this credential is bound to, e.g. `org:acme`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// The caller's primary role.
    #[serde(default)]
    pub role: String,
    /// Every role the caller holds.
    #[serde(default)]
    pub roles: Vec<String>,
}

/// `GET /api/v1/org` — the envelope the control plane wraps the org in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgEnvelope {
    pub org: OrgInfo,
}

/// The tenant's identity, as `GET /api/v1/org` reports it.
///
/// `slug` is the machine scope (`org:<slug>`), not an opaque id: there is no
/// separate org id on the wire, and the CLI used to expect one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgInfo {
    pub slug: String,
    pub name: String,
    pub plan: String,
    /// The caller's effective role in this org.
    pub role: String,
    #[serde(default)]
    pub member_count: i64,
    #[serde(default)]
    pub seats_used: i64,
    #[serde(default)]
    pub seats: i64,
    #[serde(default)]
    pub team_count: i64,
    #[serde(default)]
    pub project_count: i64,
    #[serde(default)]
    pub unassigned_sessions: i64,
    #[serde(default)]
    pub created_at: String,
}

/// Usage response (GET /usage).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageResponse {
    /// Current period start (RFC3339).
    pub period_start: String,
    /// Current period end (RFC3339).
    pub period_end: String,
    /// Total tokens used in period.
    pub tokens_used: u64,
    /// Total tokens limit for period.
    pub tokens_limit: u64,
    /// Total cost in micros (millionths of base currency).
    pub cost_micros: i64,
    /// Cost limit in micros.
    pub cost_limit_micros: i64,
    /// Breakdown by model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_model: Option<Vec<ModelUsage>>,
    /// Breakdown by project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_project: Option<Vec<ProjectUsage>>,
}

/// Per-model usage breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Model ID.
    pub model_id: String,
    /// Model name.
    pub model_name: String,
    /// Tokens used.
    pub tokens_used: u64,
    /// Cost in micros.
    pub cost_micros: i64,
    /// Request count.
    pub request_count: u64,
}

/// Per-project usage breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectUsage {
    /// Project ID.
    pub project_id: String,
    /// Project name.
    pub project_name: String,
    /// Tokens used.
    pub tokens_used: u64,
    /// Cost in micros.
    pub cost_micros: i64,
    /// Request count.
    pub request_count: u64,
}

/// Credits summary (GET /credits).
///
/// The control plane reports credit **buckets**: a balance, what burned in the
/// window, the grants still live and the ledger of what moved it. The client
/// previously required `currency` and `auto_recharge`, two fields this service
/// has never sent, so `mn credits` failed to deserialize on every account.
/// Every count is `default`ed so a deployment that grows a field does not break
/// an older CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditsResponse {
    /// Remaining micros across unexpired, unexhausted grants.
    pub balance_micros: i64,
    /// Consumed in the window, as a positive number.
    #[serde(default)]
    pub burned_micros: i64,
    /// The window `burned_micros` covers.
    #[serde(default)]
    pub window_days: i64,
    /// Live and exhausted grants, newest first.
    #[serde(default)]
    pub grants: Vec<CreditGrant>,
    /// Append-only ledger rows, newest first.
    #[serde(default)]
    pub ledger: Vec<CreditLedgerEntry>,
    /// Sessions that rolled up without a metered cost.
    #[serde(default)]
    pub unpriced_sessions: i64,
    /// Remaining micros expiring inside the warning window.
    #[serde(default)]
    pub expiring_micros: i64,
    /// How many buckets `expiring_micros` covers.
    #[serde(default)]
    pub expiring_grants: i64,
    /// The soonest expiry (RFC3339), when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_expiry_at: Option<String>,
    /// The window `expiring_micros` is measured over.
    #[serde(default)]
    pub expiry_warning_days: i64,
    /// Total grants, against the (possibly capped) `grants` page.
    #[serde(default)]
    pub grants_total: i64,
    /// Total ledger rows, against the (possibly capped) `ledger` page.
    #[serde(default)]
    pub ledger_total: i64,
    /// Whether the numbers came from a demo dataset.
    #[serde(default)]
    pub demo: bool,
}

/// One credit grant: a bucket with its own expiry and provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditGrant {
    pub id: String,
    pub amount_micros: i64,
    #[serde(default)]
    pub remaining_micros: i64,
    /// `grant` | `plan` | `promo` | `adjustment`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub granted_by: String,
    /// `None` = never expires.
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

/// One append-only ledger row. `amount_micros` is signed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditLedgerEntry {
    pub id: String,
    /// `grant` | `debit` | `adjustment` | `expiry`.
    #[serde(default)]
    pub kind: String,
    pub amount_micros: i64,
    #[serde(default)]
    pub balance_after_micros: Option<i64>,
    #[serde(default)]
    pub reason: String,
    /// The session id for a debit; empty otherwise.
    #[serde(default)]
    pub ref_id: String,
    #[serde(default)]
    pub actor: String,
    #[serde(default)]
    pub created_at: String,
}

/// Model list response (GET /models).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsResponse {
    /// Available models.
    pub models: Vec<ModelInfo>,
}

/// Model information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Provider (e.g., "openai", "anthropic", "memnoth").
    pub provider: String,
    /// Model family (e.g., "gpt-4", "claude-3").
    pub family: String,
    /// Context window size.
    pub context_window: u32,
    /// Max output tokens.
    pub max_output_tokens: u32,
    /// Cost per 1M input tokens in micros.
    pub input_cost_per_million_micros: i64,
    /// Cost per 1M output tokens in micros.
    pub output_cost_per_million_micros: i64,
    /// Whether the model supports streaming.
    pub supports_streaming: bool,
    /// Whether the model supports tools/function calling.
    pub supports_tools: bool,
    /// Whether the model supports vision.
    pub supports_vision: bool,
    /// Model capabilities tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Whether the model is deprecated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    /// Deprecation date (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecation_date: Option<String>,
}

/// Project list response (GET /projects).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsResponse {
    /// Projects.
    pub projects: Vec<ProjectInfo>,
}

/// `POST /api/v1/projects`, `PATCH /api/v1/projects/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEnvelope {
    pub project: ProjectInfo,
}

/// An org's project, as the control plane reports it.
///
/// The CLI used to expect a description and a settings blob; the server has
/// neither. What it does carry is the caller's own access level and how many
/// people reach the project, which is what a CLI can act on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    /// `org` | `restricted`.
    #[serde(default)]
    pub visibility: String,
    /// The caller's effective level (`viewer` | `developer` | `admin`).
    #[serde(default)]
    pub access_level: String,
    #[serde(default)]
    pub member_count: i64,
    #[serde(default)]
    pub repos: i32,
    #[serde(default)]
    pub created_at: String,
}

/// Project settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// Default model for this project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Whether ingestion is enabled.
    pub ingestion_enabled: bool,
    /// Retention days for session data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
    /// Custom metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// `GET /api/v1/orgs` — the org switcher's rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationsResponse {
    #[serde(default)]
    pub orgs: Vec<OrganizationInfo>,
}

/// One org the caller belongs to, as `GET /api/v1/orgs` reports it.
///
/// There is no opaque org id on the wire: `slug` is the machine scope, and
/// `current` says which one a browser session has selected (always false for a
/// key, which is bound to the org it was minted in).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationInfo {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub member_count: i64,
    #[serde(default)]
    pub current: bool,
}

/// `POST /api/v1/auth/session/org` — the selection after a switch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgSwitch {
    pub slug: String,
    #[serde(default)]
    pub role: String,
}

/// MCP token (POST /mcp/token).
///
/// `endpoint` and `expires_at` are the server's names; the CLI used to read
/// `mcp_url` / `expires_in`, so the hosted-MCP path never parsed a minted token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTokenResponse {
    /// The signed bearer. Never stored server-side.
    pub token: String,
    /// The MCP endpoint the token is valid at.
    pub endpoint: String,
    /// When it stops working (RFC3339).
    pub expires_at: String,
}

/// Ingest request (POST /ingest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
    /// Session ID.
    pub session_id: String,
    /// Workspace path.
    pub workspace: String,
    /// Model used.
    pub model: String,
    /// Tokens used (input + output).
    pub tokens: u64,
    /// Files touched.
    pub files: u64,
    /// Cost in micros.
    pub cost_micros: i64,
    /// Event type: "created" | "updated" | "closed".
    pub event_type: String,
    /// Timestamp (RFC3339).
    pub timestamp: String,
    /// Optional project ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Ingest response (POST /ingest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResponse {
    /// Whether the ingest was accepted.
    pub accepted: bool,
    /// Ingest ID for tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_id: Option<String>,
}
/// Batch ingest request (POST /api/v1/ingest/batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchIngestRequest {
    /// Array of ingest requests.
    pub items: Vec<IngestRequest>,
    /// Dry run flag - don't actually persist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
}

/// Batch ingest response (POST /api/v1/ingest/batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchIngestResponse {
    /// Whether the batch was accepted.
    pub accepted: bool,
    /// Batch ID for tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    /// Number of items accepted.
    pub accepted_count: usize,
    /// Number of items rejected.
    pub rejected_count: usize,
    /// Per-item results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<BatchIngestItemResult>>,
}

/// Per-item result in batch ingest response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchIngestItemResult {
    /// Index of the item in the request.
    pub index: usize,
    /// Whether this item was accepted.
    pub accepted: bool,
    /// Ingest ID if accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_id: Option<String>,
    /// Error message if rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `GET /api/v1/api-keys`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeysResponse {
    #[serde(default)]
    pub keys: Vec<ApiKeyInfo>,
}

/// One API key, as the control plane reports it. Never the secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Display form of the token, e.g. `mn_live_1a2b3c4d…`.
    #[serde(default)]
    pub prefix: String,
    /// One scope, not a list: the server models a key's authority as one value.
    #[serde(default)]
    pub scope: String,
    /// The project this key is bound to; `None` is an org-wide key.
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// Set when revoked. A revoked key stays in the list, visibly dead.
    #[serde(default)]
    pub revoked_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// `POST /api/v1/api-keys` — the only body that ever carries a secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKeyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Relative deadline, resolved server-side. May not be combined with `expires_at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<i64>,
}

/// `POST /api/v1/api-keys` response: the key, plus the plaintext once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKeyResponse {
    pub key: ApiKeyInfo,
    /// The secret. Present in this response and nowhere else, ever.
    pub token: String,
}

/// `POST /api/v1/api-keys/{id}/revoke`, and the bulk form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeKeyResponse {
    #[serde(default)]
    pub revoked: u64,
}

/// A group of people inside an org (`GET /api/v1/teams`).
///
/// A team decides **who shares work with whom**; it never grants a role, which
/// is why there is no `role` field. `can_manage` is the server's own answer to
/// "may this caller rename, delete or change members", so a command can gate the
/// action on it instead of guessing from the org role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub project_count: i64,
    #[serde(default)]
    pub member_count: i64,
    /// Whether the caller is in this team.
    #[serde(default)]
    pub is_member: bool,
    #[serde(default)]
    pub can_manage: bool,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub created_by_email: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

/// `GET /api/v1/teams`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsResponse {
    #[serde(default)]
    pub teams: Vec<Team>,
    #[serde(default)]
    pub demo: bool,
}

/// One person in a team, with the org role that decides what they may do
/// elsewhere. Visible to org admins and to that team's own members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub user_id: String,
    pub email: String,
    #[serde(default)]
    pub name: String,
    /// `owner` | `admin` | `member` — the *org* role, not a team role.
    #[serde(default)]
    pub org_role: String,
}

/// A project a team has a grant on, with the level of that grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamProject {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub level: String,
}

/// `GET /api/v1/teams/{id}` — one team, its people, its projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamDetail {
    pub team: Team,
    #[serde(default)]
    pub members: Vec<TeamMember>,
    #[serde(default)]
    pub projects: Vec<TeamProject>,
    #[serde(default)]
    pub can_manage: bool,
}

/// `POST /api/v1/teams`, `PATCH /api/v1/teams/{id}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamEnvelope {
    pub team: Team,
}

/// One person in the org, as `GET /api/v1/members` reports them.
///
/// `role` is the org role (`owner` | `admin` | `member`), and `user_id` is the
/// id the role and removal routes take — not the email.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub user_id: String,
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub joined_at: String,
    /// The member's primary org relationship.
    #[serde(default)]
    pub primary: bool,
}

/// `GET /api/v1/members`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembersResponse {
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub demo: bool,
}

/// `POST /api/v1/orgs/members` — what the add did.
///
/// `created_account` says whether this was also the account's first membership:
/// "added" and "created the account and added" are different facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberAdded {
    pub user_id: String,
    pub email: String,
    pub role: String,
    #[serde(default)]
    pub created_account: bool,
}

/// One pending invite, as `GET /api/v1/orgs/invites` reports it.
///
/// Never a token: the link and code exist only in the response to the call that
/// created the invite, so a list can never re-print a working credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub role: String,
    /// Set on a guest invite: the one project it reaches, and the level it carries.
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub access_level: Option<String>,
    #[serde(default)]
    pub invited_by_email: Option<String>,
    pub expires_at: String,
    #[serde(default)]
    pub created_at: String,
}

/// `GET /api/v1/orgs/invites`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitesResponse {
    #[serde(default)]
    pub invites: Vec<Invite>,
}

/// `POST /api/v1/orgs/invites` — the one response carrying the credentials.
///
/// Only the hashes are stored, so this is the only place the token and code
/// ever appear. `link` is `None` when the deployment has no public URL, which
/// is why `email_sent` exists separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteCreated {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub expires_at: String,
    /// The join link's token.
    #[serde(default)]
    pub token: String,
    /// The short code an invitee can type instead.
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub email_sent: bool,
    #[serde(default)]
    pub email_error: Option<String>,
}

/// Generic error response from the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    /// Error code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Additional details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

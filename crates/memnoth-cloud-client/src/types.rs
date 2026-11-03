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

/// Token response (POST /auth/token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    /// The access token.
    pub access_token: String,
    /// Token type (typically "Bearer").
    pub token_type: String,
    /// Lifetime in seconds.
    pub expires_in: u64,
    /// Refresh token (if offline_access scope was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Scopes granted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
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

/// Current user info (GET /me).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeResponse {
    /// User ID.
    pub id: String,
    /// Email address.
    pub email: String,
    /// Display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Avatar URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Whether email is verified.
    pub email_verified: bool,
    /// Account creation timestamp (RFC3339).
    pub created_at: String,
    /// Last login timestamp (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<String>,
    /// User's organizations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organizations: Option<Vec<OrganizationSummary>>,
    /// User's default organization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_organization_id: Option<String>,
}

/// Organization summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationSummary {
    /// Organization ID.
    pub id: String,
    /// Organization name.
    pub name: String,
    /// User's role in the organization.
    pub role: String,
    /// Whether this is the default organization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
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

/// Credits response (GET /credits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditsResponse {
    /// Available credits in micros.
    pub balance_micros: i64,
    /// Credit currency (e.g., "USD").
    pub currency: String,
    /// Whether auto-recharge is enabled.
    pub auto_recharge: bool,
    /// Auto-recharge threshold in micros.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recharge_threshold_micros: Option<i64>,
    /// Auto-recharge amount in micros.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recharge_amount_micros: Option<i64>,
    /// Next billing date (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_billing_date: Option<String>,
}
/// Credit grant information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditGrant {
    /// Grant ID.
    pub id: String,
    /// Grant amount in micros.
    pub amount_micros: i64,
    /// Grant reason.
    pub reason: String,
    /// Grant timestamp (RFC3339).
    pub created_at: String,
    /// Expiry timestamp if applicable (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Credit expiry information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditExpiry {
    /// Expiry timestamp (RFC3339).
    pub expires_at: String,
    /// Amount expiring in micros.
    pub amount_micros: i64,
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

/// Project information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// Project ID.
    pub id: String,
    /// Project name.
    pub name: String,
    /// Project description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Organization ID.
    pub organization_id: String,
    /// Creation timestamp (RFC3339).
    pub created_at: String,
    /// Last updated timestamp (RFC3339).
    pub updated_at: String,
    /// Project settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<ProjectSettings>,
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

/// Organization list response (GET /orgs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationsResponse {
    /// Organizations.
    pub organizations: Vec<OrganizationInfo>,
}

/// Organization information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationInfo {
    /// Organization ID.
    pub id: String,
    /// Organization name.
    pub name: String,
    /// Organization slug.
    pub slug: String,
    /// User's role in the organization.
    pub role: String,
    /// Whether this is the user's default organization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    /// Member count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_count: Option<u32>,
    /// Creation timestamp (RFC3339).
    pub created_at: String,
}

/// MCP token response (POST /mcp/token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTokenResponse {
    /// The MCP access token.
    pub token: String,
    /// Token type.
    pub token_type: String,
    /// Lifetime in seconds.
    pub expires_in: u64,
    /// MCP server URL.
    pub mcp_url: String,
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

/// Pricing information for a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Model ID.
    pub model_id: String,
    /// Input token price per million tokens (in micros).
    pub input_price_per_million_micros: i64,
    /// Output token price per million tokens (in micros).
    pub output_price_per_million_micros: i64,
    /// Currency (e.g., "USD").
    pub currency: String,
}

/// Pricing response (GET /api/v1/pricing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingResponse {
    /// Model pricing entries.
    pub models: Vec<ModelPricing>,
    /// Currency.
    pub currency: String,
}

/// Billing response combining credits and pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingResponse {
    /// Credits information.
    pub credits: CreditsResponse,
    /// Pricing information.
    pub pricing: PricingResponse,
}

/// API key list response (GET /keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeysResponse {
    /// API keys.
    pub keys: Vec<ApiKeyInfo>,
}

/// API key information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    /// Key ID (not the secret).
    pub id: String,
    /// Key name/label.
    pub name: String,
    /// Key prefix (first 8 chars of secret).
    pub prefix: String,
    /// Scopes/permissions.
    pub scopes: Vec<String>,
    /// Creation timestamp (RFC3339).
    pub created_at: String,
    /// Last used timestamp (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    /// Expiration timestamp (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Whether the key is revoked.
    pub revoked: bool,
}

/// Create API key request (POST /keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKeyRequest {
    /// Key name/label.
    pub name: String,
    /// Scopes/permissions.
    pub scopes: Vec<String>,
    /// Expiration in days (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
}

/// Create API key response (POST /keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKeyResponse {
    /// Key ID.
    pub id: String,
    /// The full secret (only returned once).
    pub secret: String,
    /// Key prefix.
    pub prefix: String,
    /// Key name.
    pub name: String,
    /// Scopes.
    pub scopes: Vec<String>,
    /// Creation timestamp (RFC3339).
    pub created_at: String,
    /// Expiration timestamp (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Revoke API key response (DELETE /keys/{id}).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeKeyResponse {
    /// Whether the key was revoked.
    pub revoked: bool,
    /// Key ID.
    pub id: String,
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

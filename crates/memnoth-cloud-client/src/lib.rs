use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

pub mod error;
pub use error::{CloudError, CloudResult, RetryExhausted};
pub mod types;
pub use types::{
    ApiErrorResponse, ApiKeyInfo, BatchIngestItemResult,
    BatchIngestRequest, BatchIngestResponse, BillingResponse, CreateKeyRequest, CreateKeyResponse,
    CreditExpiry, CreditGrant, CreditsResponse, DeviceApproveResponse, DeviceAuthResponse,
    IngestRequest, IngestResponse, KeysResponse, McpTokenResponse, MeResponse,
    ModelInfo, ModelPricing, ModelUsage, OrganizationInfo, OrganizationSummary, OrganizationsResponse,
    PricingResponse, ProjectInfo, ProjectSettings, ProjectUsage, ProjectsResponse, RevokeKeyResponse,
    TokenResponse,
};

/// Response from GET /api/v1/models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub object: String,
    pub name: String,
    pub description: String,
}

/// Parameters for the usage API.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UsageParams {
    /// Time range: "7d", "30d", "90d", "365d"
    pub range: Option<String>,
    /// Group by: "day", "model", "project"
    pub by: Option<String>,
    /// Since date in YYYY-MM-DD format
    pub since: Option<String>,
}

/// Response from GET /api/v1/usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageResponse {
    pub range: String,
    pub demo: bool,
    pub totals: UsageTotals,
    pub series: Vec<UsagePoint>,
    pub leaderboard: Vec<LeaderboardRow>,
    pub models: Vec<ModelRow>,
}

/// Aggregate usage totals.

/// Aggregate usage totals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageTotals {
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub tasks: i64,
    pub cost_usd: f64,
}

/// One day of usage series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsagePoint {
    pub day: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: f64,
    pub tasks: i64,
}

/// One row of per-user spend leaderboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderboardRow {
    pub email: String,
    pub name: String,
    pub spend_usd: f64,
    pub tokens: i64,
    pub tasks: i64,
}

/// One row of per-model breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRow {
    pub provider: String,
    pub model: String,
    pub tokens: i64,
    pub cost_usd: f64,
}

/// Local turn receipt from memnoth-session for reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalReceipt {
    pub at_ms: i64,
    pub total_tokens: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_micros: Option<u64>,
    pub model: String,
}

/// Local totals computed from receipts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalTotals {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tasks: usize,
    pub cost_usd: f64,
}

/// Delta between cloud and local.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageDelta {
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub tasks: i64,
    pub cost_usd: f64,
}

/// Reconciliation result between cloud and local receipts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reconciliation {
    pub cloud_totals: UsageTotals,
    pub local_totals: LocalTotals,
    pub delta: UsageDelta,
    pub matched: usize,
    pub unmatched_cloud: usize,
    pub unmatched_local: usize,
}

/// Request for accepting an invite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptInviteRequest {
    /// The invite code in XXXX-XXXX format (hyphen optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The invite token from a join link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Response from accepting an invite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptInviteResponse {
    /// The organization ID that was joined.
    pub org_id: Uuid,
    /// The role assigned in the organization.
    pub role: String,
}

/// Telemetry preferences request (POST /api/v1/telemetry/preferences).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryPreferencesRequest {
    /// Whether telemetry collection is enabled.
    pub telemetry_enabled: bool,
}

/// Telemetry preferences response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryPreferencesResponse {
    /// Whether telemetry collection is enabled.
    pub telemetry_enabled: bool,
    /// When the preference was last updated.
    pub updated_at: String,
}

/// Device authorization request (POST /auth/device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAuthRequest {
    /// Client name for display purposes.
    pub client_name: String,
}

/// Response from GET /api/v1/models — the OpenAI-compatible envelope the cloud
/// serves, so a client can list models with the same parser it uses elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelList {
    /// Models this account can select.
    pub data: Vec<ModelEntry>,
}

const DEFAULT_BASE_URL: &str = "https://memnoth.astrivya.ai/api/v1";


/// Typed HTTP client for the memnoth cloud API.

/// Typed HTTP client for the memnoth cloud API.

/// Typed HTTP client for the memnoth cloud API.
#[derive(Debug, Clone)]
pub struct CloudClient {
    client: Client,
    base_url: Url,
    auth_token: Option<String>,
}

impl CloudClient {
    /// Create a new cloud client with the default base URL.
    pub fn new() -> CloudResult<Self> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Create a new cloud client with a custom base URL.
    pub fn with_base_url(base_url: &str) -> CloudResult<Self> {
        let base_url = Url::parse(base_url)
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?
            .join("/")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Ok(Self {
            client,
            base_url,
            auth_token: None,
        })
    }

    /// Set the authentication token.
    pub fn set_auth_token(&mut self, token: String) {
        self.auth_token = Some(token);
    }

    /// Clear the authentication token.
    pub fn clear_auth_token(&mut self) {
        self.auth_token = None;
    }

    /// Build request headers with auth if available.
    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        if let Some(token) = &self.auth_token {
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", token)) {
                headers.insert(AUTHORIZATION, value);
            }
        }
        headers
    }

    /// GET /api/v1/models
    pub fn models(&self) -> CloudResult<ModelList> {
        let url = self
            .base_url
            .join("models")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }
    /// POST /api/v1/auth/device - Start device authorization flow.
    pub fn device_auth(&self, request: DeviceAuthRequest) -> CloudResult<DeviceAuthResponse> {
        let url = self
            .base_url
            .join("auth/device")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .post(url)
            .headers(self.headers())
            .json(&request)
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// POST /api/v1/auth/token - Poll for device authorization token.
    pub fn device_token(&self, device_code: &str) -> CloudResult<TokenResponse> {
        let url = self
            .base_url
            .join("auth/token")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let body = serde_json::json!({ "device_code": device_code });

        let response = self
            .client
            .post(url)
            .headers(self.headers())
            .json(&body)
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// GET /api/v1/me - Get current user info.
    pub fn me(&self) -> CloudResult<MeResponse> {
        let url = self
            .base_url
            .join("me")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// GET /api/v1/org - Get organization info.
    pub fn org(&self) -> CloudResult<OrganizationSummary> {
        let url = self
            .base_url
            .join("org")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// DELETE /api/v1/keys/current - Revoke current API key.
    pub fn revoke_current_key(&self) -> CloudResult<()> {
        let url = self
            .base_url
            .join("keys/current")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .delete(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let text = response
                .text()
                .map_err(|e| CloudError::NetworkError(e.to_string()))?;
            Err(Self::status_to_error(status, text))
        }
    }

    fn status_to_error(status: reqwest::StatusCode, text: String) -> CloudError {
        match status.as_u16() {
            401 => CloudError::AuthError(text),
            402 => CloudError::CreditsExhausted(text),
            404 => CloudError::NotFound(text),
            429 => CloudError::RateLimited {
                retry_after_secs: 60,
                message: text,
            },
            500..=599 => CloudError::Transient {
                status: status.as_u16(),
                message: text,
            },
            _ => CloudError::ClientError {
                status: status.as_u16(),
                message: text,
            },
        }
    }

    /// GET /api/v1/usage
    pub fn usage(&self, params: UsageParams) -> CloudResult<UsageResponse> {
        let mut url = self
            .base_url
            .join("usage")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        if let Some(range) = params.range {
            url.query_pairs_mut().append_pair("range", &range);
        }
        if let Some(by) = params.by {
            url.query_pairs_mut().append_pair("by", &by);
        }
        if let Some(since) = params.since {
            url.query_pairs_mut().append_pair("since", &since);
        }

        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// Handle API response, converting errors appropriately.
    fn handle_response<T: for<'de> Deserialize<'de>>(
        response: reqwest::blocking::Response,
    ) -> CloudResult<T> {
        let status = response.status();
        let headers = response.headers().clone();
        let text = response
            .text()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        if status.is_success() {
            serde_json::from_str(&text).map_err(|e| CloudError::SerializationError(e.to_string()))
        } else {
            match status.as_u16() {
                401 => Err(CloudError::AuthError(text)),
                402 => Err(CloudError::CreditsExhausted(text)),
                404 => Err(CloudError::NotFound(text)),
                429 => {
                    let retry_after = headers
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse().ok());
                    Err(CloudError::RateLimited {
                        retry_after_secs: retry_after.unwrap_or(60),
                        message: text,
                    })
                }
                500..=599 => Err(CloudError::Transient {
                    status: status.as_u16(),
                    message: text,
                }),
                _ => Err(CloudError::ClientError {
                    status: status.as_u16(),
                    message: text,
                }),
            }
        }
    }
}

impl Default for CloudClient {
    fn default() -> Self {
        Self::new().expect("default cloud client")
    }
}
impl CloudClient {
    pub fn set_telemetry_preferences(&self, enabled: bool) -> CloudResult<TelemetryPreferencesResponse> {
        let url = self
            .base_url
            .join("telemetry/preferences")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let request_body = TelemetryPreferencesRequest {
            telemetry_enabled: enabled,
        };

        let response = self
            .client
            .post(url)
            .headers(self.headers())
            .json(&request_body)
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// Accept an invite using a code.
    pub fn accept_invite(&self, code: &str) -> CloudResult<AcceptInviteResponse> {
        let url = self
            .base_url
            .join("invites/accept")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let request_body = AcceptInviteRequest {
            code: Some(code.to_string()),
            token: None,
        };

        let response = self
            .client
            .post(url)
            .headers(self.headers())
            .json(&request_body)
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }
    /// POST /api/v1/ingest/batch - Send batch ingest data.
    pub fn ingest_batch(&self, request: BatchIngestRequest) -> CloudResult<BatchIngestResponse> {
        let url = self
            .base_url
            .join("ingest/batch")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .post(url)
            .headers(self.headers())
            .json(&request)
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// GET /api/v1/credits - Get credits balance, grants, and expiry.
    pub fn credits(&self) -> CloudResult<CreditsResponse> {
        let url = self
            .base_url
            .join("credits")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// GET /api/v1/pricing - Get model pricing.
    pub fn pricing(&self) -> CloudResult<PricingResponse> {
        let url = self
            .base_url
            .join("pricing")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        Self::handle_response(response)
    }

    /// GET /api/v1/billing - Get combined billing info (credits + pricing).
    pub fn billing(&self) -> CloudResult<BillingResponse> {
        let url = self
            .base_url
            .join("billing")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        Self::handle_response(response)
    }

    fn compute_local_totals(receipts: &[LocalReceipt]) -> LocalTotals {
        let mut tokens_in: u64 = 0;
        let mut tokens_out: u64 = 0;
        let mut cost_micros: u64 = 0;

        for r in receipts {
            tokens_in += r.input_tokens;
            tokens_out += r.output_tokens;
            if let Some(c) = r.cost_micros {
                cost_micros += c;
            }
        }

        LocalTotals {
            tokens_in,
            tokens_out,
            tasks: receipts.len(),
            cost_usd: cost_micros as f64 / 1_000_000.0,
        }
    }

    /// Reconcile cloud usage with local receipts.
    pub fn reconcile(
        &self,
        params: UsageParams,
        local_receipts: Vec<LocalReceipt>,
    ) -> CloudResult<Reconciliation> {
        let cloud = self.usage(params)?;
        let local_totals = Self::compute_local_totals(&local_receipts);

        // Simple matching: match by timestamp proximity (within 5 minutes)
        let mut matched = 0;

        // For now, just compare totals using the new API structure
        let delta = UsageDelta {
            tokens_in: cloud.totals.tokens_in - local_totals.tokens_in as i64,
            tokens_out: cloud.totals.tokens_out - local_totals.tokens_out as i64,
            tasks: cloud.totals.tasks - local_totals.tasks as i64,
            cost_usd: cloud.totals.cost_usd - local_totals.cost_usd,
        };

        Ok(Reconciliation {
            cloud_totals: UsageTotals {
                tokens_in: cloud.totals.tokens_in,
                tokens_out: cloud.totals.tokens_out,
                tasks: cloud.totals.tasks,
                cost_usd: cloud.totals.cost_usd,
            },
            local_totals,
            delta,
            matched,
            unmatched_cloud: 0,
            unmatched_local: local_receipts.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = CloudClient::new().unwrap();
        assert_eq!(
            client.base_url.as_str(),
            "https://memnoth.astrivya.ai/api/v1/"
        );
    }

    #[test]
    fn test_client_custom_base_url() {
        let client = CloudClient::with_base_url("https://custom.example.com/api/v1").unwrap();
        assert_eq!(
            client.base_url.as_str(),
            "https://custom.example.com/api/v1/"
        );
    }

    #[test]
    fn test_usage_params_default() {
        let params = UsageParams::default();
        assert!(params.range.is_none());
        assert!(params.by.is_none());
        assert!(params.since.is_none());
    }
}

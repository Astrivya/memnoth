use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

pub mod error;
pub use error::{CloudError, CloudResult, RetryExhausted};
pub mod types;
pub use types::{
    ApiErrorResponse, ApiKeyInfo, ApprovedAccountKey, BatchIngestRequest, BatchIngestResponse,
    BillingEntitlements, CreateKeyRequest, CreateKeyResponse, CreditGrant, CreditLedgerEntry,
    CreditsResponse, DeviceApproveResponse, DeviceAuthResponse, HeartbeatAck, HeartbeatRequest,
    IngestCounts, IngestEvent, InstanceCounts, Invite, InviteCreated, InvitesResponse,
    KeysResponse, McpTokenResponse, Me, Member, MemberAdded, MembersResponse, ModelInfo,
    ModelUsage, OkResponse, OrgEnvelope, OrgInfo, OrgSwitch, OrganizationInfo,
    OrganizationsResponse, PlaneStatus, ProjectEnvelope, ProjectInfo, ProjectSettings,
    ProjectUsage, ProjectsResponse, RevokeKeyResponse, SessionReport, SyncManifestAck,
    SyncManifestList, SyncManifestSummary, SyncObjectAck, Team, TeamDetail, TeamEnvelope,
    TeamMember, TeamProject, TeamsResponse,
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

// Telemetry is a **local** preference: the control plane has no endpoint for it,
// and the call this client used to make (`POST /telemetry/preferences`) answered
// 404 on every deployment. The setting lives in `config.toml`; see `mn telemetry`
// and `mn privacy`.

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

/// The variable that points this client at a different control plane.
///
/// It must be named somewhere a caller can find it, and the client must honour
/// it everywhere: `team.rs` and `teams.rs` have always read this variable, so a
/// staging plane accepted `mn team` requests while every other cloud command in
/// the same shell went to production. It is also the only way to point the CLI
/// at a plane on this machine, which is what makes an end-to-end check of these
/// endpoints possible at all.
const BASE_URL_ENV: &str = "MEMNOTH_CLOUD_URL";

/// Typed HTTP client for the memnoth cloud API.
#[derive(Debug, Clone)]
pub struct CloudClient {
    client: Client,
    base_url: Url,
    auth_token: Option<String>,
}

impl CloudClient {
    /// Create a new cloud client with the default base URL.
    ///
    /// Deliberately ignores `MEMNOTH_CLOUD_URL`: a constructor whose host
    /// depends on the environment makes every test that asserts a URL read the
    /// developer's shell. [`CloudClient::from_env`] is the command-facing one.
    pub fn new() -> CloudResult<Self> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Create the client a command should use: `MEMNOTH_CLOUD_URL` when it is
    /// set, the hosted plane otherwise.
    ///
    /// This is what points the CLI at a staging — or a local — plane. It has to
    /// be honoured by every command, because `team.rs` and `teams.rs` have
    /// always read the variable: honouring it in two modules and ignoring it
    /// everywhere else meant `mn team` reached the plane an operator named while
    /// `mn sync` and `mn usage` in the same shell reached production.
    pub fn from_env() -> CloudResult<Self> {
        match std::env::var(BASE_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            Some(base) => Self::with_base_url(&base),
            None => Self::new(),
        }
    }

    /// Create a new cloud client with a custom base URL.
    ///
    /// Accepts either an API base (`…/api/v1`) or a deployment root (`https://plane.example`),
    /// because both are what an operator reasonably pastes into [`BASE_URL_ENV`]
    /// and this client appends bare paths (`get("projects")`). A root was joined
    /// as `…/projects`, which the router does not serve: every cloud verb except
    /// the few that pass an absolute path (`/api/v1/me` in `auth.rs`) answered
    /// 404, so `mn projects list` said "not found" against a plane that was
    /// working fine.
    pub fn with_base_url(base_url: &str) -> CloudResult<Self> {
        let mut base_url =
            Url::parse(base_url).map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        // The API lives under `/api/v1`. Append it when the caller gave a root
        // or a wider prefix, keep it when they already named it.
        let path = base_url.path().trim_end_matches('/').to_string();
        let path = if path.ends_with("/api/v1") {
            path
        } else {
            format!("{path}/api/v1")
        };

        // The trailing slash is what makes `join("models")` append rather than
        // replace the last segment; `join("/")` would also throw away `/api/v1`
        // and land every request on the web router.
        base_url.set_path(&format!("{path}/"));

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

    /// The resolved base this client appends paths to, always ending in `/`.
    ///
    /// Resolved, not echoed: [`CloudClient::with_base_url`] adds `/api/v1` when
    /// it was not given, so this is the URL a request is actually built from.
    pub fn base_url(&self) -> &Url {
        &self.base_url
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
        // The API refuses writes without this marker. It exists to stop a
        // browser from issuing state-changing requests on a user's behalf; a
        // CLI has no ambient cookie session to abuse, and the server asks for
        // the literal marker, so every client request carries it.
        headers.insert("x-memnoth-csrf", HeaderValue::from_static("1"));
        if let Some(token) = &self.auth_token
            && let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", token))
        {
            headers.insert(AUTHORIZATION, value);
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

    /// POST /api/v1/auth/token — poll for the key an approval minted.
    ///
    /// The 200 body is the account contract (`api_key`, `account_id`, `email`,
    /// `tier`, `status`), not an OAuth token envelope.
    pub fn device_token(&self, device_code: &str) -> CloudResult<ApprovedAccountKey> {
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

    /// GET /api/v1/me — who this credential is (subject, org, roles).
    pub fn me(&self) -> CloudResult<Me> {
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

    /// GET /api/v1/org — the tenant's identity.
    ///
    /// The server wraps it as `{"org": {…}}`; callers want the org, so the
    /// envelope stops here rather than in every command.
    pub fn org(&self) -> CloudResult<OrgInfo> {
        let envelope: OrgEnvelope = Self::handle_response(self.get("org")?)?;
        Ok(envelope.org)
    }

    /// GET /api/v1/billing/entitlements — the live plan ceilings.
    pub fn entitlements(&self) -> CloudResult<BillingEntitlements> {
        Self::handle_response(self.get("billing/entitlements")?)
    }

    /// GET — one request with the standard headers, no decoding.
    fn get(&self, path: &str) -> CloudResult<reqwest::blocking::Response> {
        let url = self
            .base_url
            .join(path)
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        self.client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))
    }

    /// POST — one request with the standard headers and a JSON body.
    fn post<T: Serialize>(&self, path: &str, body: &T) -> CloudResult<reqwest::blocking::Response> {
        let url = self
            .base_url
            .join(path)
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        self.client
            .post(url)
            .headers(self.headers())
            .json(body)
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))
    }

    /// PATCH — the partial update the org and project routes take.
    fn patch<T: Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> CloudResult<reqwest::blocking::Response> {
        let url = self
            .base_url
            .join(path)
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        self.client
            .patch(url)
            .headers(self.headers())
            .json(body)
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))
    }

    /// DELETE — no body; the caller only needs to know whether it was refused.
    fn delete(&self, path: &str) -> CloudResult<reqwest::blocking::Response> {
        let url = self
            .base_url
            .join(path)
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        self.client
            .delete(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))
    }

    /// Expect success and ignore the body.
    ///
    /// The delete routes answer `204` and the two writes in `org.rs` answer
    /// `{ "ok": true }`; neither is a shape worth a type, but a refusal still has
    /// to become the same typed error every other call produces.
    fn expect_ok(response: reqwest::blocking::Response) -> CloudResult<()> {
        let status = response.status();
        let text = response
            .text()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        if status.is_success() {
            Ok(())
        } else {
            Err(Self::status_to_error(status, text))
        }
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
    /// POST /api/v1/ingest/batch - Send one surface's queued events.
    ///
    /// The plane deduplicates on `(org, event_id)`, so a batch that is retried
    /// after a timeout is a no-op and answers with `duplicates` instead of a
    /// second copy of every event.
    pub fn ingest_batch(&self, request: &BatchIngestRequest) -> CloudResult<BatchIngestResponse> {
        Self::handle_response(self.post("ingest/batch", request)?)
    }

    /// GET /api/v1/sync/manifests?since=… — manifest metadata, never plaintext.
    pub fn sync_manifests(&self, since: u64) -> CloudResult<SyncManifestList> {
        let mut url = self
            .base_url
            .join("sync/manifests")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("since", &since.to_string());
        let response = self
            .client
            .get(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        Self::handle_response(response)
    }

    /// GET /api/v1/sync/manifests/{version} — the encrypted envelope.
    pub fn sync_manifest(&self, version: u64) -> CloudResult<Vec<u8>> {
        let response = self.get(&format!("sync/manifests/{version}"))?;
        Self::handle_bytes(response)
    }

    /// PUT /api/v1/sync/manifests/{version} — store one encrypted envelope.
    ///
    /// The version is the client's generation. The server refuses a second body
    /// at the same version, which is why a retry must reuse its ciphertext.
    pub fn put_sync_manifest(
        &self,
        version: u64,
        ciphertext: &[u8],
        ciphertext_hash: &str,
    ) -> CloudResult<SyncManifestAck> {
        let url = self
            .base_url
            .join(&format!("sync/manifests/{version}"))
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        let mut headers = self.headers();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(
            "x-memnoth-ciphertext-hash",
            HeaderValue::from_str(ciphertext_hash)
                .map_err(|e| CloudError::ConfigError(e.to_string()))?,
        );
        let response = self
            .client
            .put(url)
            .headers(headers)
            .body(ciphertext.to_vec())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        Self::handle_response(response)
    }

    /// GET /api/v1/sync/objects/{hash} — one encrypted object.
    pub fn sync_object(&self, hash: &str) -> CloudResult<Vec<u8>> {
        let response = self.get(&format!("sync/objects/{hash}"))?;
        Self::handle_bytes(response)
    }

    /// PUT /api/v1/sync/objects/{hash} — one complete encrypted object.
    pub fn put_sync_object(&self, hash: &str, ciphertext: &[u8]) -> CloudResult<SyncObjectAck> {
        let url = self
            .base_url
            .join(&format!("sync/objects/{hash}"))
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        let mut headers = self.headers();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        let response = self
            .client
            .put(url)
            .headers(headers)
            .body(ciphertext.to_vec())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        Self::handle_response(response)
    }

    /// PUT one resumable prefix of an object.
    ///
    /// `offset` is the first byte in `chunk`; `total` is the full envelope size.
    /// The server answers `complete: false` until the last byte lands.
    pub fn put_sync_object_range(
        &self,
        hash: &str,
        chunk: &[u8],
        offset: u64,
        total: u64,
    ) -> CloudResult<SyncObjectAck> {
        let url = self
            .base_url
            .join(&format!("sync/objects/{hash}"))
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        let end = offset.saturating_add(chunk.len() as u64);
        let mut headers = self.headers();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {offset}-{}-{total}", end.saturating_sub(1)))
                .map_err(|e| CloudError::ConfigError(e.to_string()))?,
        );
        let response = self
            .client
            .put(url)
            .headers(headers)
            .body(chunk.to_vec())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        Self::handle_response(response)
    }

    /// HEAD /api/v1/sync/objects/{hash} — how many bytes the server already has.
    ///
    /// A completed object reports its size; a partial upload reports the next
    /// offset. `0` means the object is absent. This is what lets a retry resume
    /// instead of either restarting or guessing.
    pub fn sync_object_offset(&self, hash: &str) -> CloudResult<u64> {
        let url = self
            .base_url
            .join(&format!("sync/objects/{hash}"))
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;
        let response = self
            .client
            .head(url)
            .headers(self.headers())
            .send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(0);
        }
        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .map_err(|e| CloudError::NetworkError(e.to_string()))?;
            return Err(CloudError::from_response(status.as_u16(), text, None));
        }
        Ok(response
            .headers()
            .get("x-memnoth-upload-offset")
            .or_else(|| response.headers().get("x-memnoth-size-bytes"))
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0))
    }

    /// Read a binary response, mapping an error body through the same table as
    /// every JSON route.
    fn handle_bytes(response: reqwest::blocking::Response) -> CloudResult<Vec<u8>> {
        let status = response.status();
        let bytes = response
            .bytes()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        if status.is_success() {
            Ok(bytes.to_vec())
        } else {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Err(CloudError::from_response(status.as_u16(), text, None))
        }
    }

    /// POST /api/v1/sessions - record (or update) one session row.
    ///
    /// The dashboard reads these rows: this is what puts a local conversation on
    /// the console's Sessions page, and closing it is what rolls its tokens into
    /// the usage charts. The plane upserts on `id`, so a session is reported
    /// opened, then updated as it runs, then closed — the same id every time.
    pub fn record_session(&self, session: &SessionReport) -> CloudResult<OkResponse> {
        Self::handle_response(self.post("sessions", session)?)
    }

    /// GET /api/v1/sessions — every row this credential can see.
    ///
    /// The console's Sessions page and this call read the same rows, which is
    /// what makes "what is still unassigned" answerable from a terminal.
    pub fn sessions(&self) -> CloudResult<types::SessionsResponse> {
        Self::handle_response(self.get("sessions")?)
    }

    /// PATCH /api/v1/sessions/{id} — file a session under a project, or unfile it.
    ///
    /// The row is the same one the console's assign button writes: attribution
    /// is one fact, not two surfaces with an opinion each.
    pub fn assign_session(&self, id: &str, project: Option<&str>) -> CloudResult<OkResponse> {
        let body = serde_json::json!({ "project_id": project });
        Self::handle_response(self.patch(&format!("sessions/{id}"), &body)?)
    }

    /// POST /api/v1/instances/heartbeat - say this machine is alive.
    ///
    /// `/api/v1/status` counts instances, and an instance only exists because it
    /// beat: without this the console can report every session as history and
    /// never say that the machine that ran it is still there.
    pub fn heartbeat(&self, beat: &HeartbeatRequest) -> CloudResult<HeartbeatAck> {
        Self::handle_response(self.post("instances/heartbeat", beat)?)
    }

    /// GET /api/v1/status - live, stale and known instances plus ingest totals.
    pub fn status(&self) -> CloudResult<PlaneStatus> {
        Self::handle_response(self.get("status")?)
    }

    /// POST /api/v1/feedback — one message against an org, for the operator queue.
    ///
    /// Returns the server's reply as it sent it: the filer reads `id`, the
    /// operator console reads the row. Through this client rather than a
    /// hand-built request because the route is a **mutating** one, and a write
    /// without the `x-memnoth-csrf` marker is refused `403` before it is read —
    /// which is what every `mn feedback send` did.
    pub fn feedback(
        &self,
        message: &str,
        category: &str,
        include_context: bool,
    ) -> CloudResult<serde_json::Value> {
        let body = serde_json::json!({
            "message": message,
            "category": category,
            "include_context": include_context,
        });
        Self::handle_response(self.post("feedback", &body)?)
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

    /// GET /api/v1/orgs — every org this credential can see.
    pub fn organizations(&self) -> CloudResult<OrganizationsResponse> {
        Self::handle_response(self.get("orgs")?)
    }

    /// POST /api/v1/auth/session/org — move a browser session's org selection.
    ///
    /// Refused for an API key by design: a key is bound to the org it was
    /// minted in. The CLI surfaces the refusal verbatim rather than retrying.
    pub fn switch_org(&self, slug: &str) -> CloudResult<OrgSwitch> {
        Self::handle_response(self.post("auth/session/org", &serde_json::json!({ "org": slug }))?)
    }

    /// GET /api/v1/projects
    pub fn projects(&self) -> CloudResult<ProjectsResponse> {
        Self::handle_response(self.get("projects")?)
    }

    /// POST /api/v1/projects
    pub fn create_project(&self, name: &str, visibility: Option<&str>) -> CloudResult<ProjectInfo> {
        let body = serde_json::json!({ "name": name, "visibility": visibility });
        let envelope: ProjectEnvelope = Self::handle_response(self.post("projects", &body)?)?;
        Ok(envelope.project)
    }

    /// GET /api/v1/api-keys — keys without their secrets.
    pub fn api_keys(&self) -> CloudResult<KeysResponse> {
        Self::handle_response(self.get("api-keys")?)
    }

    /// POST /api/v1/api-keys — mint a key. The response carries it once.
    pub fn create_key(&self, request: CreateKeyRequest) -> CloudResult<CreateKeyResponse> {
        Self::handle_response(self.post("api-keys", &request)?)
    }

    /// POST /api/v1/api-keys/{id}/revoke
    pub fn revoke_key(&self, id: &str) -> CloudResult<RevokeKeyResponse> {
        let response = self.post(&format!("api-keys/{id}/revoke"), &serde_json::json!({}))?;
        Self::handle_response(response)
    }

    /// POST /api/v1/mcp/token — a short-lived bearer for the hosted MCP endpoint.
    pub fn mcp_token(&self) -> CloudResult<McpTokenResponse> {
        Self::handle_response(self.post("mcp/token", &serde_json::json!({}))?)
    }

    /// PATCH /api/v1/projects/{id} — rename, or change who can see it.
    pub fn rename_project(&self, id: &str, name: &str) -> CloudResult<ProjectInfo> {
        let body = serde_json::json!({ "name": name });
        let envelope: ProjectEnvelope =
            Self::handle_response(self.patch(&format!("projects/{id}"), &body)?)?;
        Ok(envelope.project)
    }

    /// DELETE /api/v1/projects/{id} — the sessions it held survive, unassigned.
    pub fn delete_project(&self, id: &str) -> CloudResult<()> {
        Self::expect_ok(self.delete(&format!("projects/{id}"))?)
    }

    /// POST /api/v1/api-keys/revoke-all — every live key in the org, at once.
    ///
    /// An owner/admin action on the server; the refusal is reported verbatim.
    pub fn revoke_all_keys(&self) -> CloudResult<RevokeKeyResponse> {
        Self::handle_response(self.post("api-keys/revoke-all", &serde_json::json!({}))?)
    }

    /// GET /api/v1/members — the org's roster (an admin read).
    pub fn members(&self) -> CloudResult<MembersResponse> {
        Self::handle_response(self.get("members")?)
    }

    /// POST /api/v1/orgs/members — add an existing account to the org by email.
    ///
    /// It does not create an account: the server answers 404 for an address that
    /// has never signed in. `create_invite` is the route that can bring someone new.
    pub fn add_member(&self, email: &str, role: Option<&str>) -> CloudResult<MemberAdded> {
        let body = serde_json::json!({ "email": email, "role": role });
        Self::handle_response(self.post("orgs/members", &body)?)
    }

    /// PATCH /api/v1/orgs/members/{user_id} — set a member's org role.
    pub fn set_member_role(&self, user_id: &str, role: &str) -> CloudResult<()> {
        let body = serde_json::json!({ "role": role });
        Self::expect_ok(self.patch(&format!("orgs/members/{user_id}"), &body)?)
    }

    /// DELETE /api/v1/orgs/members/{user_id} — removal takes their grants and
    /// keys with it, so this answers 204 and nothing else.
    pub fn remove_member(&self, user_id: &str) -> CloudResult<()> {
        Self::expect_ok(self.delete(&format!("orgs/members/{user_id}"))?)
    }

    /// GET /api/v1/orgs/invites — pending invites only, never a credential.
    pub fn invites(&self) -> CloudResult<InvitesResponse> {
        Self::handle_response(self.get("orgs/invites")?)
    }

    /// POST /api/v1/orgs/invites — mint an invite and report how it was delivered.
    pub fn create_invite(&self, email: &str, role: Option<&str>) -> CloudResult<InviteCreated> {
        let body = serde_json::json!({ "email": email, "role": role });
        Self::handle_response(self.post("orgs/invites", &body)?)
    }

    /// DELETE /api/v1/orgs/invites/{id} — withdraw one pending invite.
    pub fn withdraw_invite(&self, id: &str) -> CloudResult<()> {
        Self::expect_ok(self.delete(&format!("orgs/invites/{id}"))?)
    }

    /// PATCH /api/v1/org — rename the tenant (owner or admin).
    pub fn rename_org(&self, name: &str) -> CloudResult<OrgInfo> {
        let body = serde_json::json!({ "name": name });
        let envelope: OrgEnvelope = Self::handle_response(self.patch("org", &body)?)?;
        Ok(envelope.org)
    }

    /// GET /api/v1/teams — the org's groups, with the caller's own membership
    /// and management rights on each row.
    pub fn teams(&self) -> CloudResult<TeamsResponse> {
        Self::handle_response(self.get("teams")?)
    }

    /// POST /api/v1/teams — any member may start one; they own it.
    pub fn create_team(&self, name: &str) -> CloudResult<Team> {
        let body = serde_json::json!({ "name": name });
        let envelope: TeamEnvelope = Self::handle_response(self.post("teams", &body)?)?;
        Ok(envelope.team)
    }

    /// GET /api/v1/teams/{id} — one team, its people and the projects it reaches.
    pub fn team(&self, id: &str) -> CloudResult<TeamDetail> {
        Self::handle_response(self.get(&format!("teams/{id}"))?)
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
        let matched = 0;

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
#[cfg(test)]
mod tests_wire;

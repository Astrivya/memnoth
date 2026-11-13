//! Main cloud client providing both authenticated and unauthenticated API access.

use std::time::Duration;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use url::Url;

use crate::{CloudError, Result as CloudResult};
use crate::types::{BillingEntitlements, Me, OrgInfo, ErrorBody};
use crate::auth::DeviceAuthClient;

/// Main entry point for the cloud API client.
pub struct CloudClient {
    http: Client,
    base_url: Url,
    api_key: Option<String>,
    device_client: DeviceAuthClient,
}

impl CloudClient {
    /// Create a new cloud client.
    pub fn new(base_url: &str, api_key: Option<String>) -> CloudResult<Self> {
        let base_url = Url::parse(base_url)
            .map_err(|e| CloudError::InvalidUrl(format!("invalid base URL: {e}")))?;
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        let device_client = DeviceAuthClient::new(crate::auth::CloudConfig {
            base_url: base_url.to_string(),
            api_key: api_key.clone(),
        })?;

        Ok(Self {
            http,
            base_url,
            api_key,
            device_client,
        })
    }

    /// Get a device auth client for unauthenticated device flow operations.
    pub fn device_auth(&self) -> &DeviceAuthClient {
        &self.device_client
    }

    /// Get current user identity.
    ///
    /// GET /api/v1/me
    pub fn get_me(&self) -> CloudResult<Me> {
        let url = self.base_url.join("/api/v1/me")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let mut req = self.http.get(url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        handle_response(resp).map(|v| serde_json::from_value(v).unwrap())
    }

    /// Get organization info.
    ///
    /// GET /api/v1/org
    pub fn get_org(&self) -> CloudResult<OrgInfo> {
        let url = self.base_url.join("/api/v1/org")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let mut req = self.http.get(url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        handle_response(resp).map(|v| serde_json::from_value(v).unwrap())
    }

    /// Get organization entitlements.
    ///
    /// GET /api/v1/billing/entitlements
    pub fn entitlements(&self) -> CloudResult<BillingEntitlements> {
        let url = self.base_url.join("/api/v1/billing/entitlements")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let mut req = self.http.get(url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        handle_response(resp).map(|v| serde_json::from_value(v).unwrap())
    }

    /// Revoke the current API key.
    ///
    /// DELETE /api/v1/keys/current
    pub fn revoke_current_key(&self) -> CloudResult<()> {
        self.device_client.revoke_current_key()
    }
}

/// Handle a successful HTTP response, extracting JSON.
fn handle_response(resp: reqwest::blocking::Response) -> CloudResult<serde_json::Value> {
    let status = resp.status();
    if status.is_success() {
        resp.json()
            .map_err(|e| CloudError::SerializationError(e.to_string()))
    } else {
        let error_body: Result<ErrorBody, _> = resp.json();
        let error_msg = error_body
            .map(|e| e.error)
            .unwrap_or_else(|_| format!("HTTP {}", status));
        Err(map_http_error(status, error_msg))
    }
}

/// Map HTTP status to CloudError.
fn map_http_error(status: reqwest::StatusCode, message: String) -> CloudError {
    match status.as_u16() {
        401 => CloudError::AuthError(message),
        402 => CloudError::CreditsExhausted(message),
        404 => CloudError::NotFound(message),
        429 => {
            let retry_after = status
                .as_str()
                .parse()
                .ok()
                .and_then(|s: String| s.parse().ok());
            CloudError::RateLimited {
                retry_after_secs: retry_after.unwrap_or(60),
                message,
            }
        }
        500..=599 => CloudError::Transient {
            status: status.as_u16(),
            message,
        },
        _ => CloudError::ClientError {
            status: status.as_u16(),
            message,
        },
    }
}

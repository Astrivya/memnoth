//! Device authorization flow client for memnoth cloud API.

use std::time::Duration;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use url::Url;

use crate::{CloudError, Result as CloudResult};
use crate::types::{DeviceAuthorization, ApprovedAccountKey, ErrorBody};

/// Configuration for the cloud client.
#[derive(Debug, Clone)]
pub struct CloudConfig {
    /// Base URL for the cloud API (e.g., "https://api.memnoth.sh")
    pub base_url: String,
    /// Optional API key for authenticated requests.
    pub api_key: Option<String>,
}

/// Client for device authorization flow.
pub struct DeviceAuthClient {
    http: Client,
    base_url: Url,
    api_key: Option<String>,
}

impl DeviceAuthClient {
    /// Create a new device auth client with the given configuration.
    pub fn new(config: CloudConfig) -> CloudResult<Self> {
        let base_url = Url::parse(&config.base_url)
            .map_err(|e| CloudError::InvalidUrl(format!("invalid base URL: {e}")))?;
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;
        Ok(Self {
            http,
            base_url,
            api_key: config.api_key,
        })
    }

    /// Start a device authorization flow.
    ///
    /// POST /api/v1/auth/device
    /// Body: { "client_name": string, "requested_tier": string? }
    pub fn start_device_auth(
        &self,
        client_name: &str,
        requested_tier: Option<&str>,
    ) -> CloudResult<DeviceAuthorization> {
        let url = self.base_url.join("/api/v1/auth/device")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let body = serde_json::json!({
            "client_name": client_name,
            "requested_tier": requested_tier,
        });

        let mut req = self.http.post(url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        handle_response(resp).map(|v| serde_json::from_value(v).unwrap())
    }

    /// Poll for device authorization completion.
    ///
    /// POST /api/v1/auth/token
    /// Body: { "device_code": string }
    ///
    /// Returns:
    /// - Ok(DevicePollResult::Approved(key)) on success
    /// - Ok(DevicePollResult::Pending) if still waiting for approval
    /// - Ok(DevicePollResult::Denied) if user denied the request
    /// - Err on other errors
    pub fn poll_device_token(&self, device_code: &str) -> CloudResult<DevicePollResult> {
        let url = self.base_url.join("/api/v1/auth/token")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let body = serde_json::json!({ "device_code": device_code });

        let mut req = self.http.post(url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        let status = resp.status();
        if status.is_success() {
            let key: ApprovedAccountKey = resp.json()
                .map_err(|e| CloudError::SerializationError(e.to_string()))?;
            return Ok(DevicePollResult::Approved(key));
        }

        // Check for specific error codes
        let error_body: Result<ErrorBody, _> = resp.json();
        let error_msg = error_body
            .map(|e| e.error)
            .unwrap_or_else(|_| "unknown error".to_string());

        if status.as_u16() == 428 {
            // 428 Precondition Required = still pending
            return Ok(DevicePollResult::Pending);
        }
        if status.as_u16() == 400 && error_msg.contains("denied") {
            return Ok(DevicePollResult::Denied);
        }

        Err(map_http_error(status, error_msg))
    }

    /// Revoke the current API key (logout).
    ///
    /// DELETE /api/v1/keys/current
    pub fn revoke_current_key(&self) -> CloudResult<()> {
        let url = self.base_url.join("/api/v1/keys/current")
            .map_err(|e| CloudError::InvalidUrl(e.to_string()))?;

        let mut req = self.http.delete(url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send()
            .map_err(|e| CloudError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let error_body: Result<ErrorBody, _> = resp.json();
            let error_msg = error_body
                .map(|e| e.error)
                .unwrap_or_else(|_| "revoke failed".to_string());
            Err(map_http_error(resp.status(), error_msg))
        }
    }

    /// Get current user identity.
    ///
    /// GET /api/v1/me
    pub fn get_me(&self) -> CloudResult<crate::types::Me> {
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
    pub fn get_org(&self) -> CloudResult<crate::types::OrgInfo> {
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
}

/// Result of polling the device token endpoint.
#[derive(Debug, Clone)]
pub enum DevicePollResult {
    Approved(ApprovedAccountKey),
    Pending,
    Denied,
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
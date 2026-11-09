//! Error types for the memnoth cloud API client.

use thiserror::Error;

/// Result type for cloud client operations.
pub type Result<T, E = CloudError> = std::result::Result<T, E>;
/// Type alias for the cloud client result.
pub type CloudResult<T> = Result<T, CloudError>;
/// All possible failure modes when calling the memnoth cloud API.
#[derive(Error, Debug, Clone)]
pub enum CloudError {
    /// Authentication failed (401). Token expired, invalid, or missing.
    #[error("authentication failed: {0}")]
    AuthError(String),

    /// Credits exhausted (402). Account has no remaining credits.
    #[error("credits exhausted: {0}")]
    CreditsExhausted(String),

    /// The encrypted-sync storage quota is exhausted (402, `E_SYNC_QUOTA`).
    #[error("sync quota exhausted: {message}")]
    SyncQuota {
        /// The server's explanation, including the attempted and quota bytes.
        message: String,
        /// The plan page the refusal points at.
        upgrade_url: Option<String>,
    },

    /// Resource not found (404).
    #[error("not found: {0}")]
    NotFound(String),

    /// Rate limited (429). Includes Retry-After seconds if provided.
    #[error("rate limited: retry after {retry_after_secs}s: {message}")]
    RateLimited {
        /// Seconds to wait before retrying, from Retry-After header.
        retry_after_secs: u64,
        /// Human-readable message from the server.
        message: String,
    },

    /// Transient server error (5xx). Safe to retry with backoff.
    #[error("server error ({status}): {message}")]
    Transient {
        /// HTTP status code (500, 502, 503, etc.)
        status: u16,
        /// Response body or error message.
        message: String,
    },

    /// Client error (4xx except 401, 402, 404, 429). Not retryable.
    #[error("client error ({status}): {message}")]
    ClientError {
        /// HTTP status code.
        status: u16,
        /// Response body or error message.
        message: String,
    },

    /// Network or transport error (DNS, TLS, connection refused, timeout).
    #[error("network error: {0}")]
    NetworkError(String),

    /// Request/response serialization or deserialization failed.
    #[error("serialization error: {0}")]
    SerializationError(String),

    /// Invalid URL or URL construction failed.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// Configuration error (missing base URL, missing credentials, etc.).
    #[error("configuration error: {0}")]
    ConfigError(String),
}

fn sync_quota_or_credits(body: String) -> CloudError {
    #[derive(serde::Deserialize)]
    struct Refusal {
        code: Option<String>,
        error: Option<String>,
        upgrade_url: Option<String>,
    }

    match serde_json::from_str::<Refusal>(&body) {
        Ok(refusal) if refusal.code.as_deref() == Some("E_SYNC_QUOTA") => CloudError::SyncQuota {
            message: refusal.error.unwrap_or(body),
            upgrade_url: refusal.upgrade_url,
        },
        _ => CloudError::CreditsExhausted(body),
    }
}

impl CloudError {
    /// Map an HTTP response to a CloudError based on status code.
    pub fn from_response(status: u16, body: String, retry_after: Option<u64>) -> Self {
        match status {
            401 => CloudError::AuthError(body),
            402 => sync_quota_or_credits(body),
            404 => CloudError::NotFound(body),
            429 => CloudError::RateLimited {
                retry_after_secs: retry_after.unwrap_or(60),
                message: body,
            },
            500..=599 => CloudError::Transient {
                status,
                message: body,
            },
            400..=499 => CloudError::ClientError {
                status,
                message: body,
            },
            _ => CloudError::ClientError {
                status,
                message: body,
            },
        }
    }

    /// Check if this error is retryable (transient or rate limited).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            CloudError::Transient { .. } | CloudError::RateLimited { .. }
        )
    }

    /// Get the recommended retry delay in seconds, if applicable.
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            CloudError::RateLimited {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            CloudError::Transient { .. } => None, // Use exponential backoff
            _ => None,
        }
    }
}

/// Error for retry logic exhaustion.
#[derive(Error, Debug, Clone)]
#[error("max retries ({max_retries}) exceeded: {last_error}")]
pub struct RetryExhausted {
    /// Maximum number of retry attempts that were made.
    pub max_retries: u32,
    /// The last error encountered.
    pub last_error: CloudError,
}

impl RetryExhausted {
    /// Create a new RetryExhausted error.
    pub fn new(max_retries: u32, last_error: CloudError) -> Self {
        Self {
            max_retries,
            last_error,
        }
    }
}

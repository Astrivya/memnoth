//! Retry logic with exponential backoff and Retry-After header support.

use std::time::Duration;

use crate::error::{CloudError, Result, RetryExhausted};

/// Configuration for retry behavior.
#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (not counting the initial attempt).
    pub max_retries: u32,
    /// Base delay for exponential backoff in milliseconds.
    pub base_delay_ms: u64,
    /// Maximum delay cap in milliseconds.
    pub max_delay_ms: u64,
    /// Jitter factor (0.0 to 1.0) to add randomness to delays.
    pub jitter_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
            jitter_factor: 0.1,
        }
    }
}

impl RetryConfig {
    /// Create a new retry config with custom max retries.
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Create a new retry config with custom base delay.
    pub fn with_base_delay(mut self, base_delay_ms: u64) -> Self {
        self.base_delay_ms = base_delay_ms;
        self
    }
}

/// Calculate the delay for a given attempt with exponential backoff and jitter.
fn calculate_delay(config: &RetryConfig, attempt: u32, retry_after: Option<u64>) -> Duration {
    // If server provided Retry-After, use that (capped at max_delay)
    if let Some(seconds) = retry_after {
        let delay_ms = (seconds * 1000).min(config.max_delay_ms);
        return Duration::from_millis(delay_ms);
    }

    // Exponential backoff: base_delay * 2^attempt
    let exp_delay = config.base_delay_ms.saturating_mul(1u64.saturating_shl(attempt.min(31)));
    let capped_delay = exp_delay.min(config.max_delay_ms);

    // Add jitter: ±jitter_factor * capped_delay
    let jitter_range = (capped_delay as f64 * config.jitter_factor) as u64;
    let jitter = if jitter_range > 0 {
        // Simple deterministic pseudo-random based on attempt
        let seed = attempt.wrapping_mul(1103515245).wrapping_add(12345);
        (seed % (2 * jitter_range + 1)).saturating_sub(jitter_range)
    } else {
        0
    };

    Duration::from_millis(capped_delay.saturating_add(jitter))
}

/// Execute an operation with retry logic.
///
/// Retries on:
/// - Network errors
/// - Rate limited (429) with Retry-After
/// - Server errors (5xx)
///
/// Does not retry on:
/// - Auth errors (401)
/// - Credits exhausted (402)
/// - Not found (404)
/// - Client errors (4xx except 429)
pub fn with_retry<F, T, E>(config: RetryConfig, mut operation: F) -> Result<T, RetryExhausted>
where
    F: FnMut() -> Result<T>,
    E: Into<CloudError>,
{
    let mut last_error = None;

    for attempt in 0..=config.max_retries {
        match operation() {
            Ok(value) => return Ok(value),
            Err(e) => {
                // Check if we should retry
                let should_retry = e.is_retryable();

                // Get retry-after from rate limited error
                let retry_after = e.retry_after_secs();

                last_error = Some(e);

                // If this was the last attempt or not retryable, break
                if attempt == config.max_retries || !should_retry {
                    break;
                }

                // Calculate delay and sleep
                let delay = calculate_delay(&config, attempt, retry_after);
                std::thread::sleep(delay);
            }
        }
    }

    Err(RetryExhausted::new(
        config.max_retries,
        last_error.unwrap_or_else(|| CloudError::ConfigError("unknown error".to_string())),
    ))
}

/// Execute an async operation with retry logic.
#[cfg(feature = "async")]
pub async fn with_retry_async<F, Fut, T, E>(config: RetryConfig, mut operation: F) -> Result<T, RetryExhausted>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    E: Into<CloudError>,
{
    let mut last_error = None;

    for attempt in 0..=config.max_retries {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(e) => {
                let should_retry = e.is_retryable();
                let retry_after = e.retry_after_secs();

                last_error = Some(e);

                if attempt == config.max_retries || !should_retry {
                    break;
                }

                let delay = calculate_delay(&config, attempt, retry_after);
                tokio::time::sleep(delay).await;
            }
        }
    }

    Err(RetryExhausted::new(
        config.max_retries,
        last_error.unwrap_or_else(|| CloudError::ConfigError("unknown error".to_string())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_delay_exponential() {
        let config = RetryConfig::default();

        // Attempt 0: base_delay = 500ms
        let d0 = calculate_delay(&config, 0, None);
        assert!(d0 >= Duration::from_millis(450) && d0 <= Duration::from_millis(550));

        // Attempt 1: 1000ms
        let d1 = calculate_delay(&config, 1, None);
        assert!(d1 >= Duration::from_millis(900) && d1 <= Duration::from_millis(1100));

        // Attempt 2: 2000ms
        let d2 = calculate_delay(&config, 2, None);
        assert!(d2 >= Duration::from_millis(1800) && d2 <= Duration::from_millis(2200));

        // Attempt 10: capped at max_delay (30000ms)
        let d10 = calculate_delay(&config, 10, None);
        assert!(d10 <= Duration::from_millis(30000));
    }

    #[test]
    fn test_calculate_delay_retry_after() {
        let config = RetryConfig::default();

        // Retry-After: 5 seconds
        let d = calculate_delay(&config, 0, Some(5));
        assert_eq!(d, Duration::from_secs(5));

        // Retry-After: 100 seconds (capped at max_delay 30s)
        let d = calculate_delay(&config, 0, Some(100));
        assert_eq!(d, Duration::from_millis(config.max_delay_ms));
    }

    #[test]
    fn test_with_retry_success_first_try() {
        let config = RetryConfig::default();
        let result = with_retry(config, || Ok::<i32, CloudError>(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_with_retry_success_after_retry() {
        let config = RetryConfig::default().with_max_retries(3);
        let mut attempts = 0;

        let result = with_retry(config, || {
            attempts += 1;
            if attempts < 2 {
                Err(CloudError::Transient {
                    status: 500,
                    message: "server error".to_string(),
                })
            } else {
                Ok(42)
            }
        });

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts, 2);
    }

    #[test]
    fn test_with_retry_exhausted() {
        let config = RetryConfig::default().with_max_retries(2);
        let mut attempts = 0;

        let result = with_retry(config, || {
            attempts += 1;
            Err(CloudError::Transient {
                status: 500,
                message: "server error".to_string(),
            })
        });

        assert!(result.is_err());
        assert_eq!(attempts, 3); // initial + 2 retries
    }

    #[test]
    fn test_with_retry_non_retryable() {
        let config = RetryConfig::default().with_max_retries(3);
        let mut attempts = 0;

        let result = with_retry(config, || {
            attempts += 1;
            Err(CloudError::AuthError("invalid token".to_string()))
        });

        assert!(result.is_err());
        assert_eq!(attempts, 1); // no retry on auth error
    }
}
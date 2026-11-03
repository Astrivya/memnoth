//! Prometheus metrics surface for the auth crate. The HTTP layer
//! (`memnoth-mcp`'s `http.rs`) increments one counter per `verify`
//! call and serves the exposition on `GET /metrics` (scraped by the
//! in-network Prometheus; see `astrivya-deploy/prometheus.yml`).
//!
//! ## Feature gate
//! The `prometheus` feature is **opt-in**. Without it, the public
//! `record_verify` and `render` functions compile and run as no-ops,
//! so consumers (Lane B's `memnoth-mcp`) can wire the call site
//! unconditionally and only enable the dep when actually scraping.
//!
//! ## Metric shape
//! - `memnoth_mcp_auth_total{mode, reason}` — Counter. `mode` is one
//!   of the `AuthMode::as_str()` values (`static` / `hs256` /
//!   `rs256_zitadel`). `reason` is `allow` (on success) or one of the
//!   `AuthFailureReason::as_str()` values (e.g. `bad_signature`,
//!   `expired`, `jwks_unavailable`).
//! - `memnoth_mcp_auth_jwks_cache_entries` — Gauge (set by the
//!   HTTP layer periodically; we expose a setter).
//!
//! These two are the metrics described in
//! `D:\xclaw\memnoth-findings.md` §13 (T8 handoff). The Grafana
//! dashboard (`astrivya-deploy` → `astrivya-memnoth.json`) queries
//! `rate(memnoth_mcp_auth_total[5m])` by mode / reason.

use crate::{AuthFailureReason, AuthMode};

/// Increment the auth counter. Called from the HTTP layer after every
/// `verify` attempt. With the `prometheus` feature disabled, this is
/// a no-op (the function signature is preserved so the call site
/// doesn't need feature-gating).
///
/// `reason` is `None` for a successful verify (recorded as
/// `reason="allow"`); `Some(err)` for a failure.
#[cfg(feature = "prometheus")]
pub fn record_verify(mode: AuthMode, reason: Option<&AuthFailureReason>) {
    use prometheus::{IntCounterVec, register_int_counter_vec};
    use std::sync::OnceLock;

    static COUNTER: OnceLock<IntCounterVec> = OnceLock::new();
    let counter = COUNTER.get_or_init(|| {
        register_int_counter_vec!(
            "memnoth_mcp_auth_total",
            "Total number of authentication attempts, labeled by mode and outcome reason",
            &["mode", "reason"]
        )
        .expect("metric registration must succeed (no other writer)")
    });
    let reason_label = match reason {
        None => "allow",
        Some(r) => r.as_str(),
    };
    counter
        .with_label_values(&[mode.as_str(), reason_label])
        .inc();
}

/// No-op variant of [`record_verify`] compiled when the `prometheus`
/// feature is off. Keeps the call site unconditional.
#[cfg(not(feature = "prometheus"))]
pub fn record_verify(_mode: AuthMode, _reason: Option<&AuthFailureReason>) {
    // No-op when the prometheus feature is disabled. The HTTP layer
    // can call this unconditionally; the in-process counter is
    // skipped but the verify itself still happens.
}

/// Set the JWKS cache size gauge. Called by the HTTP layer every
/// `METRICS_REFRESH_INTERVAL` (suggested: 30s) so the gauge stays
/// fresh without hammering the cache.
#[cfg(feature = "prometheus")]
pub fn set_jwks_cache_entries(entries: u64) {
    use prometheus::{IntGauge, register_int_gauge};
    use std::sync::OnceLock;

    static GAUGE: OnceLock<IntGauge> = OnceLock::new();
    let gauge = GAUGE.get_or_init(|| {
        register_int_gauge!(
            "memnoth_mcp_auth_jwks_cache_entries",
            "Number of keys currently in the RS256 JWKS cache"
        )
        .expect("metric registration must succeed (no other writer)")
    });
    gauge.set(entries as i64);
}

/// No-op variant of [`set_jwks_cache_entries`] compiled when the
/// `prometheus` feature is off.
#[cfg(not(feature = "prometheus"))]
pub fn set_jwks_cache_entries(_entries: u64) {
    // No-op without the prometheus feature.
}

/// Render the current metrics snapshot in Prometheus text exposition
/// format. With the `prometheus` feature disabled, returns an empty
/// string — the HTTP layer should serve a 204 No Content in that
/// case (no metrics to expose).
#[cfg(feature = "prometheus")]
pub fn render() -> String {
    use prometheus::Encoder;
    let metric_families = prometheus::gather();
    let mut buf = Vec::new();
    let encoder = prometheus::TextEncoder::new();
    encoder
        .encode(&metric_families, &mut buf)
        .expect("text encoding must succeed (in-memory buffer)");
    String::from_utf8(buf).unwrap_or_default()
}

/// No-op variant of [`render`] compiled when the `prometheus`
/// feature is off. Returns an empty string; the HTTP layer should
/// serve 204 No Content in that case.
#[cfg(not(feature = "prometheus"))]
pub fn render() -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuthContext;

    /// Without the `prometheus` feature, `record_verify` / `set_jwks_cache_entries`
    /// / `render` are no-ops. This is a structural test: it just calls
    /// them to make sure they don't panic and don't allocate.
    #[cfg(not(feature = "prometheus"))]
    #[test]
    fn noop_when_feature_disabled() {
        record_verify(
            AuthMode::Hs256UserJwt,
            Some(&AuthFailureReason::BadSignature),
        );
        record_verify(AuthMode::StaticBearer, None);
        set_jwks_cache_entries(42);
        let s = render();
        assert!(
            s.is_empty(),
            "render() must be empty without the prometheus feature"
        );
    }

    /// With the `prometheus` feature enabled, `render()` should
    /// include at least the auth counter in the output. This proves
    /// the feature gate wires the real implementation.
    #[cfg(feature = "prometheus")]
    #[test]
    fn render_includes_auth_counter() {
        record_verify(AuthMode::Hs256UserJwt, None); // allow
        record_verify(
            AuthMode::StaticBearer,
            Some(&AuthFailureReason::BadStaticToken),
        );
        let s = render();
        assert!(
            s.contains("memnoth_mcp_auth_total"),
            "render must include the auth counter, got: {s}"
        );
        assert!(
            s.contains("mode=\"hs256\""),
            "render must include the mode label"
        );
        assert!(
            s.contains("reason=\"allow\""),
            "render must include the allow reason"
        );
        assert!(
            s.contains("reason=\"bad_static_token\""),
            "render must include the failure reason"
        );
    }

    /// Sanity: the typed `AuthMode::as_str` and `AuthFailureReason::as_str`
    /// values are stable (used as metric label values). If someone
    /// renames these, the Grafana dashboard breaks. This test is the
    /// alarm bell.
    #[test]
    fn label_values_are_stable() {
        assert_eq!(AuthMode::StaticBearer.as_str(), "static");
        assert_eq!(AuthMode::Hs256UserJwt.as_str(), "hs256");
        assert_eq!(AuthMode::Rs256ZitadelSession.as_str(), "rs256_zitadel");
        assert_eq!(AuthFailureReason::BadSignature.as_str(), "bad_signature");
        assert_eq!(AuthFailureReason::Expired.as_str(), "expired");
        assert_eq!(
            AuthFailureReason::JwksUnavailable.as_str(),
            "jwks_unavailable"
        );
        assert_eq!(AuthFailureReason::Revoked.as_str(), "revoked");
    }

    /// `AuthContext` (referenced from this module) is Send + Sync.
    /// If someone adds a non-thread-safe field to it the build breaks
    /// here, which is what we want — metrics flow across all HTTP
    /// worker threads.
    #[test]
    fn auth_context_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthContext>();
    }
}

//! Outbound URL policy — the one place a remote endpoint is judged.
//!
//! The product is local-first but not local-only: a provider endpoint receives
//! the prompt, and an MCP URL is written into another program's config. Both
//! are places a typo, a malicious preset or a model suggestion can turn into a
//! request to this machine or a private network, so both go through here.
//! The rule, stated once:
//!
use std::net::{IpAddr, ToSocketAddrs};

/// Strip proxy environment variables so no outbound request can be routed
/// through an attacker-controlled proxy to an internal address.
///
/// Called once at process start, before anything makes a network call.
pub fn strip_proxy_env() {
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        // Edition 2024 marks environment mutation unsafe: another thread could
        // be reading the environment while this runs. This is called once, from
        // `run`, before any thread or request exists.
        unsafe { std::env::remove_var(var) };
    }
}

/// Validate a URL at config/registration time. Performs no DNS query.
pub fn validate_configured_url(raw: &str) -> Result<reqwest::Url, String> {
    validate(raw, false)
}

/// Validate a URL immediately before a request. Resolves a domain and refuses
/// a private or loopback answer.
pub fn validate_before_request(raw: &str) -> Result<reqwest::Url, String> {
    validate(raw, true)
}

/// A redirect is a new URL chosen by the server, so it gets the same literal
/// and scheme checks as the original. It does not re-resolve: doing blocking
/// DNS inside a redirect callback would stall every hop, and the socket
/// resolver is the system's regardless.
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        match validate_configured_url(attempt.url().as_str()) {
            Ok(_) => attempt.follow(),
            Err(_) => attempt.stop(),
        }
    })
}

fn validate(raw: &str, resolve: bool) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|error| format!("not a URL: {error}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "URLs with embedded credentials are refused; send the token in a header".into(),
        );
    }
    let host = url
        .host_str()
        .ok_or_else(|| "the URL has no host".to_string())?
        .to_ascii_lowercase();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let literal: Option<IpAddr> = bare.parse().ok();
    let explicit_local = literal.is_some_and(|ip| ip.is_loopback())
        || host == "localhost"
        || host.ends_with(".localhost");

    match url.scheme() {
        "https" => {}
        "http" if explicit_local => {}
        other => {
            return Err(format!(
                "scheme {other:?} is not allowed; use https (http is allowed only for an explicit loopback host)"
            ));
        }
    }

    if let Some(ip) = literal {
        if explicit_local {
            return Ok(url);
        }
        if is_disallowed(ip) {
            return Err(format!(
                "host {host} is a private, link-local, reserved or unspecified address"
            ));
        }
        return Ok(url);
    }

    if explicit_local {
        return Ok(url);
    }

    if resolve {
        let port = url.port_or_known_default().unwrap_or(443);
        let mut addrs = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|error| format!("cannot resolve {host}: {error}"))?;
        let first = addrs
            .next()
            .ok_or_else(|| format!("{host} resolved to no addresses"))?;
        for addr in std::iter::once(first).chain(addrs) {
            if addr.ip().is_loopback() || is_disallowed(addr.ip()) {
                return Err(format!(
                    "{host} resolves to a private or loopback address; refusing the request"
                ));
            }
        }
    }

    Ok(url)
}

/// Private, link-local, multicast, unspecified, broadcast and documentation
/// space. Loopback is deliberately **not** here: it is allowed when written
/// literally and refused when it is the answer to a domain lookup.
fn is_disallowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_documentation()
        }
        IpAddr::V6(ip) => {
            ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_https_and_explicit_local_endpoints_are_allowed() {
        for allowed in [
            "https://api.anthropic.com",
            "https://api.example.com/v1",
            "https://127.0.0.1:8788/mcp",
            "http://127.0.0.1:8788/mcp",
            "http://localhost:8788/mcp",
            "http://memnoth.localhost/mcp",
            "https://[::1]:8788",
        ] {
            assert!(
                validate_configured_url(allowed).is_ok(),
                "{allowed} should be allowed"
            );
        }
    }

    #[test]
    fn plain_http_to_a_remote_host_is_refused() {
        let error = validate_configured_url("http://api.example.com").unwrap_err();
        assert!(error.contains("https"), "{error}");
    }

    #[test]
    fn private_link_local_and_metadata_literals_are_refused() {
        for refused in [
            "https://10.0.0.1",
            "https://172.16.0.1",
            "https://192.168.1.1",
            "https://169.254.169.254/latest/meta-data",
            "https://0.0.0.0",
            "https://224.0.0.1",
            "https://192.0.2.10",
            "https://[fe80::1]",
            "https://[fc00::1]",
        ] {
            assert!(
                validate_configured_url(refused).is_err(),
                "{refused} should be refused"
            );
        }
    }

    #[test]
    fn credentials_in_a_url_are_refused() {
        let error = validate_configured_url("https://user:secret@api.example.com").unwrap_err();
        assert!(error.contains("credentials"), "{error}");
    }

    #[test]
    fn a_domain_that_resolves_private_is_refused_before_a_request() {
        // `localhost` is handled as an explicit local host; a domain resolving
        // to loopback like this one must not be. This uses the OS resolver, so
        // it is the same path a request would take.
        let error = validate_before_request("https://localtest.me").unwrap_err();
        assert!(
            error.contains("private or loopback"),
            "expected a refusal naming the resolution, got: {error}"
        );
    }
}

//! Network egress policy: the second half of the first-match decision
//! procedure.
//!
//! Filesystem decisions live in [`crate::backend::decide_record`]; this module
//! answers the other half of the same question — may this command reach that
//! host? — with the same discipline: a fixed, documented rule order, deny
//! first, and a rule id on every answer so an audit line can name what decided.
//!
//! ## What is decided where
//!
//! There are two layers, and they are not the same claim:
//!
//! - **Kernel.** `bwrap --unshare-net` (Linux) and `(deny network*)` (Seatbelt)
//!   are all-or-nothing: when [`NetworkPolicy::Off`] is in force, the confined
//!   child has no interface at all. That is enforcement.
//! - **Tool layer.** [`NetworkPolicy::Allowlist`] cannot be expressed by either
//!   kernel boundary, so it is decided here, in the process that owns the HTTP
//!   client, *and* the kernel still denies everything it cannot name. The result
//!   is stricter than the policy, never looser: a host on the list is reachable
//!   only through a client that asked this module first.
//!
//! A caller that reports an allowlist as "enforced by the OS" would be
//! overstating the boundary; [`crate::status`] therefore prints the allowlist
//! as a declaration and the kernel state separately.
//!
//! ## Order (first match wins)
//!
//! 1. `deny:host-invalid` — empty host, whitespace, path, credentials, or a
//!    non-ASCII name that could not be compared safely.
//! 2. `deny:metadata-endpoint` — a cloud instance-metadata address, whatever the
//!    policy says. `open` is a statement about the internet, not permission to
//!    read this machine's credentials out of a link-local endpoint.
//! 3. `deny:network-off` — policy [`NetworkPolicy::Off`].
//! 4. `allow:network-open` — policy [`NetworkPolicy::Open`].
//! 5. `allow:egress-allowlist` / `deny:egress-allowlist` — policy
//!    [`NetworkPolicy::Allowlist`].
//! 6. `deny:rebinding` — [`recheck_resolved`] only: a name that was allowed by
//!    name and answered with a loopback, private, link-local, unique-local,
//!    multicast, unspecified or documentation address. Checking the literal
//!    spelling of a URL is not enough; the resolver is the thing an attacker
//!    controls when they own the name.

use std::net::IpAddr;

use crate::backend::{BackendPlan, NetworkPolicy};

/// Environment variables that can route a request through a proxy the user did
/// not choose.
///
/// A confined child inherits the parent environment unless someone removes
/// these, and a proxy variable is an egress path that no host allowlist sees:
/// the client connects to the proxy, not to the host it was told about.
/// [`sanitise_child_env`] is the removal, applied to the environment a caller
/// builds for the child rather than by mutating this process (edition 2024
/// marks `env::set_var` unsafe for good reason).
pub const PROXY_ENV_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// Whether a host may be contacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressDecision {
    Allow,
    Deny,
}

/// One egress decision with the rule that made it.
///
/// Same shape as a path decision: the rule id is the audit target, `resolved`
/// is what the name actually answered, and `detail` is the sentence a denial
/// card or `mn sandbox test` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRecord {
    pub outcome: EgressDecision,
    pub rule: String,
    /// The host as requested (`api.example.com`, `[::1]`, `*.internal`).
    pub host: String,
    pub port: Option<u16>,
    /// Addresses the name resolved to, when a resolution was supplied.
    pub resolved: Vec<IpAddr>,
    pub detail: String,
}

impl EgressRecord {
    fn new(
        outcome: EgressDecision,
        rule: &str,
        host: &str,
        port: Option<u16>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            outcome,
            rule: rule.to_string(),
            host: host.to_string(),
            port,
            resolved: Vec::new(),
            detail: detail.into(),
        }
    }

    /// Attach the resolution result (rebinding checks).
    pub fn with_resolved(mut self, resolved: &[IpAddr]) -> Self {
        self.resolved = resolved.to_vec();
        self
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self.outcome, EgressDecision::Allow)
    }
}

/// One allowlist entry: a host, optionally a port, optionally a leading `*.`
/// wildcard that covers exactly one level of subdomain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    /// Lowercased host with the wildcard prefix removed.
    pub host: String,
    pub port: Option<u16>,
    /// `*.example.com` — matches `a.example.com` and `a.b.example.com`, never
    /// `example.com` itself.
    pub wildcard: bool,
}

impl HostSpec {
    /// Parse one allowlist entry.
    ///
    /// Fail-closed: a scheme, a path, credentials, whitespace, a bare `*`, or a
    /// wildcard anywhere but the leftmost label is an error. An entry that
    /// silently meant something broader than it looked is the failure mode this
    /// parser exists to prevent.
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let entry = raw.trim();
        if entry.is_empty() {
            anyhow::bail!("empty network allowlist entry");
        }
        if entry.contains(char::is_whitespace) {
            anyhow::bail!("network allowlist entry {entry:?} contains whitespace");
        }
        if entry.contains("://") || entry.contains('/') || entry.contains('\\') {
            anyhow::bail!(
                "network allowlist entry {entry:?} names a URL or path; write a host, \
                 optionally with :port"
            );
        }
        if entry.contains('@') {
            anyhow::bail!("network allowlist entry {entry:?} carries credentials");
        }
        if entry == "*" {
            anyhow::bail!(
                "`*` is not an allowlist entry; use `network = \"open\"` when every host is \
                 intended"
            );
        }
        let parse_port = |raw: &str| -> anyhow::Result<u16> {
            let port: u16 = raw.parse().map_err(|_| {
                anyhow::anyhow!("network allowlist entry {entry:?} has a port that is not a number")
            })?;
            if port == 0 {
                anyhow::bail!("network allowlist entry {entry:?} names port 0");
            }
            Ok(port)
        };
        // A bracketed IPv6 address carries colons of its own, so the port can
        // only be the part after the closing bracket. Splitting on the last
        // colon first would read `[::1]` as host `[:` plus a port.
        let (host_part, port) = if let Some(rest) = entry.strip_prefix('[') {
            let (addr, tail) = rest.split_once(']').ok_or_else(|| {
                anyhow::anyhow!("network allowlist entry {entry:?} has an unclosed `[`")
            })?;
            let host = format!("[{addr}]");
            match tail {
                "" => (host, None),
                tail => match tail.strip_prefix(':') {
                    Some(raw) => (host, Some(parse_port(raw)?)),
                    None => anyhow::bail!(
                        "network allowlist entry {entry:?}: nothing may follow the closing `]` \
                         except `:port`"
                    ),
                },
            }
        } else {
            match entry.rsplit_once(':') {
                Some((host, raw_port)) if !host.contains(':') => {
                    (host.to_string(), Some(parse_port(raw_port)?))
                }
                _ => (entry.to_string(), None),
            }
        };

        let (wildcard, host) = match host_part.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, host_part.as_str()),
        };
        if host.contains('*') {
            anyhow::bail!(
                "network allowlist entry {entry:?}: a wildcard is only allowed as the leftmost \
                 label (`*.example.com`)"
            );
        }
        let host = host
            .trim_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        if host.is_empty() {
            anyhow::bail!("network allowlist entry {entry:?} has no host");
        }
        if !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        {
            anyhow::bail!(
                "network allowlist entry {entry:?}: hosts must be ASCII letters, digits, `.`, \
                 `-`, or an IPv6 address in brackets"
            );
        }
        if wildcard && host.parse::<IpAddr>().is_ok() {
            anyhow::bail!("network allowlist entry {entry:?}: an IP address cannot be a wildcard");
        }
        Ok(Self {
            host,
            port,
            wildcard,
        })
    }

    /// Whether this entry covers `host` at `port`.
    pub fn matches(&self, host: &str, port: Option<u16>) -> bool {
        let host = host
            .trim_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if let Some(entry_port) = self.port
            && Some(entry_port) != port
        {
            return false;
        }
        if self.wildcard {
            // At least one label in front, and the boundary is a dot: `evilexample.com`
            // must not match `*.example.com`.
            return host.len() > self.host.len() + 1 && host.ends_with(&format!(".{}", self.host));
        }
        host == self.host
    }
}

/// The egress half of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPlan {
    pub policy: NetworkPolicy,
    pub allowlist: Vec<HostSpec>,
}

impl EgressPlan {
    /// Build from a plan. An allowlist entry that does not parse is a hard
    /// error: refusing to run beats running with a policy nobody can state.
    pub fn from_backend_plan(plan: &BackendPlan) -> anyhow::Result<Self> {
        let policy = plan.effective_network();
        let allowlist = match &policy {
            NetworkPolicy::Allowlist(hosts) => hosts
                .iter()
                .map(|host| {
                    HostSpec::parse(host)
                        .map_err(|error| anyhow::anyhow!("profile '{}': {error}", plan.profile))
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
            _ => Vec::new(),
        };
        Ok(Self { policy, allowlist })
    }

    /// Decide one host without resolving it.
    pub fn decide(&self, host: &str, port: Option<u16>) -> EgressRecord {
        decide(&self.policy, &self.allowlist, host, port)
    }

    /// Decide one host and re-check the addresses it resolved to.
    pub fn recheck_resolved(
        &self,
        host: &str,
        port: Option<u16>,
        resolved: &[IpAddr],
    ) -> EgressRecord {
        recheck_resolved(&self.policy, &self.allowlist, host, port, resolved)
    }
}

/// Decide one host under `policy` with a pre-parsed allowlist.
pub fn decide(
    policy: &NetworkPolicy,
    allowlist: &[HostSpec],
    host: &str,
    port: Option<u16>,
) -> EgressRecord {
    let bare = host
        .trim()
        .trim_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.');

    if let Some(rule) = invalid_host_rule(bare) {
        return EgressRecord::new(
            EgressDecision::Deny,
            rule,
            host,
            port,
            format!("{host:?} is not a host this policy can decide"),
        );
    }

    if let Ok(ip) = bare.parse::<IpAddr>()
        && is_metadata_endpoint(ip)
    {
        return EgressRecord::new(
            EgressDecision::Deny,
            "deny:metadata-endpoint",
            host,
            port,
            format!(
                "{ip} is a cloud instance-metadata address; reaching it is credential theft, \
                 not egress"
            ),
        );
    }

    match policy {
        NetworkPolicy::Off => EgressRecord::new(
            EgressDecision::Deny,
            "deny:network-off",
            host,
            port,
            "the profile denies child egress (network = \"off\")".to_string(),
        ),
        NetworkPolicy::Open => EgressRecord::new(
            EgressDecision::Allow,
            "allow:network-open",
            host,
            port,
            "network = \"open\": egress is not filtered".to_string(),
        ),
        NetworkPolicy::Allowlist(_) => {
            if let Some(entry) = allowlist.iter().find(|entry| entry.matches(bare, port)) {
                EgressRecord::new(
                    EgressDecision::Allow,
                    "allow:egress-allowlist",
                    host,
                    port,
                    format!(
                        "{host} matches allowlist entry `{}{}`",
                        if entry.wildcard { "*." } else { "" },
                        entry.host
                    ),
                )
            } else {
                EgressRecord::new(
                    EgressDecision::Deny,
                    "deny:egress-allowlist",
                    host,
                    port,
                    format!("{host} is not on the network allowlist"),
                )
            }
        }
    }
}

/// Decide one host, then re-check the addresses the resolver returned.
///
/// This is the DNS-rebinding half: a name that is on the allowlist is allowed
/// by name, but if it answers with a loopback or private address the request
/// would reach this machine's network, not the internet. Because the answer can
/// differ between the check and the connection, callers pass the addresses they
/// resolved immediately before connecting.
pub fn recheck_resolved(
    policy: &NetworkPolicy,
    allowlist: &[HostSpec],
    host: &str,
    port: Option<u16>,
    resolved: &[IpAddr],
) -> EgressRecord {
    let first = decide(policy, allowlist, host, port);
    if !first.is_allowed() || resolved.is_empty() {
        return first.with_resolved(resolved);
    }

    // A literal address is its own resolution; it was already judged above.
    if host
        .trim()
        .trim_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok()
    {
        return first.with_resolved(resolved);
    }

    for ip in resolved {
        if is_metadata_endpoint(*ip) {
            return EgressRecord::new(
                EgressDecision::Deny,
                "deny:metadata-endpoint",
                host,
                port,
                format!("{host} resolves to the metadata address {ip}"),
            )
            .with_resolved(resolved);
        }
        if let NetworkPolicy::Allowlist(_) = policy
            && is_local(ip)
        {
            return EgressRecord::new(
                EgressDecision::Deny,
                "deny:rebinding",
                host,
                port,
                format!(
                    "{host} is on the allowlist but resolved to {ip}, a loopback or private \
                     address (possible DNS rebinding)"
                ),
            )
            .with_resolved(resolved);
        }
    }

    first.with_resolved(resolved)
}

/// Why a host spelling cannot be decided, if it cannot.
fn invalid_host_rule(host: &str) -> Option<&'static str> {
    if host.is_empty() {
        return Some("deny:host-invalid");
    }
    if host.contains(char::is_whitespace)
        || host.contains('/')
        || host.contains('\\')
        || host.contains('@')
        || !host.is_ascii()
    {
        return Some("deny:host-invalid");
    }
    // A bare `*` is not a host: the policy that means "everything" is `open`.
    if host == "*" || host.contains('*') {
        return Some("deny:host-invalid");
    }
    None
}

/// Loopback, private, link-local, unique-local, multicast, unspecified and
/// documentation space.
fn is_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_documentation()
                || ip.octets()[0] == 0
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
            // IPv4-mapped forms (`::ffff:169.254.169.254`) must be judged by
            // their IPv4 meaning, not slip past as an opaque v6 address.
                || ip.to_ipv4_mapped().is_some_and(|v4| is_local(&IpAddr::V4(v4)))
        }
    }
}

/// Cloud instance-metadata endpoints, refused under every policy.
fn is_metadata_endpoint(ip: IpAddr) -> bool {
    const V4: &[&str] = &[
        "169.254.169.254", // AWS, GCP, Azure, OpenStack, DigitalOcean
        "169.254.170.2",   // ECS task metadata (container credentials)
        "100.100.100.200", // Alibaba Cloud
    ];
    match ip {
        IpAddr::V4(v4) => V4.iter().any(|entry| {
            entry
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|addr| addr == v4)
        }),
        IpAddr::V6(v6) => {
            // AWS IMDS over IPv6.
            v6.segments() == [0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x254]
        }
    }
}

/// Remove the proxy variables from an environment a child is about to receive.
///
/// The sandbox cannot stop a proxy variable from being *read* by a child that
/// inherits it; it can stop the child from inheriting it. Callers that build a
/// confined child's environment pass their pairs through here.
pub fn sanitise_child_env(env: &[(String, String)]) -> Vec<(String, String)> {
    env.iter()
        .filter(|(key, _)| {
            !PROXY_ENV_VARS
                .iter()
                .any(|proxy| proxy.eq_ignore_ascii_case(key))
        })
        .cloned()
        .collect()
}

/// Which proxy variables are still set in this process, sorted.
///
/// `doctor` prints this: a set variable is not a policy violation by itself
/// (the CLI removes them at startup), but a confined child that kept one would
/// be an egress path no allowlist sees, so it is a fact worth stating.
pub fn proxy_env_present() -> Vec<String> {
    let mut present: Vec<String> = PROXY_ENV_VARS
        .iter()
        .filter(|name| std::env::var_os(name).is_some())
        .map(|name| (*name).to_string())
        .collect();
    present.sort();
    present.dedup();
    present
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn allowlist(entries: &[&str]) -> Vec<HostSpec> {
        entries
            .iter()
            .map(|e| HostSpec::parse(e).unwrap())
            .collect()
    }

    #[test]
    fn off_denies_every_host_that_is_not_a_metadata_endpoint() {
        let record = decide(&NetworkPolicy::Off, &[], "api.example.com", Some(443));
        assert_eq!(record.outcome, EgressDecision::Deny);
        assert_eq!(record.rule, "deny:network-off");
    }

    #[test]
    fn open_allows_internet_but_never_the_metadata_endpoint() {
        let record = decide(&NetworkPolicy::Open, &[], "api.example.com", Some(443));
        assert!(record.is_allowed());
        assert_eq!(record.rule, "allow:network-open");

        let metadata = decide(&NetworkPolicy::Open, &[], "169.254.169.254", Some(80));
        assert_eq!(metadata.outcome, EgressDecision::Deny);
        assert_eq!(metadata.rule, "deny:metadata-endpoint");
    }

    #[test]
    fn an_allowlist_allows_only_named_hosts() {
        let policy = NetworkPolicy::Allowlist(vec!["api.example.com".into()]);
        let list = allowlist(&["api.example.com"]);
        assert!(decide(&policy, &list, "api.example.com", Some(443)).is_allowed());
        let denied = decide(&policy, &list, "evil.example.com", Some(443));
        assert_eq!(denied.rule, "deny:egress-allowlist");
    }

    #[test]
    fn a_wildcard_covers_subdomains_and_not_the_lookalike() {
        let spec = HostSpec::parse("*.example.com").unwrap();
        assert!(spec.matches("api.example.com", None));
        assert!(spec.matches("a.b.example.com", None));
        assert!(!spec.matches("example.com", None));
        assert!(!spec.matches("evilexample.com", None));
    }

    #[test]
    fn a_port_on_the_entry_must_match_exactly() {
        let spec = HostSpec::parse("api.example.com:8443").unwrap();
        assert!(spec.matches("api.example.com", Some(8443)));
        assert!(!spec.matches("api.example.com", Some(443)));
        assert!(!spec.matches("api.example.com", None));
    }

    #[test]
    fn allowlist_entries_that_cannot_be_stated_are_refused() {
        for bad in [
            "",
            " * ",
            "*",
            "https://example.com",
            "example.com/path",
            "user:pw@example.com",
            "api.*.example.com",
            "*.10.0.0.1",
            "example.com:0",
            "example.com:banana",
            "exa mple.com",
        ] {
            assert!(HostSpec::parse(bad).is_err(), "`{bad}` must not parse");
        }
    }

    #[test]
    fn a_rebound_name_is_denied_even_though_it_was_on_the_list() {
        let policy = NetworkPolicy::Allowlist(vec!["api.example.com".into()]);
        let list = allowlist(&["api.example.com"]);
        let rebound = recheck_resolved(
            &policy,
            &list,
            "api.example.com",
            Some(443),
            &[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))],
        );
        assert_eq!(rebound.outcome, EgressDecision::Deny);
        assert_eq!(rebound.rule, "deny:rebinding");
        assert_eq!(rebound.resolved.len(), 1);
    }

    #[test]
    fn a_name_that_resolves_to_a_public_address_stays_allowed() {
        let policy = NetworkPolicy::Allowlist(vec!["api.example.com".into()]);
        let list = allowlist(&["api.example.com"]);
        let record = recheck_resolved(
            &policy,
            &list,
            "api.example.com",
            Some(443),
            &[IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))],
        );
        assert!(record.is_allowed(), "{}", record.detail);
    }

    #[test]
    fn a_name_that_resolves_to_metadata_is_denied_even_under_open() {
        let record = recheck_resolved(
            &NetworkPolicy::Open,
            &[],
            "metadata.example.com",
            Some(80),
            &[IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))],
        );
        assert_eq!(record.outcome, EgressDecision::Deny);
        assert_eq!(record.rule, "deny:metadata-endpoint");
    }

    #[test]
    fn open_keeps_loopback_usable_for_local_development() {
        let record = recheck_resolved(
            &NetworkPolicy::Open,
            &[],
            "localhost",
            Some(8788),
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        );
        assert!(record.is_allowed(), "{}", record.detail);
    }

    #[test]
    fn a_bracketed_ipv6_entry_keeps_its_colons_and_its_port() {
        let bare = HostSpec::parse("[::1]").unwrap();
        assert_eq!(bare.host, "::1");
        assert_eq!(bare.port, None);
        assert!(bare.matches("[::1]", None));

        let with_port = HostSpec::parse("[fd00::1]:8443").unwrap();
        assert_eq!(with_port.port, Some(8443));
        assert!(with_port.matches("[fd00::1]", Some(8443)));
        assert!(!with_port.matches("[fd00::1]", Some(443)));
        assert!(HostSpec::parse("[::1").is_err());
    }

    #[test]
    fn proxy_variables_do_not_reach_a_child_environment() {
        let env = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HTTPS_PROXY".to_string(), "http://evil:8080".to_string()),
            ("no_proxy".to_string(), "*".to_string()),
        ];
        let sanitised = sanitise_child_env(&env);
        assert_eq!(sanitised.len(), 1);
        assert_eq!(sanitised[0].0, "PATH");
    }

    #[test]
    fn an_invalid_host_is_denied_before_the_policy_is_consulted() {
        for bad in ["", "api example.com", "https://example.com", "*"] {
            let record = decide(&NetworkPolicy::Open, &[], bad, Some(443));
            assert_eq!(record.outcome, EgressDecision::Deny, "`{bad}`");
        }
    }
}

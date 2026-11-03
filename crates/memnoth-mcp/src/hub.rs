//! M3 hub — memnoth as a gateway to N downstream MCP servers.
//!
//! The MCP-SURFACE lane's kill map says memnoth should sit *alongside* the
//! incumbent servers a host already runs, not replace them. That means three
//! things this module owns, all of them offline and deterministic:
//!
//! 1. **Routing** — N downstream servers expose their tools through one surface.
//!    Tool names are namespaced `server__tool`, and a name collision is a
//!    *conflict that is recorded and refused*, never a silent overwrite: two
//!    servers claiming `search` must not leave the host wondering which one
//!    answered.
//! 2. **Claims** — a downstream server's namespace is claimed by the session
//!    that publishes it, and only that session may replace it. This is the
//!    `coord_claim` discipline applied to tool routing: capability registration
//!    is a write, and writes are owned.
//! 3. **Trust** — a server's tool surface is attested by a receipt
//!    (MCPRadar / SBOM / Glama / Smithery). The receipt is verified against the
//!    *actual* tool list by digest, and a server with no receipt is reported
//!    `unverified` rather than implicitly trusted. Plus npm hygiene over the
//!    launch line (`npx -y pkg@latest` is a moving target), which is the same
//!    supply-chain question the doctor asks.
//!
//! Deliberately not here: connecting to the downstream servers. The transport
//! table and the client own that; this module owns what the hub *knows*, so its
//! rules are testable without spawning anything.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

/// A tool as a downstream server advertises it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownstreamTool {
    /// The tool name as the downstream server advertises it, before namespacing.
    pub name: String,
    /// The server's own one-line description; part of the attestation digest.
    pub description: String,
}

/// A downstream server's advertised surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownstreamServer {
    /// The server's configured name — also its claim key and namespace prefix.
    pub name: String,
    /// The transport scheme it was reached over (`stdio`, `http`, …), as
    /// resolved by the transport table.
    pub transport: String,
    /// Tools it advertised, in the order it listed them (the digest sorts).
    pub tools: Vec<DownstreamTool>,
    /// Capability names it declared (`resources`, `prompts`, `tools`, …).
    pub capabilities: Vec<String>,
}

impl DownstreamServer {
    /// A server with no tools or capabilities yet — the shape a connect path
    /// fills in as the downstream handshake reports them.
    pub fn new(name: &str, transport: &str) -> Self {
        Self {
            name: name.to_string(),
            transport: transport.to_string(),
            tools: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    /// Builder step: append one advertised tool. Chainable, and order is kept.
    pub fn with_tool(mut self, name: &str, description: &str) -> Self {
        self.tools.push(DownstreamTool {
            name: name.to_string(),
            description: description.to_string(),
        });
        self
    }
}

/// The separator between a downstream server and its tool name.
///
/// Two underscores is the convention hosts already use for MCP namespacing, and
/// it is *refused* inside a tool name for exactly that reason: `a__b__c` would
/// otherwise parse two ways.
pub const NAMESPACE_SEPARATOR: &str = "__";

/// A name collision that was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The name that was wanted and could not be had.
    pub exposed: String,
    /// The server that already owned the name.
    pub held_by: String,
    /// The server whose publish was refused.
    pub refused_from: String,
}

/// How a downstream tool is exposed through the hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    /// `server__tool`. Collision-free across servers *by construction* — two
    /// servers cannot produce the same name — so the only collisions left are a
    /// server advertising one tool name twice.
    Prefixed,
    /// The bare tool name, for a host that wants the merged surface to read like
    /// one server. This is where a real cross-server collision lives, and where
    /// first-registrant-wins has to be explicit: two servers both offering
    /// `search` must not silently swap which one a host is calling.
    Flat,
}

impl Namespace {
    fn exposed(self, server: &str, tool: &str) -> String {
        match self {
            Namespace::Prefixed => format!("{server}{NAMESPACE_SEPARATOR}{tool}"),
            Namespace::Flat => tool.to_string(),
        }
    }
}

/// What a publish did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishReport {
    /// The server whose surface this report describes.
    pub server: String,
    /// `server__tool` names newly exposed by this publish.
    pub exposed: Vec<String>,
    /// Names the publish wanted but could not have (already held).
    pub refused: Vec<String>,
}

/// The routing table plus its claim book.
#[derive(Debug, Default)]
pub struct HubRegistry {
    /// downstream server name → the session that claimed it.
    claims: BTreeMap<String, String>,
    /// exposed name → (server, tool).
    routes: BTreeMap<String, (String, String)>,
    servers: BTreeMap<String, DownstreamServer>,
    conflicts: Vec<Conflict>,
}

impl HubRegistry {
    /// An empty hub: no claims, no routes, no conflicts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim a downstream server's namespace for `session`.
    ///
    /// A claim held by another session is refused **and named** — the caller
    /// needs to know who to coordinate with, which is the difference between a
    /// retryable conflict and a mystery.
    pub fn claim(&mut self, server: &str, session: &str) -> Result<()> {
        if server.trim().is_empty() {
            bail!("hub claim needs a server name");
        }
        if let Some(holder) = self.claims.get(server) {
            if holder != session {
                bail!("hub namespace '{server}' is claimed by session '{holder}'");
            }
            return Ok(());
        }
        self.claims.insert(server.to_string(), session.to_string());
        Ok(())
    }

    /// Release a claim. Only the holder may release it.
    pub fn release(&mut self, server: &str, session: &str) -> Result<()> {
        match self.claims.get(server) {
            None => Ok(()),
            Some(holder) if holder == session => {
                self.claims.remove(server);
                self.servers.remove(server);
                self.routes.retain(|_, (srv, _)| srv.as_str() != server);
                Ok(())
            }
            Some(holder) => bail!("hub namespace '{server}' is claimed by session '{holder}'"),
        }
    }

    /// The session currently holding `server`'s claim, if any.
    pub fn holder(&self, server: &str) -> Option<&str> {
        self.claims.get(server).map(String::as_str)
    }

    /// Publish (or replace) a server's surface under its claim.
    ///
    /// Replacement is atomic for the publishing session: the server's old routes
    /// are dropped before the new ones are inserted, so a renamed tool cannot
    /// leave a route to a tool that no longer exists. First-registrant-wins for
    /// a name that another server already exposes.
    pub fn publish(&mut self, server: DownstreamServer, session: &str) -> Result<PublishReport> {
        self.publish_with(server, session, Namespace::Prefixed)
    }

    /// [`HubRegistry::publish`] under an explicit namespace mode.
    pub fn publish_with(
        &mut self,
        server: DownstreamServer,
        session: &str,
        namespace: Namespace,
    ) -> Result<PublishReport> {
        self.claim(&server.name, session)?;
        for tool in &server.tools {
            if tool.name.contains(NAMESPACE_SEPARATOR) {
                bail!(
                    "tool '{}' of '{}' contains the namespace separator '{NAMESPACE_SEPARATOR}'; \
                     it would be unroutable",
                    tool.name,
                    server.name
                );
            }
        }
        // Drop this server's previous routes first (replace, not merge).
        let name = server.name.clone();
        self.routes
            .retain(|_, (srv, _)| srv.as_str() != name.as_str());
        let mut exposed = Vec::new();
        let mut refused = Vec::new();
        for tool in &server.tools {
            let exposed_name = namespace.exposed(&server.name, &tool.name);
            match self.routes.get(&exposed_name) {
                Some((holder, _)) => {
                    self.conflicts.push(Conflict {
                        exposed: exposed_name.clone(),
                        held_by: holder.clone(),
                        refused_from: server.name.clone(),
                    });
                    refused.push(exposed_name);
                }
                None => {
                    self.routes.insert(
                        exposed_name.clone(),
                        (server.name.clone(), tool.name.clone()),
                    );
                    exposed.push(exposed_name);
                }
            }
        }
        self.servers.insert(server.name.clone(), server);
        Ok(PublishReport {
            server: name,
            exposed,
            refused,
        })
    }

    /// Resolve an exposed name to `(server, tool)`.
    pub fn route(&self, exposed: &str) -> Option<(&str, &str)> {
        self.routes
            .get(exposed)
            .map(|(s, t)| (s.as_str(), t.as_str()))
    }

    /// Every exposed tool, sorted — the surface a host would be shown.
    pub fn exposed_tools(&self) -> Vec<String> {
        self.routes.keys().cloned().collect()
    }

    /// Every published server, in name order (the map is a `BTreeMap`).
    pub fn servers(&self) -> Vec<&DownstreamServer> {
        self.servers.values().collect()
    }

    /// Every refused collision so far, in the order it was refused.
    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }
}

// ---------------------------------------------------------------------------
// Trust receipts
// ---------------------------------------------------------------------------

/// The registries a receipt may come from. Named rather than a free string: a
/// typo'd source is not a weaker attestation, it is an unknown one, and the
/// report must say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Registry {
    /// MCPRadar's listing.
    McpRadar,
    /// A CycloneDX/SPDX software bill of materials.
    Sbom,
    /// Glama's directory entry.
    Glama,
    /// Smithery's directory entry.
    Smithery,
}

impl Registry {
    /// The stable lowercase id used in reports and receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            Registry::McpRadar => "mcpradar",
            Registry::Sbom => "sbom",
            Registry::Glama => "glama",
            Registry::Smithery => "smithery",
        }
    }

    /// Parse a receipt's `source` field. Unknown names are `None` on purpose:
    /// a typo'd registry is an unknown attestation, not a weaker one.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mcpradar" | "mcp-radar" => Some(Registry::McpRadar),
            "sbom" | "cyclonedx" | "spdx" => Some(Registry::Sbom),
            "glama" => Some(Registry::Glama),
            "smithery" => Some(Registry::Smithery),
            _ => None,
        }
    }
}

/// A third party's attestation that a server exposes exactly these tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustReceipt {
    /// The server name the attestation is about.
    pub server: String,
    /// Which registry issued it.
    pub registry: Registry,
    /// [`tool_digest`] of the attested tool list.
    pub digest: String,
    /// Where the attestation can be read, when the registry publishes a URL.
    pub source_url: Option<String>,
}

/// Deterministic digest of a tool surface: sorted `name\ndescription` pairs,
/// FNV-1a, hex. Sorted because a server may list tools in any order and an
/// attestation must not depend on that order.
pub fn tool_digest(tools: &[DownstreamTool]) -> String {
    let mut rows: Vec<String> = tools
        .iter()
        .map(|t| format!("{}\n{}", t.name, t.description))
        .collect();
    rows.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for row in rows {
        for b in row.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= b'\n' as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// What a trust sweep found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustReport {
    /// Servers whose receipt digest matches the tools they advertise.
    pub verified: Vec<String>,
    /// Servers with no receipt at all. Not an accusation — an admission.
    pub unverified: Vec<String>,
    /// Servers whose receipt does not match the tools they actually advertise.
    pub mismatched: Vec<(String, String)>,
}

impl TrustReport {
    /// True only when every server was verified — no absences, no mismatches.
    pub fn is_clean(&self) -> bool {
        self.unverified.is_empty() && self.mismatched.is_empty()
    }
}

/// Verify each server against its receipts: `verified` needs a digest match,
/// anything else is reported by name with the reason.
pub fn trust_report(servers: &[DownstreamServer], receipts: &[TrustReceipt]) -> TrustReport {
    let mut report = TrustReport::default();
    for server in servers {
        let mine: Vec<&TrustReceipt> = receipts
            .iter()
            .filter(|r| r.server == server.name)
            .collect();
        if mine.is_empty() {
            report.unverified.push(server.name.clone());
            continue;
        }
        let digest = tool_digest(&server.tools);
        if mine.iter().any(|r| r.digest == digest) {
            report.verified.push(server.name.clone());
        } else {
            report.mismatched.push((
                server.name.clone(),
                format!("receipt digest {} != actual {digest}", mine[0].digest),
            ));
        }
    }
    report.verified.sort();
    report.unverified.sort();
    report.mismatched.sort();
    report
}

// ---------------------------------------------------------------------------
// npm hygiene
// ---------------------------------------------------------------------------

/// One npm-style launch spec, as it appears on a server's `command`/`args` line.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HygieneFinding {
    /// No version at all: the launch resolves to whatever is latest today.
    Unpinned {
        /// The package whose requirement carries no version.
        package: String,
    },
    /// A range (`^1.2`, `~1.2`, `*`, `latest`) — still moving.
    RangeInsteadOfPin {
        /// The package name.
        package: String,
        /// The moving requirement it declared, verbatim.
        version: String,
    },
    /// Not scoped (`@scope/pkg`), so the name is squattable.
    Unscoped {
        /// The unscoped package name.
        package: String,
    },
    /// A git/url dependency: no registry attestation applies at all.
    GitDependency {
        /// The package named on the launch line.
        package: String,
    },
    /// The launch line and the server name disagree — which of the two is the
    /// identity a receipt would be checked against?
    NameMismatch {
        /// The package the launch line would run.
        package: String,
        /// The server name that package advertises, which disagrees.
        server: String,
    },
}

impl HygieneFinding {
    /// One human-readable line, used by the doctor report.
    pub fn as_str(&self) -> String {
        match self {
            HygieneFinding::Unpinned { package } => format!("unpinned: {package}"),
            HygieneFinding::RangeInsteadOfPin { package, version } => {
                format!("range not pin: {package}@{version}")
            }
            HygieneFinding::Unscoped { package } => format!("unscoped: {package}"),
            HygieneFinding::GitDependency { package } => format!("git dependency: {package}"),
            HygieneFinding::NameMismatch { package, server } => {
                format!("name mismatch: {package} vs server '{server}'")
            }
        }
    }
}

/// Extract the npm package spec from a launch line, if it is an npx-style one.
///
/// Recognises `npx`/`npm exec`/`pnpx`/`bunx` launches and takes the first
/// non-flag argument after the runner. Returns `None` for anything else (a
/// local binary, a python module): those have no npm attestation to check, and
/// inventing one would be worse than saying nothing.
pub fn package_spec(command: &str, args: &[String]) -> Option<String> {
    let cmd = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_end_matches(".cmd")
        .trim_end_matches(".exe");
    let runner = ["npx", "npm", "pnpx", "bunx", "pnpm", "yarn"].contains(&cmd);
    if !runner {
        return None;
    }
    for arg in args {
        if arg == "exec" || arg == "dlx" {
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.clone());
    }
    None
}

/// Split `@scope/pkg@1.2.3` into `(name, version)` without mis-reading the scope
/// `@` as a version separator.
pub fn split_spec(spec: &str) -> (String, Option<String>) {
    let at = spec.rfind('@').filter(|i| *i > 0);
    match at {
        Some(i) => (spec[..i].to_string(), Some(spec[i + 1..].to_string())),
        None => (spec.to_string(), None),
    }
}

/// Findings for one server's launch line, sorted for determinism.
pub fn npm_hygiene(server: &str, command: &str, args: &[String]) -> Vec<HygieneFinding> {
    let Some(spec) = package_spec(command, args) else {
        return Vec::new();
    };
    let (package, version) = split_spec(&spec);
    let mut findings = Vec::new();
    if package.starts_with("git+") || package.contains("://") || package.contains("github:") {
        findings.push(HygieneFinding::GitDependency { package });
        findings.sort();
        return findings;
    }
    match version.as_deref() {
        None => findings.push(HygieneFinding::Unpinned {
            package: package.clone(),
        }),
        Some(v)
            if v.is_empty()
                || v == "latest"
                || v == "*"
                || v.starts_with('^')
                || v.starts_with('~')
                || v.starts_with('>')
                || v.starts_with('<')
                || v.contains('|')
                || v.contains(' ') =>
        {
            findings.push(HygieneFinding::RangeInsteadOfPin {
                package: package.clone(),
                version: v.to_string(),
            });
        }
        Some(_) => {}
    }
    if !package.starts_with('@') {
        findings.push(HygieneFinding::Unscoped {
            package: package.clone(),
        });
    }
    // `@modelcontextprotocol/server-filesystem` vs a server named
    // `filesystem` is the common, legitimate case, so the comparison is on the
    // final path segment and only fires when neither direction matches.
    let base = package
        .rsplit('/')
        .next()
        .unwrap_or(&package)
        .to_ascii_lowercase();
    let server_l = server.to_ascii_lowercase();
    if !base.contains(&server_l) && !server_l.contains(&base) {
        findings.push(HygieneFinding::NameMismatch {
            package: package.clone(),
            server: server.to_string(),
        });
    }
    findings.sort();
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs_server() -> DownstreamServer {
        DownstreamServer::new("filesystem", "stdio")
            .with_tool("read_file", "read a file")
            .with_tool("write_file", "write a file")
    }

    fn search_server() -> DownstreamServer {
        DownstreamServer::new("search", "http").with_tool("search", "web search")
    }

    #[test]
    fn publishing_requires_a_claim_and_the_claim_names_its_holder() {
        let mut hub = HubRegistry::new();
        hub.claim("filesystem", "sess-a").unwrap();
        let err = hub.claim("filesystem", "sess-b").unwrap_err().to_string();
        assert!(err.contains("sess-a"), "the holder must be named: {err}");

        // A second session cannot publish under someone else's claim. It must
        // name the same server, or the claim it takes is its own (that is the
        // difference between a conflict and a second, legitimate namespace).
        let err = hub.publish(fs_server(), "sess-b").unwrap_err().to_string();
        assert!(err.contains("claimed by"), "{err}");

        // The holder can, and re-claiming its own namespace is idempotent.
        hub.claim("filesystem", "sess-a").unwrap();
        let report = hub.publish(fs_server(), "sess-a").unwrap();
        assert_eq!(
            report.exposed,
            vec!["filesystem__read_file", "filesystem__write_file"]
        );
        assert_eq!(
            hub.route("filesystem__read_file"),
            Some(("filesystem", "read_file"))
        );
    }

    #[test]
    fn a_collision_is_refused_and_recorded_not_silently_overwritten() {
        // Prefixed names cannot collide across servers by construction, so the
        // collision that *can* happen there is a server advertising one tool
        // name twice.
        let doubled = DownstreamServer::new("echo", "stdio")
            .with_tool("search", "first")
            .with_tool("search", "second");
        let mut hub = HubRegistry::new();
        let report = hub.publish(doubled, "sess-a").unwrap();
        assert_eq!(report.exposed, vec!["echo__search"]);
        assert_eq!(report.refused, vec!["echo__search"]);
        assert_eq!(hub.conflicts().len(), 1);
        assert_eq!(hub.conflicts()[0].held_by, "echo");
        // Both tools are named `search`, so the route's tool name cannot
        // distinguish them — which is exactly why the duplicate is refused
        // instead of silently replacing the first one: nothing downstream
        // could tell which registration answered.
        assert_eq!(hub.route("echo__search"), Some(("echo", "search")));

        // Flat mode is where two *different* servers can collide: first
        // registrant wins, the second is refused and the refusal is recorded.
        let mut flat = HubRegistry::new();
        flat.publish_with(
            DownstreamServer::new("echo", "stdio").with_tool("search", "echo"),
            "sess-a",
            Namespace::Flat,
        )
        .unwrap();
        let report = flat
            .publish_with(search_server(), "sess-a", Namespace::Flat)
            .unwrap();
        assert!(report.exposed.is_empty());
        assert_eq!(report.refused, vec!["search"]);
        assert_eq!(flat.conflicts().len(), 1);
        assert_eq!(flat.conflicts()[0].held_by, "echo");
        assert_eq!(flat.conflicts()[0].refused_from, "search");
        assert_eq!(flat.route("search"), Some(("echo", "search")));
    }

    #[test]
    fn republish_replaces_rather_than_merges() {
        let mut hub = HubRegistry::new();
        hub.publish(fs_server(), "sess-a").unwrap();
        let renamed = DownstreamServer::new("filesystem", "stdio").with_tool("read", "read a file");
        let report = hub.publish(renamed, "sess-a").unwrap();
        assert_eq!(report.exposed, vec!["filesystem__read"]);
        assert_eq!(
            hub.route("filesystem__read_file"),
            None,
            "a replaced surface must not leave a route behind"
        );
        assert_eq!(hub.exposed_tools(), vec!["filesystem__read"]);
    }

    #[test]
    fn release_takes_the_routes_with_it_and_only_for_the_holder() {
        let mut hub = HubRegistry::new();
        hub.publish(fs_server(), "sess-a").unwrap();
        assert!(hub.release("filesystem", "sess-b").is_err());
        assert_eq!(hub.exposed_tools().len(), 2);
        hub.release("filesystem", "sess-a").unwrap();
        assert!(hub.exposed_tools().is_empty());
        assert!(hub.servers().is_empty());
        assert_eq!(hub.route("filesystem__read_file"), None);
    }

    #[test]
    fn a_tool_name_carrying_the_separator_is_refused_as_unroutable() {
        let mut hub = HubRegistry::new();
        let bad = DownstreamServer::new("srv", "stdio").with_tool("a__b", "ambiguous");
        let err = hub.publish(bad, "sess-a").unwrap_err().to_string();
        assert!(err.contains("unroutable"), "{err}");
    }

    #[test]
    fn receipts_verify_by_digest_and_absence_is_reported_not_trusted() {
        let fs = fs_server();
        let good = TrustReceipt {
            server: "filesystem".into(),
            registry: Registry::McpRadar,
            digest: tool_digest(&fs.tools),
            source_url: Some("https://mcpradar.example/filesystem".into()),
        };
        let report = trust_report(std::slice::from_ref(&fs), std::slice::from_ref(&good));
        assert_eq!(report.verified, vec!["filesystem"]);
        assert!(report.is_clean());

        // A receipt for a *different* surface is a mismatch, not a pass.
        let stale = TrustReceipt {
            digest: tool_digest(&search_server().tools),
            ..good.clone()
        };
        let report = trust_report(std::slice::from_ref(&fs), std::slice::from_ref(&stale));
        assert_eq!(report.verified.len(), 0);
        assert_eq!(report.mismatched.len(), 1);
        assert!(report.mismatched[0].1.contains("!= actual"));

        // No receipt at all: unverified, named.
        let report = trust_report(&[fs, search_server()], &[]);
        assert_eq!(report.unverified, vec!["filesystem", "search"]);
        assert!(!report.is_clean());
    }

    #[test]
    fn digest_is_order_independent_and_description_sensitive() {
        let a = vec![
            DownstreamTool {
                name: "b".into(),
                description: "second".into(),
            },
            DownstreamTool {
                name: "a".into(),
                description: "first".into(),
            },
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(tool_digest(&a), tool_digest(&b), "order must not matter");
        let mut c = a.clone();
        c[0].description.push('!');
        assert_ne!(tool_digest(&a), tool_digest(&c));
    }

    #[test]
    fn registry_names_parse_and_unknown_ones_do_not() {
        assert_eq!(Registry::parse("MCPRadar"), Some(Registry::McpRadar));
        assert_eq!(Registry::parse("cyclonedx"), Some(Registry::Sbom));
        assert_eq!(Registry::parse("smithery"), Some(Registry::Smithery));
        assert_eq!(Registry::parse("glama"), Some(Registry::Glama));
        assert_eq!(Registry::parse("some-blog"), None);
        assert_eq!(Registry::McpRadar.as_str(), "mcpradar");
    }

    #[test]
    fn npm_hygiene_flags_the_moving_targets() {
        // A pinned, scoped package on a name that matches: clean.
        let clean = npm_hygiene(
            "filesystem",
            "npx",
            &[
                "-y".into(),
                "@modelcontextprotocol/server-filesystem@1.2.3".into(),
            ],
        );
        assert!(clean.is_empty(), "{clean:?}");

        // `@latest` is a moving target even though it has a version.
        let latest = npm_hygiene(
            "filesystem",
            "npx",
            &["-y".into(), "@scope/server-filesystem@latest".into()],
        );
        assert!(latest.iter().any(|f| matches!(
            f,
            HygieneFinding::RangeInsteadOfPin { version, .. } if version == "latest"
        )));

        // No version at all.
        let unpinned = npm_hygiene("thing", "npx", &["-y".into(), "@scope/thing".into()]);
        assert!(unpinned.contains(&HygieneFinding::Unpinned {
            package: "@scope/thing".into()
        }));

        // Unscoped AND name-mismatched, both reported.
        let messy = npm_hygiene("other", "npx", &["-y".into(), "whatever@1.0.0".into()]);
        assert!(messy.contains(&HygieneFinding::Unscoped {
            package: "whatever".into()
        }));
        assert!(
            messy
                .iter()
                .any(|f| matches!(f, HygieneFinding::NameMismatch { .. }))
        );

        // A git URL is its own category — no registry attestation applies.
        let git = npm_hygiene(
            "thing",
            "npx",
            &["git+https://example.test/thing.git".into()],
        );
        assert!(
            git.iter()
                .any(|f| matches!(f, HygieneFinding::GitDependency { .. }))
        );

        // A local binary has no npm spec, so nothing is invented.
        assert!(npm_hygiene("local", "/usr/bin/local-server", &["--flag".into()]).is_empty());
        assert_eq!(
            package_spec("C:\\tools\\npx.cmd", &["-y".into(), "pkg@1".into()]).as_deref(),
            Some("pkg@1")
        );
    }

    #[test]
    fn scope_is_not_mistaken_for_a_version_separator() {
        assert_eq!(
            split_spec("@scope/pkg@1.2.3"),
            ("@scope/pkg".to_string(), Some("1.2.3".to_string()))
        );
        assert_eq!(split_spec("@scope/pkg"), ("@scope/pkg".to_string(), None));
        assert_eq!(
            split_spec("plain@2"),
            ("plain".to_string(), Some("2".to_string()))
        );
    }
}

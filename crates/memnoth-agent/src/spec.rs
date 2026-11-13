//! `AgentSpec` v1: the definition is the contract.
//!
//! A spec is markdown with YAML front matter. It is parsed by hand, on
//! purpose: the front matter is twenty known keys and a typo in `maxTurns`
//! that silently leaves the agent unbounded is worse than a refusal to start.
//! Every refusal names the file and the line ([`crate::error::AgentError`]).
//!
//! Precedence is `built-in → user → project → plugin`: a later layer replaces
//! an earlier one by name, and two specs in the *same* layer are a duplicate
//! error rather than a coin flip.
//!
//! Two rules carry the safety story and are implemented, not documented:
//!
//! * `disallowed_tools` is applied after `tools`, so a pattern in both is
//!   removed. Addition then removal is the only order that cannot grant.
//! * `Agent(...)` relations must be acyclic across the whole set, checked by a
//!   DFS at load. A cycle is a spawn loop with a human waiting at the end.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AgentError, AgentResult, ErrorCode};
use crate::hooks::HookSpec;

/// Default `max_turns`; a runaway loop stop, not a budget.
pub const DEFAULT_MAX_TURNS: u32 = 40;
/// Default `max_runtime_s`: fifteen minutes.
pub const DEFAULT_MAX_RUNTIME_S: u32 = 900;
/// Default `max_tool_calls`; a cheap proxy for cost.
pub const DEFAULT_MAX_TOOL_CALLS: u32 = 200;
/// Longest `max_runtime_s` a spec may ask for: 24 hours.
pub const MAX_RUNTIME_S_CAP: u32 = 86_400;
/// Lowest non-zero token ceiling a spec may ask for.
pub const MIN_BUDGET_TOKENS: u64 = 1_000;

// ---------------------------------------------------------------------------
// Enumerated fields
// ---------------------------------------------------------------------------

/// How hard the model is asked to reason, mapped per provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Cheapest, for mechanical work.
    Low,
    /// The default.
    #[default]
    Medium,
    /// Deliberation over lookups.
    High,
    /// The most the provider will spend.
    Max,
}

impl Effort {
    /// Parse the lower-case spelling.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "max" => Some(Effort::Max),
            _ => None,
        }
    }
}

/// Blast-radius control, per D9.
///
/// Ordered by how much the run can touch: `readonly` is the most contained,
/// `worktree` gets its own checkout and therefore its own process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    /// No writes at all.
    Readonly,
    /// Shares the parent's checkout and runs in-process.
    #[default]
    Shared,
    /// Own git worktree, own child process.
    Worktree,
}

impl Isolation {
    /// Parse the lower-case spelling.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "readonly" => Some(Isolation::Readonly),
            "shared" => Some(Isolation::Shared),
            "worktree" => Some(Isolation::Worktree),
            _ => None,
        }
    }

    /// Whether this run needs a child process (D9: hybrid by isolation mode).
    pub fn requires_child_process(self) -> bool {
        matches!(self, Isolation::Worktree)
    }

    /// Whether a run at this isolation may write at all.
    pub fn may_write(self) -> bool {
        !matches!(self, Isolation::Readonly)
    }
}

/// Vault access for a child.
///
/// `Propose` implies `Read`: a child that may suggest a belief has to see what
/// the vault already believes, or every proposal is a duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAccess {
    /// No vault slice at all.
    None,
    /// A read-only slice.
    #[default]
    Read,
    /// A slice plus the propose path (never a direct write, per D10).
    Propose,
}

impl MemoryAccess {
    /// Parse the lower-case spelling.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "none" => Some(MemoryAccess::None),
            "read" => Some(MemoryAccess::Read),
            "propose" => Some(MemoryAccess::Propose),
            _ => None,
        }
    }

    /// Whether the slice is visible to the child.
    pub fn reads(self) -> bool {
        !matches!(self, MemoryAccess::None)
    }

    /// Whether the child may propose beliefs.
    pub fn proposes(self) -> bool {
        matches!(self, MemoryAccess::Propose)
    }
}

/// Consent profile. Rank is strictness: `DontAsk` is strictest, `Bypass`
/// most permissive, and a child may never be more permissive than its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalProfile {
    /// Ask for everything, including reads.
    DontAsk,
    /// Plan mode: reads and no edits. Absorbing downward.
    Plan,
    /// Edits allowed without a card; destructive calls still ask.
    AcceptEdits,
    /// The normal profile.
    #[default]
    Default,
    /// Nothing asks. Never inherited; re-granted at the top.
    Bypass,
}

impl ApprovalProfile {
    /// Parse the wire spelling, accepting both the design's short names and
    /// the camelCase the ecosystem uses.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "dontask" | "dont_ask" | "ask" => Some(ApprovalProfile::DontAsk),
            "plan" => Some(ApprovalProfile::Plan),
            "acceptedits" | "accept_edits" => Some(ApprovalProfile::AcceptEdits),
            "default" | "inherit" => Some(ApprovalProfile::Default),
            "bypass" => Some(ApprovalProfile::Bypass),
            _ => None,
        }
    }

    /// Whether this profile may ever be inherited downward.
    pub fn inheritable(self) -> bool {
        !matches!(self, ApprovalProfile::Bypass)
    }
}

/// A hard ceiling on a child run.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Budget {
    /// Token ceiling; `None` means the spec set none.
    pub tokens: Option<u64>,
    /// USD ceiling, resolved through the price table by the ledger.
    pub usd: Option<f64>,
}

impl Budget {
    /// Whether the spec set any ceiling at all.
    pub fn is_unset(&self) -> bool {
        self.tokens.is_none() && self.usd.is_none()
    }

    /// The tighter of two budgets, per component.
    pub fn narrower(self, other: Budget) -> Budget {
        Budget {
            tokens: match (self.tokens, other.tokens) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            usd: match (self.usd, other.usd) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
        }
    }
}

/// Which model the child runs: `inherit` resolves at spawn time in the order
/// call-site override → spec → `MEMNOTH_SUBAGENT_MODEL` → parent model.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelChoice(Option<String>);

impl ModelChoice {
    /// The default: whatever the parent is running.
    pub fn inherit() -> Self {
        ModelChoice(None)
    }

    /// An alias or exact provider id.
    pub fn named(id: impl Into<String>) -> Self {
        ModelChoice(Some(id.into()))
    }

    /// Whether this is `inherit`.
    pub fn is_inherit(&self) -> bool {
        self.0.is_none()
    }

    /// The alias or id, if one was named.
    pub fn as_str(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Tool patterns
// ---------------------------------------------------------------------------

/// The canonical arguments of a call, used for scoped pattern checks.
///
/// Scoped specs are checked **after** a tool is chosen and **before** it runs,
/// against these canonical arguments, so `<exec>(git push *)` denies the call
/// with a typed refusal rather than hiding the tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallArgs {
    /// The argv as it will be executed (`run_command` and friends).
    pub argv: Vec<String>,
    /// The path the call reads or writes, when it names one.
    pub path: Option<String>,
}

impl CallArgs {
    /// Arguments from a slice of `&str`.
    pub fn argv(args: &[&str]) -> Self {
        Self {
            argv: args.iter().map(|a| (*a).to_string()).collect(),
            path: None,
        }
    }

    /// A call that names a path.
    pub fn path(path: impl Into<String>) -> Self {
        Self {
            argv: Vec::new(),
            path: Some(path.into()),
        }
    }

    /// Command and arguments joined the way a prefix pattern reads them.
    pub fn joined(&self) -> String {
        self.argv.join(" ")
    }

    /// The text a scoped matcher should see: the path when named, else argv.
    pub fn scoped_text(&self) -> String {
        match &self.path {
            Some(path) => path.clone(),
            None => self.joined(),
        }
    }
}

/// What a scoped pattern constrains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSpec {
    /// A path glob; `*` stays inside a segment, `**` crosses segments.
    Glob(String),
    /// A command prefix, stored without its trailing `*`.
    CommandPrefix(String),
    /// The children a run may spawn.
    Agents(Vec<String>),
}

/// One parsed tool pattern: `bare`, `scoped`, or `mcp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolPattern {
    /// A tool name, unconstrained.
    Bare {
        /// The tool name.
        name: String,
    },
    /// A tool name with a scoped spec.
    Scoped {
        /// The tool name.
        name: String,
        /// What the spec constrains.
        spec: ScopeSpec,
    },
    /// An MCP tool: server, and optionally a tool glob.
    Mcp {
        /// MCP server name.
        server: String,
        /// Tool glob within the server; `None` is every tool.
        tool: Option<String>,
    },
}

impl ToolPattern {
    /// Parse one pattern.
    ///
    /// `mcp__*` is refused: an unanchored server wildcard grants every tool of
    /// every server, including ones added later, which is not least privilege.
    pub fn parse(raw: &str) -> AgentResult<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(AgentError::new(ErrorCode::Tools, "empty tool pattern"));
        }
        if let Some(rest) = raw.strip_prefix("mcp__") {
            if rest.is_empty() || rest.starts_with('*') {
                return Err(AgentError::new(
                    ErrorCode::Tools,
                    format!("`{raw}` is an unanchored MCP pattern; name the server"),
                ));
            }
            let (server, tool) = match rest.split_once("__") {
                Some((server, tool)) => (server, Some(tool.to_string())),
                None => (rest, None),
            };
            if !is_ident(server) {
                return Err(AgentError::new(
                    ErrorCode::Tools,
                    format!("`{server}` is not a valid MCP server name"),
                ));
            }
            if let Some(tool) = &tool
                && tool.trim().is_empty()
            {
                return Err(AgentError::new(
                    ErrorCode::Tools,
                    format!("`{raw}` names a server and then nothing"),
                ));
            }
            return Ok(ToolPattern::Mcp {
                server: server.to_string(),
                tool,
            });
        }

        if let Some(open) = raw.find('(') {
            if !raw.ends_with(')') {
                return Err(AgentError::new(
                    ErrorCode::Tools,
                    format!("`{raw}` opens a scope and never closes it"),
                ));
            }
            let name = raw[..open].trim();
            let inner = raw[open + 1..raw.len() - 1].trim();
            if !is_ident(name) {
                return Err(AgentError::new(
                    ErrorCode::Tools,
                    format!("`{name}` is not a valid tool name"),
                ));
            }
            if inner.is_empty() {
                return Err(AgentError::new(
                    ErrorCode::Tools,
                    format!("`{raw}` has an empty scope"),
                ));
            }
            let spec = if name == "Agent" {
                let mut children: Vec<String> = inner
                    .split(',')
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect();
                if children.is_empty() {
                    return Err(AgentError::new(
                        ErrorCode::Tools,
                        format!("`{raw}` names no children"),
                    ));
                }
                children.sort();
                children.dedup();
                for child in children.iter() {
                    if !is_agent_name(child) {
                        return Err(AgentError::new(
                            ErrorCode::Tools,
                            format!("`{child}` is not a valid agent name"),
                        ));
                    }
                }
                ScopeSpec::Agents(children)
            } else if inner.contains('/') || inner.ends_with("**") || inner == "*" {
                // A path pattern: `src/**`, `**/*.rs`, `*.rs`. Not a command
                // prefix, even though it also ends with `*` (the `**` case).
                ScopeSpec::Glob(inner.to_string())
            } else if let Some(prefix) = inner.strip_suffix('*') {
                ScopeSpec::CommandPrefix(prefix.trim_end().to_string())
            } else {
                ScopeSpec::Glob(inner.to_string())
            };
            return Ok(ToolPattern::Scoped {
                name: name.to_string(),
                spec,
            });
        }

        if !is_ident(raw) {
            return Err(AgentError::new(
                ErrorCode::Tools,
                format!("`{raw}` is not a valid tool name"),
            ));
        }
        Ok(ToolPattern::Bare {
            name: raw.to_string(),
        })
    }

    /// Parse a comma-free list of patterns; a JSON-ish `[a, b]` string is
    /// already split by the front-matter reader.
    pub fn parse_list(raw: &[String]) -> AgentResult<Vec<Self>> {
        raw.iter().map(|r| ToolPattern::parse(r)).collect()
    }

    /// The tool name a pattern is anchored to.
    pub fn tool_name(&self) -> &str {
        match self {
            ToolPattern::Bare { name } | ToolPattern::Scoped { name, .. } => name,
            ToolPattern::Mcp { server, .. } => server,
        }
    }

    /// Whether this pattern is about `tool` by name alone (the rule
    /// `disallowed_tools` uses).
    pub fn names(&self, tool: &str) -> bool {
        match self {
            ToolPattern::Bare { name } | ToolPattern::Scoped { name, .. } => name == tool,
            ToolPattern::Mcp { .. } => false,
        }
    }

    /// Whether this pattern would let `tool` with `args` through.
    pub fn allows(&self, tool: &str, args: &CallArgs) -> bool {
        match self {
            ToolPattern::Bare { name } => name == tool,
            ToolPattern::Scoped { name, spec } => match spec {
                ScopeSpec::Agents(_) => name == "Agent" && tool == "Agent",
                ScopeSpec::CommandPrefix(prefix) => {
                    name == tool && args.joined().trim_start().starts_with(prefix)
                }
                ScopeSpec::Glob(pattern) => {
                    name == tool && glob_match(pattern, &args.scoped_text())
                }
            },
            ToolPattern::Mcp { server, tool: glob } => {
                if !tool.starts_with("mcp__") {
                    return false;
                }
                let rest = &tool["mcp__".len()..];
                let (this_server, this_tool) = match rest.split_once("__") {
                    Some((s, t)) => (s, Some(t)),
                    None => (rest, None),
                };
                if this_server != server {
                    return false;
                }
                match glob {
                    None => true,
                    Some(glob) => this_tool.map(|t| glob_match(glob, t)).unwrap_or(false),
                }
            }
        }
    }

    /// Whether every call `child` admits is admitted by `self`.
    ///
    /// Deliberately conservative: an unrecognised narrowing is *not* covered,
    /// so containment can fail and a spawn is refused. Refusing a legal spawn
    /// is visible; permitting an illegal one is not.
    pub fn covers(&self, child: &ToolPattern) -> bool {
        match (self, child) {
            (ToolPattern::Bare { name }, ToolPattern::Bare { name: c }) => name == c,
            (ToolPattern::Bare { name }, ToolPattern::Scoped { name: c, spec }) => {
                name == c && !matches!(spec, ScopeSpec::Agents(_))
            }
            (ToolPattern::Bare { name }, ToolPattern::Mcp { server, .. }) => name == server,
            (
                ToolPattern::Scoped { name, spec },
                ToolPattern::Scoped {
                    name: c_name,
                    spec: c_spec,
                },
            ) => {
                name == c_name
                    && match (spec, c_spec) {
                        (ScopeSpec::Glob(a), ScopeSpec::Glob(b)) => {
                            a == b
                                || a == "*"
                                || a == "**"
                                || a.ends_with("/**") && b.starts_with(a.trim_end_matches("**"))
                        }
                        (ScopeSpec::CommandPrefix(a), ScopeSpec::CommandPrefix(b)) => {
                            a == b || b.starts_with(a.as_str())
                        }
                        (ScopeSpec::Agents(a), ScopeSpec::Agents(b)) => {
                            b.iter().all(|child_name| a.contains(child_name))
                        }
                        _ => false,
                    }
            }
            (
                ToolPattern::Mcp { server, tool },
                ToolPattern::Mcp {
                    server: c_server,
                    tool: c_tool,
                },
            ) => {
                server == c_server
                    && match (tool, c_tool) {
                        (None, _) => true,
                        (Some(a), Some(b)) => a == b || a == "*" || glob_match(a, b),
                        (Some(_), None) => false,
                    }
            }
            _ => false,
        }
    }
}

fn is_ident(raw: &str) -> bool {
    let mut chars = raw.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `^[a-z][a-z0-9-]{0,63}$`, per S-DLG-1.
pub fn is_agent_name(raw: &str) -> bool {
    let mut chars = raw.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    raw.len() <= 64 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Segment-aware glob: `*` stays inside a path segment, `**` crosses them.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => {
                if p.get(1) == Some(&'*') {
                    let mut rest = &p[1..];
                    while rest.first() == Some(&'*') {
                        rest = &rest[1..];
                    }
                    (0..=t.len()).any(|i| go(rest, &t[i..]))
                } else {
                    let rest = &p[1..];
                    let mut i = 0usize;
                    loop {
                        if go(rest, &t[i..]) {
                            return true;
                        }
                        if i >= t.len() || t[i] == '/' {
                            return false;
                        }
                        i += 1;
                    }
                }
            }
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

// ---------------------------------------------------------------------------
// Permission algebra (S-DLG-3)
// ---------------------------------------------------------------------------

/// The tool half of a permission: everything, or an explicit allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolSet {
    /// Inherit the parent's tool set.
    All,
    /// Exactly these patterns.
    Only(Vec<ToolPattern>),
}

impl ToolSet {
    /// Whether the set admits `tool` with `args`.
    pub fn allows(&self, tool: &str, args: &CallArgs) -> bool {
        match self {
            ToolSet::All => true,
            ToolSet::Only(patterns) => patterns.iter().any(|p| p.allows(tool, args)),
        }
    }

    /// `self ⊑ parent`.
    pub fn leq(&self, parent: &ToolSet) -> bool {
        match (self, parent) {
            (ToolSet::All, ToolSet::All) => true,
            (ToolSet::Only(_), ToolSet::All) => true,
            (ToolSet::All, ToolSet::Only(_)) => false,
            (ToolSet::Only(a), ToolSet::Only(b)) => a
                .iter()
                .all(|child| b.iter().any(|parent| parent.covers(child))),
        }
    }
}

/// A permission triple `(T, I, P)`: tools, isolation, approval profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permission {
    /// What the run may call.
    pub tools: ToolSet,
    /// Patterns removed after `tools`.
    pub disallowed: Vec<ToolPattern>,
    /// Blast-radius control.
    pub isolation: Isolation,
    /// Consent profile.
    pub approval: ApprovalProfile,
}

/// The answer to "may this run call this?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Allowed.
    Allow,
    /// Refused, with a sentence naming the rule.
    Deny(String),
}

impl Decision {
    /// Whether the call may proceed.
    pub fn allowed(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

impl Permission {
    /// The permission a spec asks for.
    pub fn of(spec: &AgentSpec) -> Self {
        let tools = if spec.tools.is_empty() {
            ToolSet::All
        } else {
            ToolSet::Only(spec.tools.clone())
        };
        Self {
            tools,
            disallowed: spec.disallowed_tools.clone(),
            isolation: spec.isolation,
            approval: spec.approval,
        }
    }

    /// Decide one call. Removal runs first, then the allowlist.
    pub fn permits(&self, tool: &str, args: &CallArgs) -> Decision {
        if let Some(pattern) = self.disallowed.iter().find(|p| p.names(tool)) {
            return Decision::Deny(format!(
                "`{tool}` is disallowed by `{}`",
                describe_pattern(pattern)
            ));
        }
        if self.tools.allows(tool, args) {
            Decision::Allow
        } else {
            Decision::Deny(format!("`{tool}` is not in this run's tool list"))
        }
    }

    /// `self ⊑ parent`: no component is more powerful than the parent's.
    ///
    /// `bypass` is never inherited: a parent that holds it cannot pass it down,
    /// so the approval component compares against `Default` for that case.
    pub fn leq(&self, parent: &Permission) -> bool {
        let approval_ok = self.approval <= parent.approval
            && (parent.approval != ApprovalProfile::Bypass
                || self.approval != ApprovalProfile::Bypass);
        self.tools.leq(&parent.tools) && self.isolation <= parent.isolation && approval_ok
    }

    /// The greatest lower bound: the child is narrowed, never widened.
    ///
    /// Returns the narrowed permission plus one sentence per narrowing, so the
    /// load report says what changed once instead of failing at run time.
    pub fn glb(&self, parent: &Permission) -> (Permission, Vec<String>) {
        let mut notes = Vec::new();

        let tools = match (&self.tools, &parent.tools) {
            (ToolSet::All, ToolSet::All) => ToolSet::All,
            (ToolSet::All, ToolSet::Only(p)) => {
                notes.push("tool list narrowed to the parent's allowlist".to_string());
                ToolSet::Only(p.clone())
            }
            (ToolSet::Only(c), ToolSet::All) => ToolSet::Only(c.clone()),
            (ToolSet::Only(c), ToolSet::Only(p)) => {
                let kept: Vec<ToolPattern> = c
                    .iter()
                    .filter(|child| p.iter().any(|parent| parent.covers(child)))
                    .cloned()
                    .collect();
                if kept.len() != c.len() {
                    notes.push(format!(
                        "{} tool pattern(s) dropped: the parent does not grant them",
                        c.len() - kept.len()
                    ));
                }
                ToolSet::Only(kept)
            }
        };

        let mut disallowed = self.disallowed.clone();
        for pattern in &parent.disallowed {
            if !disallowed.iter().any(|p| p == pattern) {
                disallowed.push(pattern.clone());
            }
        }

        let isolation = self.isolation.min(parent.isolation);
        if isolation != self.isolation {
            notes.push(format!(
                "isolation narrowed to {} by the parent",
                isolation_name(isolation)
            ));
        }

        let approval = if parent.approval == ApprovalProfile::Bypass {
            // `bypass` is re-granted at the top or not at all.
            self.approval
        } else {
            self.approval.min(parent.approval)
        };
        if approval != self.approval {
            notes.push(format!(
                "approval narrowed to {} by the parent",
                approval_name(approval)
            ));
        }

        (
            Permission {
                tools,
                disallowed,
                isolation,
                approval,
            },
            notes,
        )
    }
}

fn isolation_name(isolation: Isolation) -> &'static str {
    match isolation {
        Isolation::Readonly => "readonly",
        Isolation::Shared => "shared",
        Isolation::Worktree => "worktree",
    }
}

fn approval_name(profile: ApprovalProfile) -> &'static str {
    match profile {
        ApprovalProfile::DontAsk => "dontAsk",
        ApprovalProfile::Plan => "plan",
        ApprovalProfile::AcceptEdits => "acceptEdits",
        ApprovalProfile::Default => "default",
        ApprovalProfile::Bypass => "bypass",
    }
}

/// A pattern as it should read back to a person.
pub fn describe_pattern(pattern: &ToolPattern) -> String {
    match pattern {
        ToolPattern::Bare { name } => name.clone(),
        ToolPattern::Scoped { name, spec } => match spec {
            ScopeSpec::Glob(g) => format!("{name}({g})"),
            ScopeSpec::CommandPrefix(p) => format!("{name}({p}*)"),
            ScopeSpec::Agents(list) => format!("Agent({})", list.join(",")),
        },
        ToolPattern::Mcp { server, tool } => match tool {
            Some(tool) => format!("mcp__{server}__{tool}"),
            None => format!("mcp__{server}"),
        },
    }
}

// ---------------------------------------------------------------------------
// The spec
// ---------------------------------------------------------------------------

/// One agent definition.
///
/// `source` is the file it came from, and it is carried so a runtime error can
/// name a file even when the spec was parsed hours earlier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// `^[a-z][a-z0-9-]{0,63}$`, unique after precedence.
    pub name: String,
    /// One line the model routes on; starts with a verb phrase.
    pub description: String,
    /// The markdown body, told to the child once at spawn.
    pub prompt: String,
    /// An allowlist; empty means inherit the parent's set.
    pub tools: Vec<ToolPattern>,
    /// Removed after `tools`; removal wins.
    pub disallowed_tools: Vec<ToolPattern>,
    /// Model alias, exact id, or `inherit`.
    pub model: ModelChoice,
    /// Reasoning budget hint.
    pub effort: Effort,
    /// Runaway loop stop.
    pub max_turns: u32,
    /// Wall-clock stop.
    pub max_runtime_s: u32,
    /// Cheap proxy for cost.
    pub max_tool_calls: u32,
    /// Hard ceiling, enforced by the ledger before a request.
    pub budget: Budget,
    /// Blast-radius control.
    pub isolation: Isolation,
    /// Vault access.
    pub memory: MemoryAccess,
    /// Consent profile.
    pub approval: ApprovalProfile,
    /// Preloaded procedures.
    pub skills: Vec<String>,
    /// Deterministic hooks this child registers.
    pub hooks: Vec<HookSpec>,
    /// MCP servers; requires workspace trust.
    pub mcp_servers: Vec<String>,
    /// Spawn without waiting for the result.
    pub background: bool,
    /// Where the spec was read from, when it came from a file.
    pub source: Option<PathBuf>,
}

impl Default for AgentSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            prompt: String::new(),
            tools: Vec::new(),
            disallowed_tools: Vec::new(),
            model: ModelChoice::inherit(),
            effort: Effort::Medium,
            max_turns: DEFAULT_MAX_TURNS,
            max_runtime_s: DEFAULT_MAX_RUNTIME_S,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            budget: Budget::default(),
            isolation: Isolation::Shared,
            memory: MemoryAccess::Read,
            approval: ApprovalProfile::Default,
            skills: Vec::new(),
            hooks: Vec::new(),
            mcp_servers: Vec::new(),
            background: false,
            source: None,
        }
    }
}

impl AgentSpec {
    /// Parse a spec from markdown text.
    ///
    /// `source` only labels errors; pass `None` for an in-memory fixture and
    /// the location reads `<inline>`.
    pub fn from_markdown(source: Option<&Path>, text: &str) -> AgentResult<Self> {
        let label = source
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("<inline>"));
        let (front, body) = split_front_matter(&label, text)?;
        let mut map = front;

        let name = take_scalar(&mut map, "name")
            .ok_or_else(|| AgentError::at(ErrorCode::Name, &label, 1, "a spec needs a `name`"))?;
        if !is_agent_name(&name.0) {
            return Err(AgentError::at(
                ErrorCode::Name,
                &label,
                name.1,
                format!("`{}` must match ^[a-z][a-z0-9-]{{0,63}}$", name.0),
            ));
        }

        let (description, description_line) =
            take_scalar(&mut map, "description").ok_or_else(|| {
                AgentError::at(
                    ErrorCode::Description,
                    &label,
                    1,
                    "a spec needs a `description`",
                )
            })?;
        let description = description.trim().to_string();
        if description.is_empty() {
            return Err(AgentError::at(
                ErrorCode::Description,
                &label,
                description_line,
                "the description is empty; the model routes on it",
            ));
        }
        if description.chars().count() > 1024 {
            return Err(AgentError::at(
                ErrorCode::Description,
                &label,
                description_line,
                "the description is longer than 1024 characters",
            ));
        }

        let prompt = body.trim().to_string();
        if prompt.is_empty() {
            return Err(AgentError::at(
                ErrorCode::Prompt,
                &label,
                description_line,
                "the body is empty; a child with no prompt has no job",
            ));
        }

        let tools = parse_pattern_list(&mut map, "tools", &label)?.unwrap_or_default();
        let disallowed_tools =
            parse_pattern_list(&mut map, "disallowedtools", &label)?.unwrap_or_default();

        let model = match take_scalar(&mut map, "model") {
            None => ModelChoice::inherit(),
            Some((raw, line)) => {
                let raw = raw.trim();
                if raw == "inherit" || raw.is_empty() {
                    if raw.is_empty() {
                        return Err(AgentError::at(
                            ErrorCode::Model,
                            &label,
                            line,
                            "`model:` is empty; write `inherit` or an id",
                        ));
                    }
                    ModelChoice::inherit()
                } else {
                    ModelChoice::named(raw)
                }
            }
        };

        let effort = match take_scalar(&mut map, "effort") {
            None => Effort::Medium,
            Some((raw, line)) => Effort::parse(raw.trim()).ok_or_else(|| {
                AgentError::at(
                    ErrorCode::Range,
                    &label,
                    line,
                    format!("effort `{raw}` is not one of low|medium|high|max"),
                )
            })?,
        };

        let max_turns = parse_u32(&mut map, "maxturns", DEFAULT_MAX_TURNS, 1, 400, &label)?;
        let max_runtime_s = parse_u32(
            &mut map,
            "maxruntimes",
            DEFAULT_MAX_RUNTIME_S,
            5,
            MAX_RUNTIME_S_CAP,
            &label,
        )?;
        let max_tool_calls = parse_u32(
            &mut map,
            "maxtoolcalls",
            DEFAULT_MAX_TOOL_CALLS,
            1,
            5_000,
            &label,
        )?;

        let budget = parse_budget(&mut map, &label)?;

        let isolation = match take_scalar(&mut map, "isolation") {
            None => Isolation::Shared,
            Some((raw, line)) => Isolation::parse(raw.trim()).ok_or_else(|| {
                AgentError::at(
                    ErrorCode::Range,
                    &label,
                    line,
                    format!("isolation `{raw}` is not one of readonly|shared|worktree"),
                )
            })?,
        };

        let memory = match take_scalar(&mut map, "memory") {
            None => MemoryAccess::Read,
            Some((raw, line)) => MemoryAccess::parse(raw.trim()).ok_or_else(|| {
                AgentError::at(
                    ErrorCode::Range,
                    &label,
                    line,
                    format!("memory `{raw}` is not one of none|read|propose"),
                )
            })?,
        };

        let approval = match take_scalar(&mut map, "approval") {
            None => ApprovalProfile::Default,
            Some((raw, line)) => ApprovalProfile::parse(raw.trim()).ok_or_else(|| {
                AgentError::at(
                    ErrorCode::Range,
                    &label,
                    line,
                    format!("approval `{raw}` is not a profile this build knows"),
                )
            })?,
        };

        let skills = take_list(&mut map, "skills").unwrap_or_default();
        for skill in &skills {
            if skill.trim().is_empty() {
                return Err(AgentError::at(
                    ErrorCode::SpecInvalid,
                    &label,
                    1,
                    "a skills entry is empty",
                ));
            }
        }

        let hook_lines = take_list(&mut map, "hooks").unwrap_or_default();
        let mut hooks = Vec::new();
        for raw in hook_lines {
            let hook = HookSpec::parse(&raw).ok_or_else(|| {
                AgentError::at(
                    ErrorCode::SpecInvalid,
                    &label,
                    1,
                    format!("hook `{raw}` is not `point[:matcher]`"),
                )
            })?;
            if !hook.point.allowed_for_subagent() {
                return Err(AgentError::at(
                    ErrorCode::SpecInvalid,
                    &label,
                    1,
                    format!(
                        "`{}` is a parent hook; a subagent may register pre_tool_use, post_tool_use or stop",
                        hook.point.as_str()
                    ),
                ));
            }
            hooks.push(hook);
        }

        let mcp_servers = take_list(&mut map, "mcpservers").unwrap_or_default();
        let background = match take_scalar(&mut map, "background") {
            None => false,
            Some((raw, line)) => match raw.trim() {
                "true" | "yes" | "on" => true,
                "false" | "no" | "off" => false,
                other => {
                    return Err(AgentError::at(
                        ErrorCode::Range,
                        &label,
                        line,
                        format!("background `{other}` is not a boolean"),
                    ));
                }
            },
        };

        // Anything left is a key this build does not know. A typo that
        // silently disables a limit is worse than a refusal to start (F3).
        if let Some((unknown, entry)) = map.iter().next() {
            return Err(AgentError::at(
                ErrorCode::UnknownField,
                &label,
                entry.line,
                format!("unknown front-matter key `{unknown}`"),
            ));
        }

        Ok(Self {
            name: name.0,
            description,
            prompt,
            tools,
            disallowed_tools,
            model,
            effort,
            max_turns,
            max_runtime_s,
            max_tool_calls,
            budget,
            isolation,
            memory,
            approval,
            skills,
            hooks,
            mcp_servers,
            background,
            source: source.map(|p| p.to_path_buf()),
        })
    }

    /// Read and parse a spec file.
    pub fn from_file(path: &Path) -> AgentResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            AgentError::at(
                ErrorCode::SpecInvalid,
                path,
                1,
                format!("cannot read the spec: {e}"),
            )
        })?;
        Self::from_markdown(Some(path), &text)
    }

    /// `isolation: worktree` requires a git work tree; validated at spawn
    /// because the answer is a property of the checkout, not the spec.
    pub fn check_isolation(&self, is_git_work_tree: bool) -> AgentResult<()> {
        if self.isolation == Isolation::Worktree && !is_git_work_tree {
            return Err(AgentError::new(
                ErrorCode::Worktree,
                format!(
                    "agent `{}` asks for worktree isolation but this workspace is not a git work tree",
                    self.name
                ),
            ));
        }
        Ok(())
    }

    /// Whether this spec may spawn anything at all.
    ///
    /// `disallowedTools: [Agent]` is the documented way to say "never", and it
    /// wins over any `Agent(...)` pattern.
    pub fn can_spawn(&self) -> bool {
        !matches!(self.spawn_scope(), SpawnScope::None)
    }

    /// Which children this spec may spawn.
    pub fn spawn_scope(&self) -> SpawnScope {
        if self.disallowed_tools.iter().any(|p| p.names("Agent")) {
            return SpawnScope::None;
        }
        let mut named = Vec::new();
        for pattern in &self.tools {
            if let ToolPattern::Scoped { name, spec } = pattern
                && name == "Agent"
                && let ScopeSpec::Agents(children) = spec
            {
                named.extend(children.iter().cloned());
            }
        }
        named.sort();
        named.dedup();
        if !named.is_empty() {
            return SpawnScope::Only(named);
        }
        if self.tools.is_empty() {
            return SpawnScope::Inherit;
        }
        if self
            .tools
            .iter()
            .any(|p| matches!(p, ToolPattern::Bare { name } if name == "Agent"))
        {
            return SpawnScope::Inherit;
        }
        SpawnScope::None
    }

    /// Whether this child may be spawned by a parent with `parent_permission`.
    ///
    /// This is INV-P for one spec; the whole-tree property lives in
    /// [`SpecSet::narrow_children`].
    pub fn fits_under(&self, parent: &Permission) -> bool {
        Permission::of(self).leq(parent)
            && (!self.can_spawn() || parent.isolation != Isolation::Readonly)
    }

    /// This spec, narrowed to what `parent` grants, plus the report.
    ///
    /// Budget is narrowed at admission by the ledger, not here: the parent's
    /// ceiling is a session number, not a spec field.
    pub fn narrowed_to(&self, parent: &Permission) -> (AgentSpec, Vec<String>) {
        let (narrowed, notes) = Permission::of(self).glb(parent);
        let mut spec = self.clone();
        spec.tools = match narrowed.tools {
            ToolSet::All => Vec::new(),
            ToolSet::Only(patterns) => patterns,
        };
        spec.disallowed_tools = narrowed.disallowed;
        spec.isolation = narrowed.isolation;
        spec.approval = narrowed.approval;
        (spec, notes)
    }
}

/// Which children a run may spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SpawnScope {
    /// No spawning.
    None,
    /// Inherit the parent's children.
    Inherit,
    /// Exactly these names.
    Only(Vec<String>),
}

impl SpawnScope {
    /// Whether `child` is allowed.
    pub fn allows(&self, child: &str) -> bool {
        match self {
            SpawnScope::None => false,
            SpawnScope::Inherit => true,
            SpawnScope::Only(list) => list.iter().any(|c| c == child),
        }
    }
}

// ---------------------------------------------------------------------------
// Front matter
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Raw {
    Scalar(String),
    List(Vec<String>),
    Map(BTreeMap<String, Entry>),
}

#[derive(Debug, Clone)]
struct Entry {
    line: usize,
    value: Raw,
}

fn split_front_matter(label: &Path, text: &str) -> AgentResult<(BTreeMap<String, Entry>, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let first = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .ok_or_else(|| AgentError::at(ErrorCode::FrontMatter, label, 1, "the file is empty"))?;
    if lines[first].trim() != "---" {
        return Err(AgentError::at(
            ErrorCode::FrontMatter,
            label,
            first + 1,
            "a spec must open with `---` on its own line",
        ));
    }
    let close = lines
        .iter()
        .enumerate()
        .skip(first + 1)
        .find(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i)
        .ok_or_else(|| {
            AgentError::at(
                ErrorCode::FrontMatter,
                label,
                first + 1,
                "the front matter never closes; add a `---` line",
            )
        })?;

    let mut map: BTreeMap<String, Entry> = BTreeMap::new();
    let mut pending: Option<(String, usize)> = None;

    for (idx, raw_line) in lines
        .iter()
        .enumerate()
        .skip(first + 1)
        .take(close - first - 1)
    {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = raw_line.starts_with(' ') || raw_line.starts_with('\t');

        if let Some(item) = trimmed.strip_prefix("- ") {
            let (key, key_line) = pending.clone().ok_or_else(|| {
                AgentError::at(
                    ErrorCode::FrontMatter,
                    label,
                    line_no,
                    "a list item with no key above it",
                )
            })?;
            let value = unquote(item.trim());
            let entry = map.entry(key.clone()).or_insert_with(|| Entry {
                line: key_line,
                value: Raw::List(Vec::new()),
            });
            match &mut entry.value {
                Raw::List(items) => items.push(value),
                _ => {
                    return Err(AgentError::at(
                        ErrorCode::FrontMatter,
                        label,
                        line_no,
                        format!("`{key}` is a list here but a scalar above"),
                    ));
                }
            }
            continue;
        }

        if indented {
            let (key, key_line) = pending.clone().ok_or_else(|| {
                AgentError::at(
                    ErrorCode::FrontMatter,
                    label,
                    line_no,
                    "an indented line with no key above it",
                )
            })?;
            let (inner_key, inner_value) = split_pair(trimmed).ok_or_else(|| {
                AgentError::at(
                    ErrorCode::FrontMatter,
                    label,
                    line_no,
                    "expected `key: value`",
                )
            })?;
            let entry = map.entry(key.clone()).or_insert_with(|| Entry {
                line: key_line,
                value: Raw::Map(BTreeMap::new()),
            });
            match &mut entry.value {
                Raw::Map(children) => {
                    children.insert(
                        normalize_key(inner_key),
                        Entry {
                            line: line_no,
                            value: Raw::Scalar(unquote(inner_value.trim())),
                        },
                    );
                }
                _ => {
                    return Err(AgentError::at(
                        ErrorCode::FrontMatter,
                        label,
                        line_no,
                        format!("`{key}` holds a scalar, not a map"),
                    ));
                }
            }
            continue;
        }

        let (key, value) = split_pair(trimmed).ok_or_else(|| {
            AgentError::at(
                ErrorCode::FrontMatter,
                label,
                line_no,
                "expected `key: value`",
            )
        })?;
        let key = normalize_key(key);
        pending = None;
        if value.trim().is_empty() {
            // A container; the next indented line decides list or map.
            pending = Some((key, line_no));
            continue;
        }
        let entry = if value.trim().starts_with('[') {
            Entry {
                line: line_no,
                value: Raw::List(parse_inline_list(value.trim())),
            }
        } else {
            Entry {
                line: line_no,
                value: Raw::Scalar(unquote(value.trim())),
            }
        };
        map.insert(key, entry);
    }

    let body = if close + 1 < lines.len() {
        lines[close + 1..].join("\n")
    } else {
        String::new()
    };
    // A key written with no children (`tools:`) is an empty list, not an
    // absent key: "inherit-all" and "explicitly nothing" are different.
    if let Some((key, line)) = pending {
        map.entry(key).or_insert_with(|| Entry {
            line,
            value: Raw::List(Vec::new()),
        });
    }
    Ok((map, body))
}

fn split_pair(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

fn normalize_key(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('_', "")
}

fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if (bytes[0] == b'"' && bytes[trimmed.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[trimmed.len() - 1] == b'\'')
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn parse_inline_list(raw: &str) -> Vec<String> {
    let inner = raw.trim().trim_start_matches('[').trim_end_matches(']');
    // Split on top-level commas only: a scoped pattern may contain a comma
    // of its own, e.g. `Agent(explore, critic)` is one item, not two.
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in inner.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                items.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    items.push(current);
    items
        .iter()
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

fn take_scalar(map: &mut BTreeMap<String, Entry>, key: &str) -> Option<(String, usize)> {
    let entry = map.remove(key)?;
    match entry.value {
        Raw::Scalar(value) => Some((value, entry.line)),
        Raw::List(_) => Some((String::new(), entry.line)),
        Raw::Map(_) => Some((String::new(), entry.line)),
    }
}

fn take_list(map: &mut BTreeMap<String, Entry>, key: &str) -> Option<Vec<String>> {
    let entry = map.remove(key)?;
    match entry.value {
        Raw::List(items) => Some(items),
        Raw::Scalar(value) if !value.trim().is_empty() => Some(vec![value]),
        _ => Some(Vec::new()),
    }
}

fn parse_pattern_list(
    map: &mut BTreeMap<String, Entry>,
    key: &str,
    label: &Path,
) -> AgentResult<Option<Vec<ToolPattern>>> {
    let Some((items, line)) = take_list_with_line(map, key) else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for item in items {
        let pattern = ToolPattern::parse(&item)
            .map_err(|e| AgentError::at(ErrorCode::Tools, label, line, e.message))?;
        out.push(pattern);
    }
    Ok(Some(out))
}

fn take_list_with_line(
    map: &mut BTreeMap<String, Entry>,
    key: &str,
) -> Option<(Vec<String>, usize)> {
    let entry = map.remove(key)?;
    let line = entry.line;
    let items = match entry.value {
        Raw::List(items) => items,
        Raw::Scalar(value) if !value.trim().is_empty() => vec![value],
        _ => Vec::new(),
    };
    Some((items, line))
}

fn parse_u32(
    map: &mut BTreeMap<String, Entry>,
    key: &str,
    default: u32,
    min: u32,
    max: u32,
    label: &Path,
) -> AgentResult<u32> {
    let Some((raw, line)) = take_scalar(map, key) else {
        return Ok(default);
    };
    let value: u32 = raw.trim().parse().map_err(|_| {
        AgentError::at(
            ErrorCode::Range,
            label,
            line,
            format!("`{key}` must be a whole number, not `{raw}`"),
        )
    })?;
    if value < min || value > max {
        return Err(AgentError::at(
            ErrorCode::Range,
            label,
            line,
            format!("`{key}` is {value}; the range is {min}..={max}"),
        ));
    }
    Ok(value)
}

fn parse_budget(map: &mut BTreeMap<String, Entry>, label: &Path) -> AgentResult<Budget> {
    let Some(entry) = map.remove("budget") else {
        return Ok(Budget::default());
    };
    let inner = match entry.value {
        Raw::Map(children) => children,
        Raw::Scalar(raw) if raw.trim().is_empty() => BTreeMap::new(),
        Raw::Scalar(raw) => {
            return Err(AgentError::at(
                ErrorCode::Range,
                label,
                entry.line,
                format!("`budget` must be a map, not `{raw}`"),
            ));
        }
        Raw::List(_) => {
            return Err(AgentError::at(
                ErrorCode::Range,
                label,
                entry.line,
                "`budget` must be a map of tokens/usd",
            ));
        }
    };
    let mut budget = Budget::default();
    let mut unknown: Option<(String, usize)> = None;
    for (key, value) in inner {
        match key.as_str() {
            "tokens" => {
                let raw = match &value.value {
                    Raw::Scalar(raw) => raw.clone(),
                    _ => String::new(),
                };
                let tokens: u64 = raw.trim().parse().map_err(|_| {
                    AgentError::at(
                        ErrorCode::Range,
                        label,
                        value.line,
                        format!("`budget.tokens` must be a whole number, not `{raw}`"),
                    )
                })?;
                if tokens < MIN_BUDGET_TOKENS {
                    return Err(AgentError::at(
                        ErrorCode::Range,
                        label,
                        value.line,
                        format!("`budget.tokens` is {tokens}; the minimum is {MIN_BUDGET_TOKENS}"),
                    ));
                }
                budget.tokens = Some(tokens);
            }
            "usd" => {
                let raw = match &value.value {
                    Raw::Scalar(raw) => raw.clone(),
                    _ => String::new(),
                };
                let usd: f64 = raw.trim().parse().map_err(|_| {
                    AgentError::at(
                        ErrorCode::Range,
                        label,
                        value.line,
                        format!("`budget.usd` must be a number, not `{raw}`"),
                    )
                })?;
                if !usd.is_finite() || usd < 0.01 {
                    return Err(AgentError::at(
                        ErrorCode::Range,
                        label,
                        value.line,
                        format!("`budget.usd` is {usd}; the minimum is 0.01"),
                    ));
                }
                budget.usd = Some(usd);
            }
            other => unknown = Some((other.to_string(), value.line)),
        }
    }
    if let Some((key, line)) = unknown {
        return Err(AgentError::at(
            ErrorCode::UnknownField,
            label,
            line,
            format!("unknown budget key `{key}`"),
        ));
    }
    Ok(budget)
}

// ---------------------------------------------------------------------------
// Layers and the definition set
// ---------------------------------------------------------------------------

/// Where a definition came from, in increasing precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Compiled into the binary.
    Builtin,
    /// `~/.memnoth/agents`.
    User,
    /// `<project>/.memnoth/agents`.
    Project,
    /// `<project>/.memnoth/plugins/*/agents`.
    Plugin,
}

impl Layer {
    /// A short label for reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Builtin => "builtin",
            Layer::User => "user",
            Layer::Project => "project",
            Layer::Plugin => "plugin",
        }
    }
}

/// Every definition this build can see, resolved by precedence.
#[derive(Debug, Clone, Default)]
pub struct SpecSet {
    specs: BTreeMap<String, (Layer, AgentSpec)>,
}

impl SpecSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// A set holding the six built-ins (W8-15).
    pub fn with_builtins() -> Self {
        let mut set = Self::new();
        for spec in crate::builtin::builtin_specs() {
            // Built-ins are compiled in and validated by their own constructor,
            // so this cannot fail unless the table is edited badly.
            let _ = set.insert(Layer::Builtin, spec);
        }
        set
    }

    /// Insert a spec at `layer`.
    ///
    /// Same layer twice is a duplicate error; a lower layer never displaces a
    /// higher one, and a higher layer always displaces a lower one.
    pub fn insert(&mut self, layer: Layer, spec: AgentSpec) -> AgentResult<()> {
        let name = spec.name.clone();
        match self.specs.get(&name) {
            Some((existing_layer, _)) if *existing_layer == layer => Err(AgentError::new(
                ErrorCode::Name,
                format!(
                    "agent `{name}` is defined twice in the {} layer",
                    layer.as_str()
                ),
            )),
            Some((existing_layer, _)) if *existing_layer > layer => Ok(()),
            _ => {
                self.specs.insert(name, (layer, spec));
                Ok(())
            }
        }
    }

    /// Load every `*.md` file in `dir` at `layer`.
    ///
    /// A missing directory is not an error: a project with no agents is the
    /// normal case. Files are read in sorted order so two machines with the
    /// same files produce the same set.
    pub fn load_dir(&mut self, dir: &Path, layer: Layer) -> AgentResult<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| {
                AgentError::at(
                    ErrorCode::SpecInvalid,
                    dir,
                    1,
                    format!("cannot read the agents directory: {e}"),
                )
            })?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        files.sort();
        for path in files {
            let spec = AgentSpec::from_file(&path)?;
            self.insert(layer, spec)?;
        }
        Ok(())
    }

    /// Discover built-ins, then user, project, and plugin definitions.
    ///
    /// `home` is passed in rather than looked up so this crate needs no
    /// platform knowledge and a test can point it at a temp directory.
    pub fn discover(home: Option<&Path>, project: Option<&Path>) -> AgentResult<Self> {
        let mut set = Self::with_builtins();
        if let Some(home) = home {
            set.load_dir(&home.join(".memnoth").join("agents"), Layer::User)?;
        }
        if let Some(project) = project {
            set.load_dir(&project.join(".memnoth").join("agents"), Layer::Project)?;
            let plugins = project.join(".memnoth").join("plugins");
            if plugins.is_dir() {
                let mut dirs: Vec<PathBuf> = std::fs::read_dir(&plugins)
                    .map_err(|e| {
                        AgentError::at(
                            ErrorCode::SpecInvalid,
                            &plugins,
                            1,
                            format!("cannot read the plugins directory: {e}"),
                        )
                    })?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect();
                dirs.sort();
                for dir in dirs {
                    set.load_dir(&dir.join("agents"), Layer::Plugin)?;
                }
            }
        }
        set.check_cycles()?;
        Ok(set)
    }

    /// The spec named `name`, if any.
    pub fn get(&self, name: &str) -> Option<&AgentSpec> {
        self.specs.get(name).map(|(_, spec)| spec)
    }

    /// The layer `name` came from.
    pub fn layer_of(&self, name: &str) -> Option<Layer> {
        self.specs.get(name).map(|(layer, _)| *layer)
    }

    /// Every spec, sorted by name for stable output.
    pub fn iter(&self) -> impl Iterator<Item = (Layer, &AgentSpec)> {
        self.specs.values().map(|(layer, spec)| (*layer, spec))
    }

    /// Every name, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.specs.keys().map(|s| s.as_str()).collect()
    }

    /// How many specs are visible after precedence.
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Reject a self-reference or a cycle in the `Agent(...)` relation (F4).
    ///
    /// A cycle here is a spawn loop: every run admits another run of the same
    /// shape until the depth limit stops it, and the user sees a fan-out they
    /// never asked for.
    pub fn check_cycles(&self) -> AgentResult<()> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Unvisited,
            InProgress,
            Done,
        }

        let mut marks: BTreeMap<String, Mark> = self
            .specs
            .keys()
            .map(|name| (name.clone(), Mark::Unvisited))
            .collect();

        fn visit(
            name: &str,
            set: &SpecSet,
            marks: &mut BTreeMap<String, Mark>,
            stack: &mut Vec<String>,
        ) -> AgentResult<()> {
            match marks.get(name).copied().unwrap_or(Mark::Done) {
                Mark::Done => return Ok(()),
                Mark::InProgress => {
                    let start = stack.iter().position(|n| n == name).unwrap_or(0);
                    let mut cycle: Vec<String> =
                        stack[start..].iter().map(|n| (*n).to_string()).collect();
                    cycle.push(name.to_string());
                    return Err(AgentError::new(
                        ErrorCode::Cycle,
                        format!("spawn cycle: {}", cycle.join(" -> ")),
                    ));
                }
                Mark::Unvisited => {}
            }
            marks.insert(name.to_string(), Mark::InProgress);
            stack.push(name.to_string());
            if let Some(spec) = set.get(name)
                && let SpawnScope::Only(children) = spec.spawn_scope()
            {
                for child in children.iter() {
                    if child == name {
                        return Err(AgentError::new(
                            ErrorCode::Cycle,
                            format!("agent `{name}` names itself in Agent(...)"),
                        ));
                    }
                    // A child defined elsewhere is a run-time "unknown agent"
                    // error, not a cycle; only defined edges are walked.
                    if set.get(child).is_some() {
                        visit(child, set, marks, stack)?;
                    }
                }
            }
            stack.pop();
            marks.insert(name.to_string(), Mark::Done);
            Ok(())
        }

        let mut stack: Vec<String> = Vec::new();
        for name in self.names() {
            if marks.get(name).copied() == Some(Mark::Unvisited) {
                visit(name, self, &mut marks, &mut stack)?;
            }
        }
        Ok(())
    }

    /// Narrow every spec to `parent`, returning `(spec, notes)` pairs.
    ///
    /// This is the load-time half of INV-P: an invalid combination is narrowed
    /// once and reported, never widened at run time (S-DLG-3 rule 4).
    pub fn narrow_children(&self, parent: &Permission) -> Vec<(AgentSpec, Vec<String>)> {
        self.specs
            .values()
            .map(|(_, spec)| spec.narrowed_to(parent))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_text(extra_front: &str) -> String {
        format!(
            "---\nname: explore\ndescription: Explore the codebase for a symbol\n{extra_front}---\nFind the symbol and report its callers.\n"
        )
    }

    #[test]
    fn a_minimal_spec_parses_with_documented_defaults() {
        let spec = AgentSpec::from_markdown(None, &spec_text("")).unwrap();
        assert_eq!(spec.name, "explore");
        assert_eq!(spec.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(spec.max_runtime_s, DEFAULT_MAX_RUNTIME_S);
        assert_eq!(spec.max_tool_calls, DEFAULT_MAX_TOOL_CALLS);
        assert_eq!(spec.isolation, Isolation::Shared);
        assert_eq!(spec.memory, MemoryAccess::Read);
        assert_eq!(spec.effort, Effort::Medium);
        assert!(spec.model.is_inherit());
        assert!(spec.tools.is_empty(), "empty tools = inherit-all");
        assert!(spec.prompt.starts_with("Find the symbol"));
    }

    #[test]
    fn camel_case_and_snake_case_keys_are_one_key() {
        let camel = AgentSpec::from_markdown(
            None,
            &spec_text("disallowedTools: [fs_write]\nmaxTurns: 7\nisolation: readonly\n"),
        )
        .unwrap();
        let snake = AgentSpec::from_markdown(
            None,
            &spec_text("disallowed_tools: [fs_write]\nmax_turns: 7\nisolation: readonly\n"),
        )
        .unwrap();
        assert_eq!(camel.max_turns, 7);
        assert_eq!(camel, snake);
    }

    #[test]
    fn a_typo_is_a_refusal_with_file_and_line() {
        let err = AgentSpec::from_markdown(None, &spec_text("maxTurn: 7\n"))
            .expect_err("an unknown key must refuse");
        assert_eq!(err.code, ErrorCode::UnknownField);
        assert!(err.message.contains("maxturn"));
        // `maxTurn:` is the 4th line of spec_text() (`---`, name, description,
        // then the extra front matter); the error names the key's own line.
        assert_eq!(err.line, Some(4));
    }

    #[test]
    fn list_and_inline_list_are_equivalent() {
        let block = AgentSpec::from_markdown(
            None,
            &spec_text("tools:\n  - fs_read\n  - kg_search\nskills:\n  - rust-review\n"),
        )
        .unwrap();
        let inline = AgentSpec::from_markdown(
            None,
            &spec_text("tools: [fs_read, kg_search]\nskills: [rust-review]\n"),
        )
        .unwrap();
        assert_eq!(block.tools, inline.tools);
        assert_eq!(block.skills, inline.skills);
        assert_eq!(block.tools.len(), 2);
    }

    #[test]
    fn a_budget_map_is_read_and_bounded() {
        let spec =
            AgentSpec::from_markdown(None, &spec_text("budget:\n  tokens: 120000\n  usd: 2.50\n"))
                .unwrap();
        assert_eq!(spec.budget.tokens, Some(120_000));
        assert_eq!(spec.budget.usd, Some(2.5));

        let err = AgentSpec::from_markdown(None, &spec_text("budget:\n  tokens: 10\n"))
            .expect_err("below the floor");
        assert_eq!(err.code, ErrorCode::Range);

        let err = AgentSpec::from_markdown(None, &spec_text("budget:\n  yen: 10\n"))
            .expect_err("unknown budget key");
        assert_eq!(err.code, ErrorCode::UnknownField);
    }

    #[test]
    fn every_range_is_enforced() {
        for (front, code) in [
            ("max_turns: 0\n", ErrorCode::Range),
            ("max_runtime_s: 2\n", ErrorCode::Range),
            ("max_tool_calls: 9000\n", ErrorCode::Range),
            ("isolation: container\n", ErrorCode::Range),
            ("memory: write\n", ErrorCode::Range),
            ("approval: whatever\n", ErrorCode::Range),
            ("effort: turbo\n", ErrorCode::Range),
            ("background: maybe\n", ErrorCode::Range),
        ] {
            let err = AgentSpec::from_markdown(None, &spec_text(front))
                .expect_err(&format!("expected {code:?} for {front}"));
            assert_eq!(err.code, code, "{front}");
        }
    }

    #[test]
    fn isolation_and_memory_parse() {
        let spec =
            AgentSpec::from_markdown(None, &spec_text("isolation: worktree\nmemory: propose\n"))
                .unwrap();
        assert_eq!(spec.isolation, Isolation::Worktree);
        assert_eq!(spec.memory, MemoryAccess::Propose);
        assert!(spec.memory.proposes());
        assert!(spec.memory.reads());
        assert!(spec.isolation.requires_child_process());
    }

    #[test]
    fn an_empty_prompt_or_description_refuses() {
        let err = AgentSpec::from_markdown(None, "---\nname: x\ndescription: Do things\n---\n")
            .expect_err("empty body");
        assert_eq!(err.code, ErrorCode::Prompt);

        let err = AgentSpec::from_markdown(None, "---\nname: x\ndescription: \"\"\n---\nbody\n")
            .expect_err("empty description");
        assert_eq!(err.code, ErrorCode::Description);
    }

    #[test]
    fn a_bad_name_refuses() {
        for name in ["Explore", "1explore", "explore_this", "explore!"] {
            let text = format!("---\nname: {name}\ndescription: Do things\n---\nbody\n");
            let err = AgentSpec::from_markdown(None, &text).expect_err("bad name");
            assert_eq!(err.code, ErrorCode::Name);
        }
        let long = "a".repeat(65);
        let text = format!("---\nname: {long}\ndescription: Do things\n---\nbody\n");
        assert!(AgentSpec::from_markdown(None, &text).is_err());
    }

    #[test]
    fn front_matter_delimiters_are_required() {
        let err = AgentSpec::from_markdown(None, "name: x\n").expect_err("no front matter");
        assert_eq!(err.code, ErrorCode::FrontMatter);
        assert_eq!(err.line, Some(1));

        let err = AgentSpec::from_markdown(None, "---\nname: x\n").expect_err("never closes");
        assert_eq!(err.code, ErrorCode::FrontMatter);
    }

    #[test]
    fn disallowed_removal_wins_over_addition() {
        let spec = AgentSpec::from_markdown(
            None,
            &spec_text("tools: [fs_read, fs_write]\ndisallowed_tools: [fs_write]\n"),
        )
        .unwrap();
        let permission = Permission::of(&spec);
        assert!(
            permission
                .permits("fs_read", &CallArgs::default())
                .allowed()
        );
        let denial = permission.permits("fs_write", &CallArgs::default());
        assert!(!denial.allowed());
        match denial {
            Decision::Deny(reason) => assert!(reason.contains("disallowed")),
            Decision::Allow => unreachable!(),
        }
    }

    #[test]
    fn a_scope_is_checked_against_the_arguments() {
        let spec = AgentSpec::from_markdown(
            None,
            &spec_text("tools:\n  - fs_read(src/**)\n  - run_command(cargo test*)\n"),
        )
        .unwrap();
        let permission = Permission::of(&spec);
        assert!(
            permission
                .permits("fs_read", &CallArgs::path("src/a/b.rs"))
                .allowed()
        );
        assert!(
            !permission
                .permits("fs_read", &CallArgs::path("tests/a.rs"))
                .allowed()
        );
        assert!(
            permission
                .permits(
                    "run_command",
                    &CallArgs::argv(&["cargo", "test", "-p", "x"])
                )
                .allowed()
        );
        assert!(
            !permission
                .permits("run_command", &CallArgs::argv(&["cargo", "publish"]))
                .allowed()
        );
    }

    #[test]
    fn glob_is_segment_aware() {
        assert!(glob_match("src/*.rs", "src/a.rs"));
        assert!(!glob_match("src/*.rs", "src/a/b.rs"));
        assert!(glob_match("src/**", "src/a/b.rs"));
        assert!(glob_match("**/*.rs", "src/a/b.rs"));
        assert!(!glob_match("src/*.rs", "src/a.rs.bak"));
    }

    #[test]
    fn an_unanchored_mcp_pattern_is_refused() {
        for pattern in ["mcp__*", "mcp__", "mcp__github__get_*"] {
            let result = ToolPattern::parse(pattern);
            if pattern == "mcp__github__get_*" {
                assert!(result.is_ok(), "a scoped MCP pattern is legal");
            } else {
                let err = result.expect_err("unanchored MCP must be refused");
                assert_eq!(err.code, ErrorCode::Tools);
            }
        }
        let mcp = ToolPattern::parse("mcp__github__get_issue").unwrap();
        assert!(mcp.allows("mcp__github__get_issue", &CallArgs::default()));
        assert!(!mcp.allows("mcp__gitlab__get_issue", &CallArgs::default()));
        assert!(!mcp.allows("fs_read", &CallArgs::default()));
    }

    #[test]
    fn disallowed_agent_disables_spawning() {
        let spec = AgentSpec::from_markdown(
            None,
            &spec_text("tools: [Agent(explore)]\ndisallowed_tools: [Agent]\n"),
        )
        .unwrap();
        assert!(!spec.can_spawn());
        assert_eq!(spec.spawn_scope(), SpawnScope::None);

        let spec = AgentSpec::from_markdown(None, &spec_text("tools: [Agent(explore, critic)]\n"))
            .unwrap();
        assert!(spec.can_spawn());
        assert!(spec.spawn_scope().allows("explore"));
        assert!(!spec.spawn_scope().allows("review"));
    }

    #[test]
    fn a_child_never_exceeds_its_parent() {
        let parent = Permission {
            tools: ToolSet::Only(vec![
                ToolPattern::parse("fs_read").unwrap(),
                ToolPattern::parse("kg_search").unwrap(),
            ]),
            disallowed: vec![],
            isolation: Isolation::Shared,
            approval: ApprovalProfile::Plan,
        };
        let child = Permission {
            tools: ToolSet::Only(vec![
                ToolPattern::parse("fs_read").unwrap(),
                ToolPattern::parse("fs_write").unwrap(),
            ]),
            disallowed: vec![],
            isolation: Isolation::Worktree,
            approval: ApprovalProfile::AcceptEdits,
        };
        assert!(!child.leq(&parent), "the child adds a tool and edits");

        let (narrowed, notes) = child.glb(&parent);
        assert!(narrowed.leq(&parent));
        assert_eq!(narrowed.isolation, Isolation::Shared);
        assert_eq!(narrowed.approval, ApprovalProfile::Plan);
        assert!(!notes.is_empty());
    }

    #[test]
    fn bypass_is_never_inherited() {
        let parent = Permission {
            tools: ToolSet::All,
            disallowed: vec![],
            isolation: Isolation::Worktree,
            approval: ApprovalProfile::Bypass,
        };
        let child = Permission {
            tools: ToolSet::All,
            disallowed: vec![],
            isolation: Isolation::Shared,
            approval: ApprovalProfile::Bypass,
        };
        assert!(!child.leq(&parent), "bypass must not pass downward");
    }

    #[test]
    fn precedence_is_builtin_user_project_plugin() {
        let mut set = SpecSet::with_builtins();
        let mut user = set.get("explore").unwrap().clone();
        user.description = "User override".to_string();
        set.insert(Layer::User, user).unwrap();
        assert_eq!(set.get("explore").unwrap().description, "User override");
        assert_eq!(set.layer_of("explore"), Some(Layer::User));

        let mut project = set.get("explore").unwrap().clone();
        project.description = "Project override".to_string();
        set.insert(Layer::Project, project).unwrap();
        assert_eq!(set.layer_of("explore"), Some(Layer::Project));

        // A later lower layer does not displace it.
        let mut stale_user = set.get("explore").unwrap().clone();
        stale_user.description = "Stale user".to_string();
        set.insert(Layer::User, stale_user).unwrap();
        assert_eq!(set.get("explore").unwrap().description, "Project override");
    }

    #[test]
    fn a_duplicate_inside_one_layer_refuses() {
        let mut set = SpecSet::new();
        let spec = AgentSpec::from_markdown(None, &spec_text("")).unwrap();
        set.insert(Layer::Project, spec.clone()).unwrap();
        let err = set.insert(Layer::Project, spec).expect_err("duplicate");
        assert_eq!(err.code, ErrorCode::Name);
    }

    #[test]
    fn a_cycle_is_refused_at_load() {
        let mut set = SpecSet::new();
        let a = AgentSpec::from_markdown(
            None,
            &format!(
                "---\nname: a\ndescription: Do a\n{front}---\nbody\n",
                front = "tools: [Agent(b)]\n"
            ),
        )
        .unwrap();
        let b = AgentSpec::from_markdown(
            None,
            &format!(
                "---\nname: b\ndescription: Do b\n{front}---\nbody\n",
                front = "tools: [Agent(a)]\n"
            ),
        )
        .unwrap();
        set.insert(Layer::Project, a).unwrap();
        set.insert(Layer::Project, b).unwrap();
        let err = set.check_cycles().expect_err("a<->b is a cycle");
        assert_eq!(err.code, ErrorCode::Cycle);
        assert!(err.message.contains("a -> b -> a"));
    }

    #[test]
    fn a_self_reference_is_refused() {
        let mut set = SpecSet::new();
        let spec = AgentSpec::from_markdown(
            None,
            &format!(
                "---\nname: a\ndescription: Do a\n{front}---\nbody\n",
                front = "tools: [Agent(a)]\n"
            ),
        )
        .unwrap();
        set.insert(Layer::Project, spec).unwrap();
        let err = set.check_cycles().expect_err("self reference");
        assert_eq!(err.code, ErrorCode::Cycle);
    }

    #[test]
    fn a_dag_is_not_a_cycle() {
        let mut set = SpecSet::new();
        for (name, child) in [("a", "b"), ("b", "c"), ("c", "")] {
            let front = if child.is_empty() {
                String::new()
            } else {
                format!("tools: [Agent({child})]\n")
            };
            let text = format!("---\nname: {name}\ndescription: Do {name}\n{front}---\nbody\n");
            set.insert(
                Layer::Project,
                AgentSpec::from_markdown(None, &text).unwrap(),
            )
            .unwrap();
        }
        set.check_cycles().expect("a -> b -> c is a DAG");
    }

    #[test]
    fn narrowing_a_spec_drops_what_the_parent_does_not_grant() {
        let spec = AgentSpec::from_markdown(
            None,
            &spec_text("tools: [fs_read, fs_write]\nisolation: worktree\napproval: acceptEdits\n"),
        )
        .unwrap();
        let parent = Permission {
            tools: ToolSet::Only(vec![ToolPattern::parse("fs_read").unwrap()]),
            disallowed: vec![],
            isolation: Isolation::Readonly,
            approval: ApprovalProfile::Plan,
        };
        let (narrowed, notes) = spec.narrowed_to(&parent);
        assert_eq!(narrowed.isolation, Isolation::Readonly);
        assert_eq!(narrowed.approval, ApprovalProfile::Plan);
        assert_eq!(narrowed.tools.len(), 1);
        assert!(!notes.is_empty());
        assert!(Permission::of(&narrowed).leq(&parent));
    }
}

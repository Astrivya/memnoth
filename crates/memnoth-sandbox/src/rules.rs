//! Rule-language v1 (frozen).
//!
//! Idea credit: Claude Code's "permissions are a language, not a dialog"
//! (`Bash(git *)` rule shapes). No Claude code was copied; this grammar,
//! parser, and decision procedure are original (see THIRD-PARTY-NOTICES.md).
//!
//! # Frozen v1 grammar
//!
//! ```text
//! rules_file := line*
//! line       := blank | comment | rule
//! comment    := "#" rest-of-line
//! rule       := tool "(" pattern ")" "=>" behavior
//! tool       := "Bash" | "Read" | "Write" | "Edit" | "WebFetch" | "MCP" | "*"
//! pattern    := anchored glob over the tool's primary argument
//! behavior   := "allow" | "deny" | "ask"
//! ```
//!
//! Pattern dialect: `*`, `**`, `?`, simple `[...]`. `{`, `}`, and `\` are
//! rejected — the same fail-closed subset as deny-globs. Matching is an
//! ANCHORED full match. `*` crosses `/` for `Bash` (a command line is flat
//! text), `WebFetch` (URLs), `MCP`, and `*`, but not for path tools
//! (`Read`/`Write`/`Edit`, where `/` separates).
//!
//! # Decision procedure (Bash)
//!
//! 1. If ANY `deny` rule matches the FULL command (covers pipe-to-shell and
//!    redirect-to-device shapes that span segments) → `Deny`.
//! 2. Split the command into segments on shell separators (`;`, `&&`, `||`,
//!    `|`, `&`, newlines), respecting quotes. Backtick / `$(...)`
//!    substitution makes a segment opaque.
//! 3. Normalize each segment (trim, collapse runs of whitespace, strip
//!    leading `VAR=value` assignments, reduce the program word to its
//!    basename so `/bin/rm` cannot dodge `rm` rules).
//! 4. If ANY segment matches a `deny` rule → `Deny` (strictest wins).
//! 5. If any segment is opaque or (in a compound command) matches no `allow`
//!    rule → `Ask`.
//! 6. If any segment matches an `ask` rule → `Ask`.
//! 7. Otherwise → `Allow` (fail-open UX; security-critical paths are covered
//!    by backend deny lists plus the frozen builtin catastrophic denies).
//!
//! Non-Bash tools: the pattern matches the primary argument (path/URL)
//! directly, single-segment, same strictest-wins order.
//!
//! # Layering
//!
//! This engine matches command *shapes*, not parsed intent: `timeout 5 rm
//! -rf /` escalates to `Ask` (wrapper hides the verb) rather than `Deny`.
//! The deterministic risk parser (`memnoth-command-risk`, which unwraps
//! wrappers) is the second stage that grades those. The invariant this
//! engine guarantees: evasion shapes never silently `Allow`.
//!
//! # Non-goals for v1 (gate v2 on misuse data)
//!
//! No braces/alternation, no regex, no per-user/per-host conditions, no rule
//! priorities (strictest-wins instead), no nested-shell payload extraction,
//! no workspace-relative downgrades (system-root denies deliberately omit
//! `/home`, `/Users`, `/data`, `/opt`, `/srv`, `/tmp` so workspace-nested
//! absolute deletes ask instead of denying).

use std::fmt;

/// What a rule says to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    Allow,
    Ask,
    Deny,
}

impl Behavior {
    fn rank(self) -> u8 {
        match self {
            Behavior::Allow => 0,
            Behavior::Ask => 1,
            Behavior::Deny => 2,
        }
    }
}

impl std::str::FromStr for Behavior {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "allow" => Ok(Self::Allow),
            "ask" => Ok(Self::Ask),
            "deny" => Ok(Self::Deny),
            other => Err(format!(
                "unknown behavior {other:?}; expected allow|ask|deny"
            )),
        }
    }
}

impl fmt::Display for Behavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allow => write!(f, "allow"),
            Self::Ask => write!(f, "ask"),
            Self::Deny => write!(f, "deny"),
        }
    }
}

/// One compiled rule: `Tool(pattern) => behavior`.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Tool name (`Bash`, `Read`, …) or `*` for any tool.
    pub tool: String,
    /// Anchored glob over the primary argument.
    pub pattern: String,
    pub behavior: Behavior,
    /// `builtin:<n>` or `user:<line>` — shown in explanations and docs.
    pub id: String,
}

impl Rule {
    /// Whether this rule applies to a call of `tool` with primary argument `arg`.
    pub fn matches(&self, tool: &str, arg: &str) -> bool {
        if self.tool != "*" && !self.tool.eq_ignore_ascii_case(tool) {
            return false;
        }
        glob_full_match(&self.pattern, arg, separator_is_significant(tool))
    }
}

/// Whether `/` is a significant separator for a tool's patterns: yes for
/// path tools (`Read`/`Write`/`Edit`), no for command lines (`Bash`), URLs
/// (`WebFetch`), and multiplexed tool namespaces (`MCP`, `*`).
fn separator_is_significant(tool: &str) -> bool {
    tool.eq_ignore_ascii_case("Read")
        || tool.eq_ignore_ascii_case("Write")
        || tool.eq_ignore_ascii_case("Edit")
}

/// Parse a rules file. Fail-closed: ANY malformed rule line is an `Err`
/// (refuse to start with unknown policy rather than run a partial one).
/// Unknown tool names are also an `Err` — the tool set is frozen in v1.
pub fn parse_rules(text: &str) -> anyhow::Result<Vec<Rule>> {
    const KNOWN_TOOLS: &[&str] = &["Bash", "Read", "Write", "Edit", "WebFetch", "MCP", "*"];
    let mut rules = Vec::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let rule = parse_rule_line(line, line_no, KNOWN_TOOLS)?;
        validate_rule_pattern(&rule)?;
        rules.push(rule);
    }
    Ok(rules)
}

fn parse_rule_line(line: &str, line_no: usize, known: &[&str]) -> anyhow::Result<Rule> {
    let (head, behavior_str) = line.split_once("=>").ok_or_else(|| {
        anyhow::anyhow!("rules line {line_no}: missing '=> behavior' in {line:?}")
    })?;
    let behavior: Behavior = behavior_str
        .trim()
        .parse()
        .map_err(|e: String| anyhow::anyhow!("rules line {line_no}: {e}"))?;
    let head = head.trim();
    let open = head
        .find('(')
        .ok_or_else(|| anyhow::anyhow!("rules line {line_no}: missing '(' in {line:?}"))?;
    if !head.ends_with(')') {
        anyhow::bail!("rules line {line_no}: missing closing ')' in {line:?}");
    }
    let tool = head[..open].trim().to_string();
    if !known.iter().any(|k| k.eq_ignore_ascii_case(&tool)) {
        anyhow::bail!(
            "rules line {line_no}: unknown tool {tool:?}; v1 tools are Bash|Read|Write|Edit|WebFetch|MCP|*"
        );
    }
    // Canonicalize the tool name (keep `*` as-is).
    let tool = if tool == "*" {
        tool
    } else {
        known
            .iter()
            .find(|k| k.eq_ignore_ascii_case(&tool))
            .copied()
            .unwrap_or("Bash")
            .to_string()
    };
    let pattern = head[open + 1..head.len() - 1].trim().to_string();
    if pattern.is_empty() {
        anyhow::bail!("rules line {line_no}: empty pattern in {line:?}");
    }
    Ok(Rule {
        tool,
        pattern,
        behavior,
        id: format!("user:{line_no}"),
    })
}

fn validate_rule_pattern(rule: &Rule) -> anyhow::Result<()> {
    // Same fail-closed subset as deny-globs: no braces/backslash outside
    // `[...]` classes, `**` only as a whole segment.
    if let Some(c) = crate::deny::find_bare_alternation(&rule.pattern) {
        anyhow::bail!(
            "rule {}: pattern {:?} uses unsupported metacharacter '{c}' \
             (brace alternation and backslash-escapes are not supported outside \
             `[...]` classes; use separate rules)",
            rule.id,
            rule.pattern,
        );
    }
    for segment in rule.pattern.split('/') {
        if segment.contains("**") && segment != "**" {
            anyhow::bail!(
                "rule {}: pattern {:?}: `**` must be its own segment (got {segment:?})",
                rule.id,
                rule.pattern,
            );
        }
    }
    globset::GlobBuilder::new(&rule.pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| {
            anyhow::anyhow!("rule {}: invalid pattern {:?}: {e}", rule.id, rule.pattern)
        })?;
    Ok(())
}

/// Anchored full match. `literal_separator` follows the tool kind (see
/// [`separator_is_significant`]). A pattern that fails to compile fails
/// closed (matches) — it passed validation at parse, so this is defense in
/// depth.
fn glob_full_match(pattern: &str, candidate: &str, literal_separator: bool) -> bool {
    match globset::GlobBuilder::new(pattern)
        .literal_separator(literal_separator)
        .build()
    {
        Ok(g) => g.compile_matcher().is_match(candidate),
        Err(_) => true,
    }
}

/// A compiled rule set: frozen builtins plus user rules.
///
/// `managed_by` is `Some(governor)` in enterprise managed mode (§9): user
/// rules are refused (see [`RuleSet::managed_with_user_rules`]) and only
/// builtins decide. The TUI banner shows the governor ("who governs").
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
    pub managed_by: Option<String>,
}

impl RuleSet {
    /// Builtins plus `user_rules` (parsed with [`parse_rules`]).
    pub fn with_user_rules(user_rules: Vec<Rule>) -> Self {
        let mut rules = builtin_rules();
        rules.extend(user_rules);
        Self {
            rules,
            managed_by: None,
        }
    }

    pub fn builtins_only() -> Self {
        Self {
            rules: builtin_rules(),
            managed_by: None,
        }
    }

    /// Managed mode: builtins only, locked by `governor`.
    pub fn managed(governor: &str) -> Self {
        Self {
            rules: builtin_rules(),
            managed_by: Some(governor.to_string()),
        }
    }

    /// Managed mode with attempted user rules: fail-closed — any user rule
    /// is an `Err` naming the governor (rules are locked, never silently
    /// dropped).
    pub fn managed_with_user_rules(user_rules: Vec<Rule>, governor: &str) -> anyhow::Result<Self> {
        if !user_rules.is_empty() {
            anyhow::bail!(
                "rules are locked by {governor}: refusing {} user rule(s) \
                 (first: {}); managed mode decides with builtins only",
                user_rules.len(),
                user_rules[0].id,
            );
        }
        Ok(Self::managed(governor))
    }

    /// Whether this set is enterprise-locked.
    pub fn is_managed(&self) -> bool {
        self.managed_by.is_some()
    }

    /// Governor name for the §9 banner, if managed.
    pub fn governor(&self) -> Option<&str> {
        self.managed_by.as_deref()
    }

    /// Docs-visible allow list (rule id + shape).
    pub fn allow_list(&self) -> Vec<(String, String, String)> {
        self.rules
            .iter()
            .filter(|r| r.behavior == Behavior::Allow)
            .map(|r| (r.id.clone(), r.tool.clone(), r.pattern.clone()))
            .collect()
    }

    /// Docs-visible deny list (rule id + shape).
    pub fn deny_list(&self) -> Vec<(String, String, String)> {
        self.rules
            .iter()
            .filter(|r| r.behavior == Behavior::Deny)
            .map(|r| (r.id.clone(), r.tool.clone(), r.pattern.clone()))
            .collect()
    }
}

/// The decision for one tool call.
///
/// `rule` always names the deciding rule (`builtin:<n>`, `user:<line>`,
/// `builtin:opaque-substitution`, `builtin:compound-gap`, or
/// `default:allow`) so every approval/denial can log
/// actor/action/target/timestamp (§9 audit trail starts here).
/// `destructive` + `kind` on `Ask` are the §9 policy signals: the inline
/// card reads them for two-step confirmation routing, and the stage-2 risk
/// parser reads `kind` to decide what to unwrap vs present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleDecision {
    Allow {
        rule: String,
    },
    Ask {
        reason: String,
        rule: String,
        destructive: bool,
        kind: AskKind,
    },
    Deny {
        reason: String,
        rule: String,
    },
}

/// Why an `Ask` asks: routing signal for the §9 inline card and the
/// stage-2 risk parser (`memnoth-command-risk` unwraps wrappers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskKind {
    /// A wrapper hid the verb (`timeout 5 rm …`, `env …`, `sh -c …`):
    /// stage 2 should unwrap and grade the inner command.
    WrapperHidden,
    /// `$(…)` / backticks hide the payload: nothing safe to unwrap.
    OpaqueSubstitution,
    /// Compound command with a segment no allow rule covers.
    CompoundGap,
    /// A policy ask rule matched (network, credentials, bounded-destructive).
    PolicyAsk,
}

impl RuleDecision {
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    /// The deciding rule id (audit target).
    pub fn rule_id(&self) -> &str {
        match self {
            Self::Allow { rule } | Self::Ask { rule, .. } | Self::Deny { rule, .. } => rule,
        }
    }

    /// §9 two-step: denied outright, or an ask over a destructive shape
    /// (rm -rf class) → card, then type-to-confirm. All other asks stay a
    /// single inline card.
    pub fn requires_two_step(&self) -> bool {
        match self {
            Self::Deny { .. } => true,
            Self::Ask { destructive, .. } => *destructive,
            Self::Allow { .. } => false,
        }
    }

    /// Guardian-style auto-review (§9): only decisions an explicit allow
    /// rule made — `default:allow` unknowns are fail-open UX, never
    /// auto-approved (stage 2 still grades them).
    pub fn auto_reviewable(&self) -> bool {
        match self {
            Self::Allow { rule } => rule != "default:allow",
            Self::Ask { .. } | Self::Deny { .. } => false,
        }
    }

    /// Routing kind for asks (`None` for allow/deny).
    pub fn ask_kind(&self) -> Option<AskKind> {
        match self {
            Self::Ask { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

/// Ask-rule patterns whose match means "the verb is hidden, not the policy":
/// stage 2 unwraps these. Privilege wrappers (`sudo`/`doas`/`su`) are NOT
/// here — they are policy asks, not verb-hiding.
const WRAPPER_PATTERNS: &[&str] = &[
    "env *",
    "nice *",
    "timeout *",
    "nohup *",
    "stdbuf *",
    "setsid *",
    "command *",
    "builtin *",
    "exec *",
    "*sh -c *",
    "xargs *",
];

/// Whether a matched ask-rule pattern is a destructive shape (rm -rf class,
/// recursive chmod/chown, raw `dd`, shred, filesystem makers): §9 two-step.
/// Conservative by construction — unknown payloads (opaque/wrapper/gap) are
/// single-card asks; only *visible* destructive shapes escalate to two-step.
fn is_destructive_ask_pattern(tool: &str, pattern: &str) -> bool {
    if !tool.eq_ignore_ascii_case("Bash") {
        return false;
    }
    const MARKERS: &[&str] = &[
        "rm ",
        "rm *",
        "find ",
        "git clean",
        "chmod",
        "chown",
        "dd ",
        "shred",
        "mkfs",
        "wipefs",
    ];
    MARKERS.iter().any(|m| pattern.contains(m))
}

/// Decide one tool call with primary argument `arg`.
pub fn decide(rules: &RuleSet, tool: &str, arg: &str) -> RuleDecision {
    if tool.eq_ignore_ascii_case("Bash") {
        decide_bash(rules, arg)
    } else {
        decide_single(rules, tool, arg)
    }
}

fn decide_single(rules: &RuleSet, tool: &str, arg: &str) -> RuleDecision {
    let mut best: Option<&Rule> = None;
    for rule in &rules.rules {
        if rule.matches(tool, arg)
            && best.is_none_or(|b: &Rule| rule.behavior.rank() > b.behavior.rank())
        {
            best = Some(rule);
        }
    }
    match best {
        None => RuleDecision::Allow {
            rule: "default:allow".to_string(),
        },
        Some(rule) => match rule.behavior {
            Behavior::Allow => RuleDecision::Allow {
                rule: rule.id.clone(),
            },
            Behavior::Ask => RuleDecision::Ask {
                reason: format!(
                    "rule {}: {}({}) requires confirmation",
                    rule.id, rule.tool, rule.pattern
                ),
                rule: rule.id.clone(),
                destructive: is_destructive_ask_pattern(&rule.tool, &rule.pattern),
                kind: AskKind::PolicyAsk,
            },
            Behavior::Deny => RuleDecision::Deny {
                reason: format!(
                    "rule {}: {}({}) is denied: {}",
                    rule.id,
                    rule.tool,
                    rule.pattern,
                    explain_pattern(&rule.pattern),
                ),
                rule: rule.id.clone(),
            },
        },
    }
}

fn decide_bash(rules: &RuleSet, command: &str) -> RuleDecision {
    let full = command.trim();
    if full.is_empty() {
        return RuleDecision::Allow {
            rule: "default:allow".to_string(),
        };
    }
    // Whole-command deny pre-scan: catches pipe-to-shell and redirect shapes
    // that span segments (the splitter below consumes `|`/`>` adjacency).
    // User deny rules get the same power; allow/ask stay segment-level.
    for rule in &rules.rules {
        if rule.behavior == Behavior::Deny
            && (rule.tool == "*" || rule.tool.eq_ignore_ascii_case("Bash"))
            && glob_full_match(&rule.pattern, full, false)
        {
            return RuleDecision::Deny {
                reason: format!(
                    "rule {}: Bash({}) is denied: {}",
                    rule.id,
                    rule.pattern,
                    explain_pattern(&rule.pattern)
                ),
                rule: rule.id.clone(),
            };
        }
    }

    let segments = split_segments(command);
    // (reason, deciding rule id, destructive, kind). Later segments
    // overwrite earlier ones (same as v1 always did for the reason).
    let mut asked: Option<(String, String, bool, AskKind)> = None;
    let mut any_allow_only_gap = false;
    // Id of an allow rule that matched (for the audit trail and Guardian
    // auto-review). Only read when the final decision is Allow — i.e. every
    // segment matched allow — so any writer is representative.
    let mut allowed: Option<String> = None;

    for segment in &segments {
        let normalized = normalize_segment(segment);
        if normalized.is_empty() {
            continue;
        }
        // Absolute root wipe (`rm -rf /`, `rm -rf /*`, `rm -r -f //`, …):
        // a small absolute path-based deny that does not depend on glob
        // shapes. Stable id `builtin:root-wipe`.
        if is_root_wipe(&normalized) {
            return RuleDecision::Deny {
                reason: "rule builtin:root-wipe: recursive delete of the filesystem root \
                         is denied: destructive deletion outside any recoverable scope"
                    .to_string(),
                rule: "builtin:root-wipe".to_string(),
            };
        }
        // Opaque segments (command substitution) escalate to Ask.
        if is_opaque(&normalized) {
            asked = Some((
                format!(
                    "command substitution in {normalized:?} hides its payload; confirmation required"
                ),
                "builtin:opaque-substitution".to_string(),
                false,
                AskKind::OpaqueSubstitution,
            ));
            continue;
        }
        let mut best: Option<&Rule> = None;
        for rule in &rules.rules {
            if (rule.tool == "*" || rule.tool.eq_ignore_ascii_case("Bash"))
                && glob_full_match(&rule.pattern, &normalized, false)
                && best.is_none_or(|b: &Rule| rule.behavior.rank() > b.behavior.rank())
            {
                best = Some(rule);
            }
        }
        match best {
            Some(rule) if rule.behavior == Behavior::Deny => {
                return RuleDecision::Deny {
                    reason: format!(
                        "rule {}: Bash({}) is denied: {}",
                        rule.id,
                        rule.pattern,
                        explain_pattern(&rule.pattern)
                    ),
                    rule: rule.id.clone(),
                };
            }
            Some(rule) if rule.behavior == Behavior::Ask => {
                let kind = if WRAPPER_PATTERNS.contains(&rule.pattern.as_str()) {
                    AskKind::WrapperHidden
                } else {
                    AskKind::PolicyAsk
                };
                asked = Some((
                    format!(
                        "rule {}: Bash({}) requires confirmation",
                        rule.id, rule.pattern
                    ),
                    rule.id.clone(),
                    is_destructive_ask_pattern(&rule.tool, &rule.pattern),
                    kind,
                ));
            }
            Some(rule) if rule.behavior == Behavior::Allow => {
                allowed = Some(rule.id.clone());
            }
            Some(_) => {}
            None => {
                // No rule matched this segment. Single simple commands stay
                // fail-open; compound commands escalate the unknown part.
                if segments.len() > 1 {
                    any_allow_only_gap = true;
                }
            }
        }
    }

    if let Some((reason, rule, destructive, kind)) = asked {
        return RuleDecision::Ask {
            reason,
            rule,
            destructive,
            kind,
        };
    }
    if any_allow_only_gap {
        return RuleDecision::Ask {
            reason: "part of this compound command matches no allow rule; confirmation required"
                .to_string(),
            rule: "builtin:compound-gap".to_string(),
            destructive: false,
            kind: AskKind::CompoundGap,
        };
    }
    RuleDecision::Allow {
        rule: allowed.unwrap_or_else(|| "default:allow".to_string()),
    }
}

/// Split a command on shell separators, respecting single/double quotes and
/// backslash escapes. Quote state is per-segment (an unterminated quote
/// makes the rest one segment — fail-open parsing, strict matching).
fn split_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            current.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    current.push(n);
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                current.push(c);
            }
            '\\' => {
                current.push(c);
                if let Some(n) = chars.next() {
                    current.push(n);
                }
            }
            '\n' => {
                segments.push(std::mem::take(&mut current));
            }
            ';' => segments.push(std::mem::take(&mut current)),
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                segments.push(std::mem::take(&mut current));
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                segments.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    segments.push(current);
    segments
}

/// Trim + collapse runs of whitespace (outside quotes) + strip leading
/// `VAR=value` assignments + reduce the program word to its basename.
///
/// Whitespace collapsing keeps `rm  -rf  /` from dodging `rm -rf /`.
/// Assignment stripping keeps `FOO=1 rm -rf /` from hiding the verb.
/// Basename reduction keeps `/bin/rm -rf /` from dodging `rm` rules.
/// All three are match-time normalization only; the original text is what
/// gets logged and explained.
fn normalize_segment(segment: &str) -> String {
    // Collapse whitespace outside quotes first (quote-aware so
    // `echo "a  b"` keeps its spacing for pattern fidelity).
    let mut collapsed = String::with_capacity(segment.len());
    let mut quote: Option<char> = None;
    let mut in_space = false;
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            collapsed.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    collapsed.push(n);
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                in_space = false;
                collapsed.push(c);
            }
            c if c.is_whitespace() => {
                if !in_space {
                    collapsed.push(' ');
                    in_space = true;
                }
            }
            _ => {
                in_space = false;
                collapsed.push(c);
            }
        }
    }
    let mut rest = collapsed.trim().to_string();
    while let Some(first_end) = rest.find(' ') {
        let (first, after) = rest.split_at(first_end);
        let strips_assignment = first.contains('=')
            && !first.starts_with('=')
            && !first.contains('"')
            && !first.contains('\'')
            && first.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
        if !strips_assignment {
            break;
        }
        rest = after.trim_start().to_string();
    }
    // Reduce the program word to its basename (`/bin/rm` → `rm`).
    if let Some(space) = rest.find(' ') {
        let (prog, args) = rest.split_at(space);
        let base = prog.rsplit('/').next().unwrap_or(prog);
        return format!("{base}{args}");
    }
    rest.rsplit('/').next().unwrap_or(&rest).to_string()
}

/// Whether a segment hides its payload (backticks or `$(...)`), respecting
/// that these markers inside quotes are literal text.
fn is_opaque(segment: &str) -> bool {
    let mut chars = segment.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '`' => return true,
            '$' if chars.peek() == Some(&'(') => return true,
            _ => {}
        }
    }
    false
}

/// Whether a normalized segment is a recursive delete whose every operand
/// is the filesystem root (`/`,collapsed `//…`, or `/*`).
///
/// A glob cannot express "slashes only", so this is a tiny absolute
/// path-based deny (`builtin:root-wipe`), not a pattern. `rm -rf /tmp/*`
/// is NOT a root wipe (it asks via `rm *-r*`); `rm -rf / /*` IS.
fn is_root_wipe(normalized: &str) -> bool {
    let mut tokens = normalized.split_whitespace();
    let prog = tokens.next().unwrap_or("");
    // Unix exec is case-sensitive: `RM` is not `rm`.
    if prog != "rm" {
        return false;
    }
    let mut recursive = false;
    let mut operands = 0u32;
    let mut all_root = true;
    for token in tokens {
        if token == "--" {
            continue;
        }
        if token.starts_with("--") {
            if token == "--recursive" {
                recursive = true;
            }
            continue;
        }
        if token.starts_with('-') && token.len() > 1 {
            if token.contains('r') {
                recursive = true;
            }
            continue;
        }
        operands += 1;
        // Collapse slash runs for the comparison (`//` is `/`).
        let collapsed: String = {
            let mut out = String::with_capacity(token.len());
            let mut last_slash = false;
            for c in token.chars() {
                if c == '/' {
                    if !last_slash {
                        out.push(c);
                    }
                    last_slash = true;
                } else {
                    last_slash = false;
                    out.push(c);
                }
            }
            out
        };
        if collapsed != "/" && collapsed != "/*" {
            all_root = false;
        }
    }
    recursive && operands > 0 && all_root
}

fn explain_pattern(pattern: &str) -> String {
    if pattern.contains("rm") || pattern.contains("mkfs") || pattern.contains("wipefs") {
        "destructive deletion outside any recoverable scope".to_string()
    } else if pattern.contains("dd") {
        "raw block-device write".to_string()
    } else if pattern.contains("shred") {
        "unrecoverable file shredding".to_string()
    } else if pattern.contains("/dev/") {
        "raw device target".to_string()
    } else if pattern.contains("/etc/passwd") || pattern.contains("/etc/shadow") {
        "system credential store overwrite".to_string()
    } else if pattern.contains('|') {
        "piped remote-code execution".to_string()
    } else if pattern.contains("..") {
        "parent-directory traversal outside the visible tree".to_string()
    } else {
        format!("matches denied shape {pattern:?}")
    }
}

/// Frozen builtin rules (v1). `git status`-family commands never prompt;
/// catastrophic shapes are always denied with an explanation.
///
/// Tiering for deletion: plain `rm` (bounded, often git-recoverable) stays
/// fail-open; any `rm` with a recursive flag asks; system roots, home roots,
/// `/`-anchored and `..`-traversal deletes deny. Workspace-nested absolute
/// deletes (`/home/…`, `/data`, `/opt`, `/srv`, `/tmp`) deliberately only
/// ask: the rule engine has no workspace context in v1, and denying the
/// tree the agent works in would brick legitimate cleanup.
///
/// IDs are stable (`builtin:<n>`) so denial logs stay comparable across
/// releases. Changing this list is a v1-grammar event: review required.
pub fn builtin_rules() -> Vec<Rule> {
    let mut rules: Vec<Rule> = Vec::new();
    let mut id = 0usize;
    let mut add = |tool: &str, pattern: &str, behavior: Behavior| {
        id += 1;
        rules.push(Rule {
            tool: tool.to_string(),
            pattern: pattern.to_string(),
            behavior,
            id: format!("builtin:{id}"),
        });
    };

    // --- Catastrophic denies (absolute; strictest-wins keeps them on top).
    // Root and home roots.
    add("Bash", "rm -rf /", Behavior::Deny);
    add("Bash", "rm -fr /", Behavior::Deny);
    add("Bash", "rm * /", Behavior::Deny);
    add("Bash", "rm --no-preserve-root*", Behavior::Deny);
    add("Bash", "rm *--no-preserve-root*", Behavior::Deny);
    add("Bash", "rm -rf ~", Behavior::Deny);
    add("Bash", "rm -rf ~/*", Behavior::Deny);
    add("Bash", "rm -rf $HOME", Behavior::Deny);
    add("Bash", "rm -rf $HOME/*", Behavior::Deny);
    // `${HOME}`: braces are literal here via `[...]` classes (bare braces
    // would mean alternation to globset — see validate_rule_pattern).
    add("Bash", "rm -rf $[{]HOME[}]*", Behavior::Deny);
    // System roots (never legitimate workspaces).
    for root in [
        "/etc", "/usr", "/bin", "/sbin", "/var", "/boot", "/lib", "/lib64", "/root", "/System",
        "/Library", "/private",
    ] {
        add("Bash", &format!("rm *-rf {root}*"), Behavior::Deny);
    }
    // Parent-traversal deletes resolve outside the visible tree.
    add("Bash", "rm *-rf */../*", Behavior::Deny);
    add("Bash", "rm *-rf */..", Behavior::Deny);
    // Wrapped destroys.
    add("Bash", "sudo rm *", Behavior::Deny);
    add("Bash", "doas rm *", Behavior::Deny);
    add("Bash", "sudo dd *", Behavior::Deny);
    // Disk and block devices.
    add("Bash", "mkfs*", Behavior::Deny);
    add("Bash", "wipefs *", Behavior::Deny);
    for dev in [
        "sd", "hd", "nvme", "mmcblk", "vd", "loop", "dm-", "md", "dasd", "nbd", "xvd",
    ] {
        add("Bash", &format!("dd *of=/dev/{dev}*"), Behavior::Deny);
    }
    add("Bash", "shred /*", Behavior::Deny);
    add("Bash", "shred ~/*", Behavior::Deny);
    // Fork-bomb shape (`:(){:|:&};:`): parens are literal in globs.
    add("Bash", ":()*", Behavior::Deny);
    // Whole-command shapes (also matched pre-segment; listed here so the
    // deny list stays the single source of truth).
    add("Bash", "*| sh", Behavior::Deny);
    add("Bash", "*| sh *", Behavior::Deny);
    add("Bash", "*|sh", Behavior::Deny);
    add("Bash", "*|sh *", Behavior::Deny);
    add("Bash", "*| bash*", Behavior::Deny);
    add("Bash", "*| zsh*", Behavior::Deny);
    add("Bash", "*| dash*", Behavior::Deny);
    add("Bash", "*| powershell*", Behavior::Deny);
    for dev in ["sd", "hd", "nvme", "mmcblk"] {
        add("Bash", &format!("*> /dev/{dev}*"), Behavior::Deny);
    }
    add("Bash", "*> /etc/passwd", Behavior::Deny);
    add("Bash", "*> /etc/shadow", Behavior::Deny);

    // --- Ask (destructive-but-bounded, opaque, or credential-adjacent).
    add("Bash", "rm *-r*", Behavior::Ask);
    add("Bash", "rm -rf *", Behavior::Ask);
    add("Bash", "rm -fr *", Behavior::Ask);
    add("Bash", "find *-delete*", Behavior::Ask);
    add("Bash", "find *-exec*", Behavior::Ask);
    add("Bash", "git clean *", Behavior::Ask);
    add("Bash", "chmod -R *", Behavior::Ask);
    add("Bash", "chown -R *", Behavior::Ask);
    add("Bash", "dd *", Behavior::Ask);
    add("Bash", "shred *", Behavior::Ask);
    add("Bash", "sudo *", Behavior::Ask);
    add("Bash", "doas *", Behavior::Ask);
    add("Bash", "su *", Behavior::Ask);
    // Wrappers hide the verb: never deny (the inner command may be benign),
    // never allow (the inner command may be catastrophic).
    add("Bash", "env *", Behavior::Ask);
    add("Bash", "nice *", Behavior::Ask);
    add("Bash", "timeout *", Behavior::Ask);
    add("Bash", "nohup *", Behavior::Ask);
    add("Bash", "stdbuf *", Behavior::Ask);
    add("Bash", "setsid *", Behavior::Ask);
    add("Bash", "command *", Behavior::Ask);
    add("Bash", "builtin *", Behavior::Ask);
    add("Bash", "exec *", Behavior::Ask);
    add("Bash", "*sh -c *", Behavior::Ask);
    add("Bash", "xargs *", Behavior::Ask);
    add("Bash", "curl *", Behavior::Ask);
    add("Bash", "wget *", Behavior::Ask);
    add("Bash", "ssh *", Behavior::Ask);
    add("Bash", "cat *id_rsa*", Behavior::Ask);
    add("Bash", "cat *.pem", Behavior::Ask);
    add("Bash", "cat *.key", Behavior::Ask);
    add("Bash", "cat *.env*", Behavior::Ask);
    add("Bash", "cat *credentials*", Behavior::Ask);

    // --- Allow (never prompt; deny/ask still win via strictest-wins).
    add("Bash", "git status*", Behavior::Allow);
    add("Bash", "git diff*", Behavior::Allow);
    add("Bash", "git log*", Behavior::Allow);
    add("Bash", "git branch*", Behavior::Allow);
    add("Bash", "git show *", Behavior::Allow);
    add("Bash", "git remote -v", Behavior::Allow);
    add("Bash", "ls*", Behavior::Allow);
    add("Bash", "pwd", Behavior::Allow);
    add("Bash", "whoami", Behavior::Allow);
    add("Bash", "echo *", Behavior::Allow);
    add("Bash", "which *", Behavior::Allow);
    add("Bash", "cat *", Behavior::Allow);
    add("Bash", "true", Behavior::Allow);
    add("Bash", "cargo test *", Behavior::Allow);
    add("Bash", "cargo build *", Behavior::Allow);
    add("Bash", "cargo --version", Behavior::Allow);

    rules
}

// ---------------------------------------------------------------------------
// Escalation: changing a user rule that denied a call
// ---------------------------------------------------------------------------

/// A user-rule line about to be rewritten, as the escalation card shows it.
///
/// Both halves are carried because the card's whole ceremony is "read the rule
/// you are about to change, then type the path": showing only the new line
/// would let a user confirm a change without seeing what it replaces, and
/// showing only the old line would leave the resulting policy unstated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRuleEdit {
    /// `user:<n>` — the deciding rule's id, which is also its line number.
    pub rule_id: String,
    /// The line as it stands today, trimmed.
    pub old_line: String,
    /// The line that will take its place.
    pub new_line: String,
}

/// Why a rewrite could not be performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteError {
    /// The id is not a `user:<n>` id — a builtin, or a synthesised one like
    /// `default:allow`. Builtins are the frozen floor: nothing a user writes
    /// replaces them, so an escalation cannot target one.
    NotAUserRule,
    /// `user:<n>` names a line that does not exist, is blank, or is a comment.
    /// The rule set and the file have gone out of step (edited since the
    /// decision was taken), and rewriting by line number would then land on a
    /// line nobody named.
    LineMissing,
}

impl std::fmt::Display for RewriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAUserRule => write!(
                f,
                "only a user rule can be changed; this decision came from the \
                 builtin floor"
            ),
            Self::LineMissing => write!(
                f,
                "the rules file no longer has that line — it was edited after \
                 the decision was taken, so reopen the card"
            ),
        }
    }
}

/// Parse a `user:<n>` id into its 1-based line number.
fn user_rule_line_no(rule_id: &str) -> Option<usize> {
    rule_id
        .strip_prefix("user:")?
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
}

/// The text of the rule line `rule_id` names in `text`, if it is one of ours.
///
/// `None` covers both "not a user rule" and "that line is blank or a comment":
/// neither names a rule that can be replaced, and the caller's next step is the
/// same (offer no escalation) so they are one answer at this boundary.
pub fn user_rule_line(text: &str, rule_id: &str) -> Option<String> {
    let line_no = user_rule_line_no(rule_id)?;
    let line = text.lines().nth(line_no - 1)?;
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    Some(trimmed.to_string())
}

/// Rewrite the user rule `rule_id` in `text` so that it reads `new_line`.
///
/// This is the write half of escalation, and the reason it exists rather than
/// an append is a property of the engine: [`decide`] picks the *strictest*
/// matching rule, so an added `=> allow` or `=> ask` line loses to the `=> deny`
/// that is already there. Appending an allow beside a deny changes nothing, and
/// a suggestion that changes nothing is worse than no suggestion — it costs a
/// keystroke and teaches the user the feature is broken. Replacing the denying
/// line is the only edit that actually changes the decision.
///
/// Three refusals, all deliberate:
///
/// * a non-`user:` id is [`RewriteError::NotAUserRule`] — the builtin floor is
///   not a user's to change;
/// * a line that is blank or a comment is [`RewriteError::LineMissing`] — the
///   caller asked to replace a rule and pointed at something that is not one;
/// * the targeted line must parse to a **`Deny`** rule. An escalation exists to
///   lift a refusal; rewriting an allow or an ask would be a silent broadening
///   of policy with no refusal to justify it, so it is refused here rather than
///   trusted to a caller's intent.
///
/// Every other byte of `text` is preserved, including comments, blank lines and
/// whether the file ends in a newline. A policy file is hand-edited, and an
/// edit that reformats the rest of it is an edit the user cannot review in a
/// diff.
///
/// Returns `Ok(None)` when the target line is not a `Deny` rule, `Err` for the
/// two structural refusals above.
pub fn rewrite_user_rule(
    text: &str,
    rule_id: &str,
    new_line: &str,
) -> Result<Option<(String, UserRuleEdit)>, RewriteError> {
    let Some(line_no) = user_rule_line_no(rule_id) else {
        return Err(RewriteError::NotAUserRule);
    };
    let old_line = user_rule_line(text, rule_id).ok_or(RewriteError::LineMissing)?;

    // The line must already be a rule this escalation has a reason to change.
    let parsed = parse_rules(&old_line).map_err(|_| RewriteError::LineMissing)?;
    if parsed.is_empty() || parsed[0].behavior != Behavior::Deny {
        return Ok(None);
    }

    // Rebuild positionally so unrelated lines survive byte-for-byte. `lines()`
    // drops the trailing empty piece of a final newline, so the terminator is
    // re-applied by joining rather than assumed.
    let mut out: Vec<&str> = text.lines().collect();
    let ends_with_newline = text.ends_with('\n');
    if line_no > out.len() {
        return Err(RewriteError::LineMissing);
    }
    out[line_no - 1] = new_line;
    let mut joined = out.join("\n");
    if ends_with_newline {
        joined.push('\n');
    }

    Ok(Some((
        joined,
        UserRuleEdit {
            rule_id: rule_id.to_string(),
            old_line,
            new_line: new_line.to_string(),
        },
    )))
}

/// The replacement line that lifts a `Deny` for `rule` while leaving the same
/// shape gated: `Bash(rm *) => deny` becomes `Bash(rm *) => ask`.
///
/// `ask`, not `allow`, on purpose. The user is being handed an escalation for a
/// call the engine just refused; rewriting their refusal into a silent allow
/// would trade a visible stop for an invisible one, and the confirmation the
/// original policy asked for is exactly what `ask` buys. A user who wants the
/// shape un-gated edits the line themselves.
pub fn escalation_line(rule: &Rule) -> String {
    format!("{}({}) => ask", rule.tool, rule.pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> RuleSet {
        RuleSet::builtins_only()
    }

    #[test]
    fn git_status_never_prompts() {
        for cmd in [
            "git status",
            "git status --short",
            "git diff",
            "git log --oneline",
            "ls -la",
            "pwd",
        ] {
            let d = decide(&set(), "Bash", cmd);
            assert!(matches!(d, RuleDecision::Allow { .. }), "{cmd}: {d:?}");
        }
    }

    #[test]
    fn catastrophic_blocked_with_explanation() {
        for cmd in [
            "rm -rf /",
            "rm -rf /*",
            "sudo rm -rf /tmp",
            "mkfs.ext4 /dev/sda1",
        ] {
            let d = decide(&set(), "Bash", cmd);
            assert!(matches!(d, RuleDecision::Deny { .. }), "{cmd}: {d:?}");
        }
        match decide(&set(), "Bash", "rm -rf /") {
            RuleDecision::Deny { reason, .. } => assert!(!reason.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn compound_with_denied_segment_denies() {
        let d = decide(&set(), "Bash", "git status; rm -rf /");
        assert!(matches!(d, RuleDecision::Deny { .. }), "{d:?}");
    }

    #[test]
    fn compound_with_unknown_segment_asks() {
        let d = decide(&set(), "Bash", "git status; frobnicate --yes");
        assert!(matches!(d, RuleDecision::Ask { .. }), "{d:?}");
    }

    #[test]
    fn single_unknown_stays_fail_open() {
        let d = decide(&set(), "Bash", "frobnicate --yes");
        assert!(matches!(d, RuleDecision::Allow { .. }), "{d:?}");
    }

    #[test]
    fn env_assignment_does_not_hide_payload() {
        let d = decide(&set(), "Bash", "FOO=1 rm -rf /");
        assert!(matches!(d, RuleDecision::Deny { .. }), "{d:?}");
    }

    #[test]
    fn substitution_is_opaque() {
        let d = decide(&set(), "Bash", "echo $(rm -rf /)");
        assert!(matches!(d, RuleDecision::Ask { .. }), "{d:?}");
    }

    #[test]
    fn quoted_separator_stays_one_segment() {
        // `";"` inside quotes is not a separator: single unknown → Allow.
        let d = decide(&set(), "Bash", "echo \";\"");
        assert!(matches!(d, RuleDecision::Allow { .. }), "{d:?}");
    }

    #[test]
    fn strictest_wins_deny_over_allow() {
        // `cat *.pem` (ask) beats `cat *` (allow).
        let d = decide(&set(), "Bash", "cat key.pem");
        assert!(matches!(d, RuleDecision::Ask { .. }), "{d:?}");
    }

    #[test]
    fn parse_rejects_garbage_fail_closed() {
        assert!(parse_rules("Bash(git *) => allow").is_ok());
        assert!(parse_rules("Bash(git *) => sometimes").is_err());
        assert!(parse_rules("Bash git * => allow").is_err());
        assert!(parse_rules("Frobnicate(*) => allow").is_err());
        assert!(parse_rules("Bash(**/*.{pem,key}) => deny").is_err());
        assert!(parse_rules("# comment\n\nBash(ls*) => allow\n").is_ok());
    }

    #[test]
    fn user_deny_beats_builtin_allow() {
        let user = parse_rules("Bash(git status*) => deny").unwrap();
        let set = RuleSet::with_user_rules(user);
        let d = decide(&set, "Bash", "git status");
        assert!(matches!(d, RuleDecision::Deny { .. }), "{d:?}");
    }

    #[test]
    fn root_wipe_shapes() {
        assert!(is_root_wipe("rm -rf /"));
        assert!(is_root_wipe("rm -rf /*"));
        assert!(is_root_wipe("rm -r -f //"));
        assert!(is_root_wipe("rm -rf / /*"));
        assert!(!is_root_wipe("rm -rf /tmp/*"));
        assert!(!is_root_wipe("rm -rf /tmp/"));
        assert!(!is_root_wipe("rm file.txt"));
        assert!(!is_root_wipe("rm -rf"));
        assert!(!is_root_wipe("RM -RF /"));
        assert!(!is_root_wipe("echo rm -rf /"));
        let set = RuleSet::builtins_only();
        for cmd in ["rm -rf /*", "rm -r -f //", "rm -rf / /*"] {
            assert!(
                matches!(decide(&set, "Bash", cmd), RuleDecision::Deny { .. }),
                "{cmd} must deny"
            );
        }
    }

    #[test]
    fn non_bash_tools_match_directly() {
        let user = parse_rules("Read(**/.env) => deny\nWrite(*) => ask").unwrap();
        let set = RuleSet::with_user_rules(user);
        assert!(decide(&set, "Read", ".env").is_deny());
        assert!(matches!(
            decide(&set, "Write", "a.txt"),
            RuleDecision::Ask { .. }
        ));
        assert!(matches!(
            decide(&set, "Read", "src/main.rs"),
            RuleDecision::Allow { .. }
        ));
    }

    #[test]
    fn every_decision_names_its_rule() {
        let set = set();
        // Deny carries a stable id: the whole-command pre-scan fires first
        // for exact glob shapes (`rm -rf /` → builtin:1); the absolute
        // `builtin:root-wipe` backstops shapes no glob names (`rm -rf / /*`).
        match decide(&set, "Bash", "rm -rf /") {
            RuleDecision::Deny { rule, .. } => assert!(rule.starts_with("builtin:"), "{rule}"),
            other => panic!("{other:?}"),
        }
        match decide(&set, "Bash", "rm -rf / /*") {
            RuleDecision::Deny { rule, .. } => assert_eq!(rule, "builtin:root-wipe"),
            other => panic!("{other:?}"),
        }
        match decide(&set, "Bash", "mkfs.ext4 /dev/sda1") {
            RuleDecision::Deny { rule, .. } => assert!(rule.starts_with("builtin:"), "{rule}"),
            other => panic!("{other:?}"),
        }
        // Ask carries the matched rule; synthetic asks carry stable ids.
        match decide(&set, "Bash", "curl https://example.com/x") {
            RuleDecision::Ask { rule, .. } => assert!(rule.starts_with("builtin:"), "{rule}"),
            other => panic!("{other:?}"),
        }
        match decide(&set, "Bash", "echo $(date)") {
            RuleDecision::Ask { rule, .. } => assert_eq!(rule, "builtin:opaque-substitution"),
            other => panic!("{other:?}"),
        }
        match decide(&set, "Bash", "git status; frobnicate --yes") {
            RuleDecision::Ask { rule, .. } => assert_eq!(rule, "builtin:compound-gap"),
            other => panic!("{other:?}"),
        }
        // Allow names the allow rule or the fail-open default.
        match decide(&set, "Bash", "git status") {
            RuleDecision::Allow { rule } => assert!(rule.starts_with("builtin:"), "{rule}"),
            other => panic!("{other:?}"),
        }
        match decide(&set, "Bash", "frobnicate --yes") {
            RuleDecision::Allow { rule } => assert_eq!(rule, "default:allow"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_step_only_for_deny_and_destructive_ask() {
        let set = set();
        // Deny always two-steps (moot at enforcement, explicit for the card).
        assert!(decide(&set, "Bash", "rm -rf /").requires_two_step());
        assert!(decide(&set, "Bash", "curl x | sh").requires_two_step());
        // Visible destructive shapes two-step.
        for cmd in [
            "rm -rf /tmp/x",
            "find / -name x -delete",
            "git clean -fdx",
            "chmod -R 777 /tmp/x",
            "dd if=x of=y.img",
            "shred old.pem",
        ] {
            let d = decide(&set, "Bash", cmd);
            assert!(
                matches!(
                    d,
                    RuleDecision::Ask {
                        destructive: true,
                        ..
                    }
                ),
                "{cmd}: {d:?}"
            );
            assert!(d.requires_two_step(), "{cmd}");
        }
        // Hidden-payload and policy asks stay single-card.
        for cmd in [
            "timeout 5 rm -rf /",
            "env rm -rf /",
            "echo $(rm -rf /)",
            "git status; frobnicate --yes",
            "curl https://example.com/x",
            "ssh deploy@host",
            "cat secrets/key.pem",
            "sudo ls /root",
        ] {
            let d = decide(&set, "Bash", cmd);
            assert!(
                matches!(
                    d,
                    RuleDecision::Ask {
                        destructive: false,
                        ..
                    }
                ),
                "{cmd}: {d:?}"
            );
            assert!(!d.requires_two_step(), "{cmd}");
        }
        assert!(!decide(&set, "Bash", "git status").requires_two_step());
    }

    #[test]
    fn auto_review_only_for_explicit_allows() {
        let set = set();
        // Guardian may auto-approve what policy explicitly allows…
        assert!(decide(&set, "Bash", "git status").auto_reviewable());
        assert!(decide(&set, "Bash", "ls -la").auto_reviewable());
        // …never fail-open unknowns, asks, or denies.
        assert!(!decide(&set, "Bash", "frobnicate --yes").auto_reviewable());
        assert!(!decide(&set, "Bash", "curl https://example.com/x").auto_reviewable());
        assert!(!decide(&set, "Bash", "rm -rf /tmp/x").auto_reviewable());
        assert!(!decide(&set, "Bash", "rm -rf /").auto_reviewable());
        assert!(decide(&set, "Bash", "git status").ask_kind().is_none());
        assert!(decide(&set, "Bash", "rm -rf /").ask_kind().is_none());
    }

    #[test]
    fn ask_kinds_route_stage_two() {
        let set = set();
        for (cmd, want) in [
            ("timeout 5 rm -rf /", AskKind::WrapperHidden),
            ("env rm -rf /", AskKind::WrapperHidden),
            ("sh -c \"rm -rf /\"", AskKind::WrapperHidden),
            ("xargs -a list rm -rf /tmp", AskKind::WrapperHidden),
            ("echo $(rm -rf /)", AskKind::OpaqueSubstitution),
            ("echo `rm -rf /`", AskKind::OpaqueSubstitution),
            ("git status; frobnicate --yes", AskKind::CompoundGap),
            ("curl https://example.com/x", AskKind::PolicyAsk),
            ("sudo ls /root", AskKind::PolicyAsk),
            ("rm -rf /tmp/x", AskKind::PolicyAsk),
        ] {
            let d = decide(&set, "Bash", cmd);
            assert_eq!(d.ask_kind(), Some(want), "{cmd}: {d:?}");
        }
    }

    #[test]
    fn managed_mode_locks_user_rules() {
        let managed = RuleSet::managed("enterprise-mdm");
        assert!(managed.is_managed());
        assert_eq!(managed.governor(), Some("enterprise-mdm"));
        // Builtins still decide under lock.
        assert!(decide(&managed, "Bash", "rm -rf /").is_deny());
        assert!(matches!(
            decide(&managed, "Bash", "git status"),
            RuleDecision::Allow { .. }
        ));
        // Attempted user rules fail closed, naming the governor.
        let user = parse_rules("Bash(git status*) => deny").unwrap();
        let err = RuleSet::managed_with_user_rules(user, "enterprise-mdm").unwrap_err();
        assert!(err.to_string().contains("enterprise-mdm"), "{err}");
        // Empty user set under lock is fine (still managed).
        let ok = RuleSet::managed_with_user_rules(vec![], "enterprise-mdm").unwrap();
        assert!(ok.is_managed());
        // Unmanaged sets are unaffected.
        let plain = RuleSet::builtins_only();
        assert!(!plain.is_managed());
        assert_eq!(plain.governor(), None);
    }
}

#[cfg(test)]
mod escalation_tests {
    use super::*;

    const RULES: &str = "\
# my rules
Bash(rm -rf *) => deny
Bash(git clean *) => deny

Bash(curl * | sh) => deny
";

    #[test]
    fn a_deny_line_is_replaced_and_everything_else_survives() {
        let (text, edit) = rewrite_user_rule(RULES, "user:2", "Bash(rm -rf *) => ask")
            .expect("structural")
            .expect("deny line");
        assert_eq!(edit.old_line, "Bash(rm -rf *) => deny");
        assert_eq!(edit.new_line, "Bash(rm -rf *) => ask");
        assert_eq!(edit.rule_id, "user:2");
        // Comments, blank lines and the trailing newline are all still there.
        assert_eq!(
            text,
            "\
# my rules
Bash(rm -rf *) => ask
Bash(git clean *) => deny

Bash(curl * | sh) => deny
"
        );
        // And the change is what the engine acts on: the same call now asks
        // instead of being refused.
        let before = decide(
            &RuleSet::with_user_rules(parse_rules(RULES).unwrap()),
            "Bash",
            "rm -rf /x",
        );
        assert!(before.is_deny());
        let after = decide(
            &RuleSet::with_user_rules(parse_rules(&text).unwrap()),
            "Bash",
            "rm -rf /x",
        );
        assert_eq!(after.ask_kind(), Some(AskKind::PolicyAsk));
    }

    /// The reason this function exists at all instead of an append: an added
    /// allow cannot win, because the strictest match decides.
    #[test]
    fn appending_an_allow_would_not_have_changed_the_decision() {
        let appended = format!("{RULES}Bash(rm -rf /x) => allow\n");
        let set = RuleSet::with_user_rules(parse_rules(&appended).unwrap());
        assert!(
            decide(&set, "Bash", "rm -rf /x").is_deny(),
            "deny outranks allow, so the append is inert"
        );
    }

    #[test]
    fn a_builtin_or_synthesised_id_is_refused() {
        for id in [
            "builtin:1",
            "default:allow",
            "backend:deny",
            "user:",
            "user:x",
            "user:0",
        ] {
            assert_eq!(
                rewrite_user_rule(RULES, id, "Bash(x) => ask"),
                Err(RewriteError::NotAUserRule),
                "{id}"
            );
        }
    }

    #[test]
    fn a_line_that_is_not_a_rule_is_refused_rather_than_replaced() {
        // Line 1 is a comment; line 4 is blank.
        for id in ["user:1", "user:4", "user:99"] {
            assert_eq!(
                rewrite_user_rule(RULES, id, "Bash(x) => ask"),
                Err(RewriteError::LineMissing),
                "{id}"
            );
        }
    }

    /// Rewriting an allow or an ask would be a policy broadening with no
    /// refusal behind it, so it is declined — and declined as `Ok(None)`,
    /// because that is a statement about the rule, not a broken request.
    #[test]
    fn only_a_deny_line_is_an_escalation_target() {
        let text = "Bash(echo *) => allow\nBash(curl *) => ask\n";
        assert_eq!(
            rewrite_user_rule(text, "user:1", "Bash(echo *) => ask"),
            Ok(None)
        );
        assert_eq!(
            rewrite_user_rule(text, "user:2", "Bash(curl *) => ask"),
            Ok(None)
        );
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_it_that_way() {
        let text = "Bash(rm -rf *) => deny";
        let (out, _) = rewrite_user_rule(text, "user:1", "Bash(rm -rf *) => ask")
            .unwrap()
            .unwrap();
        assert_eq!(out, "Bash(rm -rf *) => ask");
    }

    #[test]
    fn the_escalation_line_gates_the_same_shape_instead_of_silently_allowing_it() {
        let rule = &parse_rules("Bash(git clean *) => deny").unwrap()[0];
        assert_eq!(escalation_line(rule), "Bash(git clean *) => ask");
        // It must re-parse: an escalation that writes invalid policy would fail
        // closed on the next load and take the whole rule file with it.
        assert!(parse_rules(&escalation_line(rule)).is_ok());
    }

    #[test]
    fn user_rule_line_reports_the_line_for_display() {
        assert_eq!(
            user_rule_line(RULES, "user:3").as_deref(),
            Some("Bash(git clean *) => deny")
        );
        assert_eq!(
            user_rule_line(RULES, "user:1"),
            None,
            "a comment is not a rule"
        );
        assert_eq!(user_rule_line(RULES, "builtin:2"), None);
    }
}

//! Deterministic policy evaluation (ALG20).
//!
//! This is a pure, in-process evaluator. It makes no network calls, consults no
//! model, and reads no files: every input is supplied by the caller in a
//! [`Request`] and a [`Policy`]. That makes a decision reproducible from its
//! inputs alone, which is what makes the explanation trustworthy.
//!
//! # Decision procedure (ordered, deterministic)
//!
//! 1. **Malformed policy.** If the policy failed validation, skip the cache and
//!    the rule set. A mutating [`Action`] is forbidden ([`Mode::Forbid`], fail
//!    closed); a pure [`Action::Read`] is allowed ([`Mode::Allow`]) so an
//!    unreadable policy file cannot brick inspection commands. The rule id
//!    names the fallback used.
//! 2. **Principal closure.** Compute the subject's principal set: the subject
//!    name plus every group that (transitively) contains it in the group DAG.
//!    The walk uses a visited set, so a group reachable by two paths is visited
//!    once — a rule is never skipped, duplicated, or "thought about twice".
//! 3. **Candidates.** A rule is a candidate when its principal is in the
//!    closure, its [`Target`] matches the request (command prefix *or* file
//!    glob), and all its [`Condition`]s hold against the request attributes.
//!    A missing attribute makes a condition false (fail closed).
//! 4. **Ordering.** Candidates are sorted by (a) mode precedence,
//!    `Forbid` > `Prompt` > `Allow`, then (b) declaration index ascending.
//!    The first candidate in that order is authoritative. This is deny
//!    precedence *and* first-match-wins: denies are promoted to the front, and
//!    among equally strict rules the earliest declared one wins.
//! 5. **Default.** If nothing matches, the policy's `default_mode` decides and
//!    the rule id is `<default>`.
//!
//! # Cache
//!
//! [`Evaluator::evaluate`] caches by `(generation, request)`. Every mutating
//! operation (`add_rule`, `set_member`, `remove_member`, `add_edge`, …) bumps
//! `generation` and clears the cache, so a membership or rule change cannot
//! leave a stale decision alive. [`CacheStats`] exposes hits/misses/
//! invalidations so callers (and tests) can observe this.
//!
//! # Modes vs. the sandbox rule language
//!
//! `allow | prompt | forbid` here is the ALG20 evaluation vocabulary. It is
//! deliberately separate from the sandbox crate's `allow | ask | deny` tool
//! grammar: this module owns policy *evaluation*, not shell parsing or OS
//! enforcement, and stays free of those concerns.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

const FAIL_CLOSED_RULE: &str = "<malformed-policy:fail-closed>";
const READ_FALLBACK_RULE: &str = "<malformed-policy:read-fallback>";
const DEFAULT_RULE: &str = "<default>";

/// The decision a rule carries, and the outcome of an evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Mode {
    Allow,
    #[default]
    Prompt,
    Forbid,
}

impl Mode {
    /// Higher wins. Used by the ordered procedure to promote denies.
    fn precedence(self) -> u8 {
        match self {
            Mode::Allow => 0,
            Mode::Prompt => 1,
            Mode::Forbid => 2,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Allow => write!(f, "allow"),
            Mode::Prompt => write!(f, "prompt"),
            Mode::Forbid => write!(f, "forbid"),
        }
    }
}

/// The kind of operation being requested. Only [`Action::Read`] is considered
/// non-mutating; every other action is mutating and fails closed on a malformed
/// policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Read,
    Write,
    Execute,
    Delete,
    Network,
}

impl Action {
    pub fn is_mutating(self) -> bool {
        !matches!(self, Action::Read)
    }
}

/// A value in the attribute map carried by a [`Request`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttrValue {
    Str(String),
    Int(i64),
    Bool(bool),
}

impl AttrValue {
    pub fn str_value(v: impl Into<String>) -> Self {
        Self::Str(v.into())
    }
    pub fn int_value(v: i64) -> Self {
        Self::Int(v)
    }
    pub fn bool_value(v: bool) -> Self {
        Self::Bool(v)
    }
}

/// Comparison operator for a [`Condition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttrOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    /// Left is a comma-separated string; right must be a string present in it.
    In,
}

/// An attribute predicate: `attribute <op> value`. Every condition on a rule
/// must hold for the rule to be a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Condition {
    pub key: String,
    pub op: AttrOp,
    pub value: AttrValue,
}

impl Condition {
    pub fn new(key: impl Into<String>, op: AttrOp, value: AttrValue) -> Self {
        Self {
            key: key.into(),
            op,
            value,
        }
    }

    /// Evaluate against a request's attribute map. A missing or ill-typed
    /// attribute is false for every operator, including [`AttrOp::Ne`], so a
    /// rule cannot be widened by omitting data.
    pub fn matches(&self, attrs: &BTreeMap<String, AttrValue>) -> bool {
        let Some(actual) = attrs.get(&self.key) else {
            return false;
        };
        match (self.op, &self.value, actual) {
            (AttrOp::Eq, a, b) => a == b,
            (AttrOp::Ne, a, b) => a != b,
            (AttrOp::Gt, AttrValue::Int(a), AttrValue::Int(b)) => *b > *a,
            (AttrOp::Ge, AttrValue::Int(a), AttrValue::Int(b)) => *b >= *a,
            (AttrOp::Lt, AttrValue::Int(a), AttrValue::Int(b)) => *b < *a,
            (AttrOp::Le, AttrValue::Int(a), AttrValue::Int(b)) => *b <= *a,
            (AttrOp::In, AttrValue::Str(list), AttrValue::Str(v)) => {
                list.split(',').any(|piece| piece.trim() == v.as_str())
            }
            _ => false,
        }
    }
}

/// What a rule matches against. A command prefix and a file glob are the two
/// spellings of a policy scope; a rule names one of them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Matches a normalized command line equal to the prefix or starting with
    /// the prefix followed by whitespace (`"cargo test"` matches
    /// `cargo test --all`, not `cargo testify`).
    CommandPrefix(String),
    /// Anchored glob over a file path. `*` and `?` do not cross `/`; `**` does.
    FileGlob(String),
}

impl Target {
    fn matches(&self, request: &Request) -> bool {
        match self {
            Target::CommandPrefix(prefix) => request
                .command
                .as_deref()
                .is_some_and(|cmd| command_matches(prefix, cmd)),
            Target::FileGlob(glob) => request
                .path
                .as_deref()
                .is_some_and(|path| glob_matches(glob, path)),
        }
    }

    fn describe(&self) -> String {
        match self {
            Target::CommandPrefix(p) => format!("command prefix {p:?}"),
            Target::FileGlob(g) => format!("file glob {g:?}"),
        }
    }
}

/// Collapse runs of whitespace and trim, so command prefixes cannot be dodged
/// with tabs or doubled spaces.
fn normalize_command(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn command_matches(prefix: &str, command: &str) -> bool {
    let prefix = normalize_command(prefix);
    let command = normalize_command(command);
    if prefix.is_empty() {
        return false;
    }
    command == prefix
        || command
            .strip_prefix(&prefix)
            .is_some_and(|rest| rest.starts_with(' '))
}

#[derive(Clone)]
enum GlobToken {
    Lit(char),
    Any,
    Star,
    DoubleStar,
    /// `**/` — zero or more whole directories, including the slash. Matching
    /// empty lets `src/**/*.rs` also match `src/foo.rs`.
    DoubleStarSlash,
}

fn glob_tokens(pattern: &str) -> Vec<GlobToken> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                let mut count = 0;
                while i < chars.len() && chars[i] == '*' {
                    count += 1;
                    i += 1;
                }
                if count >= 2 {
                    if i < chars.len() && chars[i] == '/' {
                        i += 1;
                        tokens.push(GlobToken::DoubleStarSlash);
                    } else {
                        tokens.push(GlobToken::DoubleStar);
                    }
                } else {
                    tokens.push(GlobToken::Star);
                }
            }
            '?' => {
                tokens.push(GlobToken::Any);
                i += 1;
            }
            c => {
                tokens.push(GlobToken::Lit(c));
                i += 1;
            }
        }
    }
    tokens
}

/// Anchored full glob match. `*` and `?` stop at `/`; `**` spans directories.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let tokens = glob_tokens(pattern);
    let text: Vec<char> = text.chars().collect();
    let (n, m) = (tokens.len(), text.len());
    let mut dp = vec![vec![false; m + 1]; n + 1];
    dp[0][0] = true;
    for i in 1..=n {
        match tokens[i - 1] {
            GlobToken::Lit(c) => {
                for j in 1..=m {
                    if text[j - 1] == c {
                        dp[i][j] = dp[i - 1][j - 1];
                    }
                }
            }
            GlobToken::Any => {
                for j in 1..=m {
                    if text[j - 1] != '/' {
                        dp[i][j] = dp[i - 1][j - 1];
                    }
                }
            }
            GlobToken::Star => {
                for j in 0..=m {
                    dp[i][j] = dp[i - 1][j] || (j > 0 && text[j - 1] != '/' && dp[i][j - 1]);
                }
            }
            GlobToken::DoubleStar | GlobToken::DoubleStarSlash => {
                for j in 0..=m {
                    dp[i][j] = dp[i - 1][j] || (j > 0 && dp[i][j - 1]);
                }
            }
        }
    }
    dp[n][m]
}

/// One policy rule: for `principal`, a `target` match with all `conditions`
/// true grants `mode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Stable, human-readable id. Named in explanations. Must be unique.
    pub id: String,
    /// A subject name or a group name in the DAG.
    pub principal: String,
    pub target: Target,
    pub conditions: Vec<Condition>,
    pub mode: Mode,
}

impl Rule {
    pub fn new(
        id: impl Into<String>,
        principal: impl Into<String>,
        target: Target,
        mode: Mode,
    ) -> Self {
        Self {
            id: id.into(),
            principal: principal.into(),
            target,
            conditions: Vec::new(),
            mode,
        }
    }

    pub fn with_condition(mut self, condition: Condition) -> Self {
        self.conditions.push(condition);
        self
    }

    fn describe(&self) -> String {
        format!(
            "{} (principal {:?}, {}{})",
            self.id,
            self.principal,
            self.target.describe(),
            if self.conditions.is_empty() {
                String::new()
            } else {
                format!(", {} condition(s)", self.conditions.len())
            }
        )
    }
}

/// Group DAG. An edge `parent -> child` means "parent contains child", so a
/// direct member of `child` is also a member of `parent`. `parents` is stored
/// child-to-parent to make the transitive walk cheap.
#[derive(Debug, Clone, Default)]
struct GroupGraph {
    /// group -> direct subject members
    members: BTreeMap<String, BTreeSet<String>>,
    /// child group -> groups that contain it
    parents: BTreeMap<String, BTreeSet<String>>,
}

impl GroupGraph {
    fn insert_member(&mut self, group: &str, subject: &str) {
        self.members
            .entry(group.to_string())
            .or_default()
            .insert(subject.to_string());
    }

    fn remove_member(&mut self, group: &str, subject: &str) {
        if let Some(set) = self.members.get_mut(group) {
            set.remove(subject);
        }
    }

    fn add_edge(&mut self, parent: &str, child: &str) {
        self.parents
            .entry(child.to_string())
            .or_default()
            .insert(parent.to_string());
    }

    fn remove_edge(&mut self, parent: &str, child: &str) {
        if let Some(set) = self.parents.get_mut(child) {
            set.remove(parent);
        }
    }

    /// All groups that contain `subject`, directly or transitively. A visited
    /// set makes diamond paths and accidental cycles harmless: each group is
    /// returned once, so a rule attached to it is considered exactly once no
    /// matter how many routes reach it.
    fn groups_for(&self, subject: &str) -> BTreeSet<String> {
        let mut groups = BTreeSet::new();
        let mut stack: Vec<String> = self
            .members
            .iter()
            .filter(|(_, members)| members.contains(subject))
            .map(|(group, _)| group.clone())
            .collect();
        while let Some(group) = stack.pop() {
            if !groups.insert(group.clone()) {
                continue;
            }
            if let Some(parents) = self.parents.get(&group) {
                stack.extend(parents.iter().cloned());
            }
        }
        groups
    }

    /// True when the DAG contains a cycle. DFS with a three-colour mark over
    /// the child->parent adjacency; a diamond is not a cycle because the node
    /// is popped off the recursion stack before the second path visits it.
    fn has_cycle(&self) -> bool {
        #[derive(Clone, Copy, PartialEq)]
        enum Colour {
            White,
            Grey,
            Black,
        }
        fn visit(node: &str, graph: &GroupGraph, colours: &mut BTreeMap<String, Colour>) -> bool {
            colours.insert(node.to_string(), Colour::Grey);
            if let Some(parents) = graph.parents.get(node) {
                for parent in parents {
                    match colours.get(parent).copied().unwrap_or(Colour::White) {
                        Colour::Grey => return true,
                        Colour::White => {
                            if visit(parent, graph, colours) {
                                return true;
                            }
                        }
                        Colour::Black => {}
                    }
                }
            }
            colours.insert(node.to_string(), Colour::Black);
            false
        }
        let mut colours = BTreeMap::new();
        self.parents
            .keys()
            .any(|node| visit(node, self, &mut colours))
    }
}

/// A compiled policy. Build with [`PolicyBuilder`]; a construction that fails
/// validation still yields a `Policy` carrying a reason so the evaluator can
/// fail closed rather than silently allow everything.
#[derive(Debug, Clone)]
pub struct Policy {
    rules: Vec<Rule>,
    groups: GroupGraph,
    default_mode: Mode,
    error: Option<String>,
}

impl Policy {
    /// A deliberately malformed policy, for callers that must represent a
    /// broken source (parse error, unsupported grammar, …) without unwrapping.
    pub fn malformed(reason: impl Into<String>) -> Self {
        Self {
            rules: Vec::new(),
            groups: GroupGraph::default(),
            default_mode: Mode::Prompt,
            error: Some(reason.into()),
        }
    }

    pub fn is_malformed(&self) -> bool {
        self.error.is_some()
    }

    pub fn malformed_reason(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn default_mode(&self) -> Mode {
        self.default_mode
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Recompute validation from the current fields. Used after a mutation so
    /// a newly introduced cycle or duplicate id is caught.
    fn revalidate(&mut self) {
        let mut problems = Vec::new();
        let mut seen = BTreeSet::new();
        for rule in &self.rules {
            if rule.id.trim().is_empty() {
                problems.push("rule with empty id".to_string());
            } else if !seen.insert(rule.id.clone()) {
                problems.push(format!("duplicate rule id {:?}", rule.id));
            }
            if rule.principal.trim().is_empty() {
                problems.push(format!("rule {:?} has empty principal", rule.id));
            }
            if rule
                .conditions
                .iter()
                .any(|condition| condition.key.trim().is_empty())
            {
                problems.push(format!("rule {:?} has an empty condition key", rule.id));
            }
            let empty_target = match &rule.target {
                Target::CommandPrefix(p) => p.trim().is_empty(),
                Target::FileGlob(g) => g.trim().is_empty(),
            };
            if empty_target {
                problems.push(format!("rule {:?} has an empty target", rule.id));
            }
        }
        for (group, members) in &self.groups.members {
            if group.trim().is_empty() {
                problems.push("empty group name".to_string());
            }
            if members.iter().any(|subject| subject.trim().is_empty()) {
                problems.push(format!("group {group:?} has an empty member name"));
            }
        }
        for (child, parents) in &self.groups.parents {
            if child.trim().is_empty() {
                problems.push("empty child group name".to_string());
            }
            if parents.iter().any(|parent| parent.trim().is_empty()) {
                problems.push(format!("group {child:?} has an empty parent name"));
            }
        }
        if self.groups.has_cycle() {
            problems.push("group graph contains a cycle".to_string());
        }
        self.error = if problems.is_empty() {
            None
        } else {
            Some(problems.join("; "))
        };
    }
}

/// Builder for a [`Policy`]. Validation is collected, not returned as an
/// error: a malformed policy is a first-class value the evaluator handles.
#[derive(Debug, Clone, Default)]
pub struct PolicyBuilder {
    rules: Vec<Rule>,
    groups: GroupGraph,
    default_mode: Mode,
}

impl PolicyBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// `group` contains `subject` directly.
    pub fn member(mut self, group: impl Into<String>, subject: impl Into<String>) -> Self {
        self.groups.insert_member(&group.into(), &subject.into());
        self
    }

    /// `parent` contains `child`.
    pub fn edge(mut self, parent: impl Into<String>, child: impl Into<String>) -> Self {
        self.groups.add_edge(&parent.into(), &child.into());
        self
    }

    pub fn default_mode(mut self, mode: Mode) -> Self {
        self.default_mode = mode;
        self
    }

    pub fn build(self) -> Policy {
        let mut policy = Policy {
            rules: self.rules,
            groups: self.groups,
            default_mode: self.default_mode,
            error: None,
        };
        policy.revalidate();
        policy
    }
}

/// The input to an evaluation. Everything that can change a decision lives
/// here or in the [`Policy`], so caching by `(generation, request)` is sound.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Request {
    pub subject: String,
    pub action: Action,
    pub command: Option<String>,
    pub path: Option<String>,
    pub attributes: BTreeMap<String, AttrValue>,
}

impl Request {
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            action: Action::Read,
            command: None,
            path: None,
            attributes: BTreeMap::new(),
        }
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.action = action;
        self
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_attr(mut self, key: impl Into<String>, value: AttrValue) -> Self {
        self.attributes.insert(key.into(), value);
        self
    }
}

/// One rule's participation in a decision, kept for explainability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOutcome {
    pub rule_id: String,
    pub matched: bool,
    pub note: String,
}

/// A cached-or-fresh decision. `rule_id` always names the rule that decided it
/// (or one of the documented fallback ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub mode: Mode,
    pub rule_id: String,
    pub explanation: String,
    pub considered: Vec<RuleOutcome>,
    pub generation: u64,
}

impl Decision {
    pub fn allowed(&self) -> bool {
        self.mode == Mode::Allow
    }
    pub fn forbidden(&self) -> bool {
        self.mode == Mode::Forbid
    }
}

/// Observable cache counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: usize,
    pub misses: usize,
    pub invalidations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    generation: u64,
    request: Request,
}

/// Owns a policy, its group membership, and the decision cache. All mutations
/// invalidate the cache and bump the generation.
#[derive(Debug, Clone)]
pub struct Evaluator {
    policy: Policy,
    cache: HashMap<CacheKey, Decision>,
    generation: u64,
    stats: CacheStats,
}

impl Evaluator {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            cache: HashMap::new(),
            generation: 0,
            stats: CacheStats::default(),
        }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Clear cached decisions and advance the generation. Called by every
    /// input mutation; safe to call directly.
    pub fn invalidate(&mut self) {
        self.cache.clear();
        self.generation = self.generation.wrapping_add(1);
        self.stats.invalidations += 1;
    }

    pub fn add_rule(&mut self, rule: Rule) {
        self.policy.rules.push(rule);
        self.policy.revalidate();
        self.invalidate();
    }

    pub fn set_member(&mut self, group: impl Into<String>, subject: impl Into<String>) {
        self.policy
            .groups
            .insert_member(&group.into(), &subject.into());
        self.policy.revalidate();
        self.invalidate();
    }

    pub fn remove_member(&mut self, group: impl Into<String>, subject: impl Into<String>) {
        self.policy
            .groups
            .remove_member(&group.into(), &subject.into());
        self.policy.revalidate();
        self.invalidate();
    }

    pub fn add_edge(&mut self, parent: impl Into<String>, child: impl Into<String>) {
        self.policy.groups.add_edge(&parent.into(), &child.into());
        self.policy.revalidate();
        self.invalidate();
    }

    pub fn remove_edge(&mut self, parent: impl Into<String>, child: impl Into<String>) {
        self.policy
            .groups
            .remove_edge(&parent.into(), &child.into());
        self.policy.revalidate();
        self.invalidate();
    }

    /// Evaluate a request, returning the authoritative decision and its
    /// explanation. Identical calls at the same generation hit the cache.
    pub fn evaluate(&mut self, request: &Request) -> Decision {
        // Step 1: malformed policy — never cached, fail closed for mutations.
        if let Some(reason) = self.policy.malformed_reason() {
            let (mode, rule_id) = if request.action.is_mutating() {
                (Mode::Forbid, FAIL_CLOSED_RULE)
            } else {
                (Mode::Allow, READ_FALLBACK_RULE)
            };
            return Decision {
                mode,
                rule_id: rule_id.to_string(),
                explanation: format!(
                    "malformed policy ({reason}): {mode} for a {} action via {rule_id}",
                    request.action_desc()
                ),
                considered: Vec::new(),
                generation: self.generation,
            };
        }

        let key = CacheKey {
            generation: self.generation,
            request: request.clone(),
        };
        if let Some(cached) = self.cache.get(&key) {
            self.stats.hits += 1;
            return cached.clone();
        }
        self.stats.misses += 1;
        let decision = self.evaluate_uncached(request);
        self.cache.insert(key, decision.clone());
        decision
    }

    fn evaluate_uncached(&self, request: &Request) -> Decision {
        // Step 2: principal closure over the group DAG.
        let mut principals = BTreeSet::new();
        principals.insert(request.subject.clone());
        principals.extend(self.policy.groups.groups_for(&request.subject));

        // Step 3: candidate rules, in declaration order.
        let mut candidates: Vec<(usize, &Rule)> = Vec::new();
        let mut considered = Vec::new();
        for (index, rule) in self.policy.rules.iter().enumerate() {
            let principal_ok = principals.contains(&rule.principal);
            let target_ok = rule.target.matches(request);
            let conditions_ok = rule
                .conditions
                .iter()
                .all(|condition| condition.matches(&request.attributes));
            if principal_ok && target_ok && conditions_ok {
                candidates.push((index, rule));
                considered.push(RuleOutcome {
                    rule_id: rule.id.clone(),
                    matched: true,
                    note: format!("candidate: {}", rule.describe()),
                });
            }
        }

        // Step 4: deny precedence, then first declared wins.
        candidates.sort_by(|(ia, a), (ib, b)| {
            b.mode
                .precedence()
                .cmp(&a.mode.precedence())
                .then_with(|| ia.cmp(ib))
        });

        let (mode, rule_id, explanation) = if let Some((_, winner)) = candidates.first() {
            (
                winner.mode,
                winner.id.clone(),
                format!(
                    "{} by rule {} — {}; chosen by ordered procedure ({} candidates, most strict mode wins, earliest declared breaks ties)",
                    winner.mode,
                    winner.id,
                    winner.describe(),
                    candidates.len()
                ),
            )
        } else {
            // Step 5: default.
            (
                self.policy.default_mode,
                DEFAULT_RULE.to_string(),
                format!(
                    "{} by default rule <default> — no rule matched this request",
                    self.policy.default_mode
                ),
            )
        };

        Decision {
            mode,
            rule_id,
            explanation,
            considered,
            generation: self.generation,
        }
    }
}

impl Request {
    fn action_desc(&self) -> &'static str {
        match self.action {
            Action::Read => "read",
            Action::Write => "write",
            Action::Execute => "execute",
            Action::Delete => "delete",
            Action::Network => "network",
        }
    }
}

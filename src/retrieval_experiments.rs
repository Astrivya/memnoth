//! Offline-replay retrieval experiments (ALG01–06 / ALG15).
//!
//! This module compares four retrieval arms on a fixed, committed fixture:
//!
//! * [`Arm::Bm25`] — lexical BM25 over document terms.
//! * [`Arm::Rrf`] — equal-weight reciprocal-rank fusion of the lexical and
//!   graph arms (`k = 60`), the honest baseline rather than proof of a win.
//! * [`Arm::WeightedFusion`] — weighted rank fusion whose two weights are
//!   **calibrated offline** from the tuning split only.
//! * [`Arm::Graph`] — a typed, bounded (two-hop) graph traversal seeded from
//!   the lexical best match, compared against a lexical-only control.
//!
//! Every arm is replayed through the **same** [`Budget`]: the same maximum
//! candidate count and the same maximum context-token count. The fixture gives
//! every document the same token cost so an equal budget produces an equal
//! context size; [`QueryRun::context_tokens`] is what would actually reach a
//! model, while [`QueryRun::total_tokens`] is cumulative *work* (a candidate
//! processed at two stages is counted twice, by design).
//!
//! Everything is pure and deterministic: `std` plus `serde` for report
//! serialization, **no model, no network, no API key, no file I/O**. There is
//! also a learned-routing fixture ([`learn_policy`] / [`route`]): a frozen
//! per-task-type policy derived from offline replay. It has no `update` method
//! and consumes no runtime feedback, so an online signal cannot change it.
//!
//! Honesty rule: [`decide`] always *names* the best arm on the tuning split,
//! but refuses to claim a win when the fixture carries no held-out split. A
//! number from a tuning-only replay is a hypothesis, not evidence.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

/// Fixture schema id. A change to the seed must change this string.
pub const FIXTURE_ID: &str = "mn-retrieval-experiments/1";
/// Reciprocal-rank fusion depth constant, shared with the shipping engine.
pub const RRF_K: f64 = 60.0;
/// Default maximum candidates every arm may return.
pub const DEFAULT_CANDIDATE_BUDGET: usize = 4;
/// Default maximum context tokens (`4 * 20`).
pub const DEFAULT_TOKEN_BUDGET: usize = 80;
/// Every seeded document costs exactly this many tokens.
pub const TOKEN_COST: usize = 20;
/// Number of lexical top hits used to seed the graph traversal.
pub const SEED_COUNT: usize = 1;
/// Maximum hops traversed from a seed.
pub const MAX_GRAPH_HOPS: usize = 2;
/// Floor added to a mode's quality before calibrating fusion weights.
const CALIBRATION_FLOOR: f64 = 0.05;
/// Tolerance used for score ties and win margins.
const EPSILON: f64 = 1e-9;

/// Fixed keyword-to-topic hints: a deterministic stand-in for the dense arm.
/// No embeddings are computed; this only simulates "semantically near" for the
/// replay fixture, and is labelled as such in any report that uses it.
const TOPIC_HINTS: &[(&str, &str)] = &[
    ("semantic", "vectors"),
    ("vector", "vectors"),
    ("embedding", "vectors"),
    ("ann", "vectors"),
    ("graph", "graphs"),
    ("traversal", "graphs"),
    ("edges", "graphs"),
    ("centrality", "graphs"),
    ("hub", "graphs"),
    ("parser", "parsing"),
    ("lexer", "parsing"),
    ("tokenize", "parsing"),
    ("ast", "parsing"),
    ("node", "parsing"),
    ("cache", "storage"),
    ("revision", "storage"),
    ("invalidation", "storage"),
    ("auth", "security"),
    ("session", "security"),
];

/// The equal budget every arm is replayed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Budget {
    /// Maximum number of candidates packed into the context.
    pub candidates: usize,
    /// Maximum number of context tokens packed into the context.
    pub tokens: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            candidates: DEFAULT_CANDIDATE_BUDGET,
            tokens: DEFAULT_TOKEN_BUDGET,
        }
    }
}

/// The four compared retrieval arms. The declared order is the deterministic
/// reporting order; tie-breaking uses [`Arm::priority`], not declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Arm {
    Bm25,
    Rrf,
    WeightedFusion,
    Graph,
}

impl Arm {
    pub const ALL: [Arm; 4] = [Arm::Bm25, Arm::Rrf, Arm::WeightedFusion, Arm::Graph];

    pub fn as_str(self) -> &'static str {
        match self {
            Arm::Bm25 => "bm25",
            Arm::Rrf => "rrf",
            Arm::WeightedFusion => "weighted-calibrated-fusion",
            Arm::Graph => "graph",
        }
    }

    /// Fixed tie-break rank. Fusion is preferred on an exact score tie because
    /// it is the configuration under evaluation; the lexical control remains
    /// the baseline the decision report compares against.
    fn priority(self) -> u8 {
        match self {
            Arm::WeightedFusion => 4,
            Arm::Rrf => 3,
            Arm::Bm25 => 2,
            Arm::Graph => 1,
        }
    }
}

impl fmt::Display for Arm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which side of the tuning / held-out split a query belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Split {
    Tuning,
    HeldOut,
}

/// The task type used by the learned-routing fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TaskKind {
    Lexical,
    Graph,
    Dense,
}

impl TaskKind {
    pub const ALL: [TaskKind; 3] = [TaskKind::Lexical, TaskKind::Graph, TaskKind::Dense];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Lexical => "lexical",
            TaskKind::Graph => "graph",
            TaskKind::Dense => "dense",
        }
    }
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One seeded document. `tokens` is its context cost; `topic` drives the
/// model-free dense stand-in; `terms` drives BM25.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Document {
    pub id: String,
    pub terms: Vec<String>,
    pub tokens: usize,
    pub topic: String,
}

/// One replay query with hand-written ground truth (`relevant`), never derived
/// from any ranker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Query {
    pub id: String,
    pub text: String,
    pub kind: TaskKind,
    pub relevant: Vec<String>,
    pub split: Split,
}

/// The fixed fixture: documents, queries and an undirected graph edge list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fixture {
    pub documents: Vec<Document>,
    pub queries: Vec<Query>,
    pub edges: Vec<(String, String)>,
}

fn doc(id: &str, terms: &[&str], topic: &str) -> Document {
    Document {
        id: id.to_string(),
        terms: terms.iter().map(|term| (*term).to_string()).collect(),
        tokens: TOKEN_COST,
        topic: topic.to_string(),
    }
}

fn query(id: &str, text: &str, kind: TaskKind, relevant: &[&str], split: Split) -> Query {
    Query {
        id: id.to_string(),
        text: text.to_string(),
        kind,
        relevant: relevant.iter().map(|value| (*value).to_string()).collect(),
        split,
    }
}

/// The committed replay fixture. The ground truth is written by hand from the
/// seed; it is a mechanism test, not a claim about real retrieval quality.
pub fn fixture() -> Fixture {
    Fixture {
        documents: vec![
            doc("d1", &["parser", "tokenize", "lexer"], "parsing"),
            doc("d2", &["graph", "traversal", "edges"], "graphs"),
            doc("d3", &["cache", "invalidation", "revision"], "storage"),
            doc("d4", &["embedding", "vector", "semantic"], "vectors"),
            doc("d5", &["auth", "token", "session"], "security"),
            doc("d6", &["parser", "ast", "node"], "parsing"),
            doc("d7", &["graph", "centrality", "hub"], "graphs"),
            doc("d8", &["vector", "index", "ann"], "vectors"),
            // Late ids: a lexical-only control cannot pack these into the top K
            // when they share no vocabulary with the query, but a graph hop can.
            doc("d9", &["blob", "store", "compaction"], "storage"),
            doc("d10", &["quota", "billing", "invoice"], "billing"),
            doc("d11", &["log", "sink", "buffer"], "logs"),
        ],
        queries: vec![
            query(
                "qt1",
                "parser tokenize",
                TaskKind::Lexical,
                &["d1", "d6"],
                Split::Tuning,
            ),
            // "graph traversal" matches d2/d7 lexically; the relevant d9 is
            // reached only through the d2 -> d9 edge.
            query(
                "qt2",
                "graph traversal",
                TaskKind::Graph,
                &["d9"],
                Split::Tuning,
            ),
            // "semantic" matches d4; the relevant d8 is a topic/graph neighbor.
            query("qt3", "semantic", TaskKind::Dense, &["d8"], Split::Tuning),
            query(
                "qh1",
                "lexer ast",
                TaskKind::Lexical,
                &["d1", "d6"],
                Split::HeldOut,
            ),
            query("qh2", "edges", TaskKind::Graph, &["d7"], Split::HeldOut),
            query("qh3", "ann", TaskKind::Dense, &["d4"], Split::HeldOut),
        ],
        edges: vec![
            ("d1", "d6"),
            ("d2", "d7"),
            ("d2", "d9"),
            ("d4", "d8"),
            ("d3", "d5"),
            ("d9", "d3"),
        ]
        .into_iter()
        .map(|(left, right)| (left.to_string(), right.to_string()))
        .collect(),
    }
}

/// A copy of the fixture with the held-out queries removed, used to prove the
/// decision report refuses to claim a win without a held-out split.
pub fn fixture_no_held_out() -> Fixture {
    let mut value = fixture();
    value.queries.retain(|item| item.split == Split::Tuning);
    value
}

/// Stable content hash (FNV-1a) of a fixture, so a silent seed edit shows up in
/// the report. Sorting makes the hash independent of declaration order.
pub fn fixture_hash(value: &Fixture) -> String {
    let mut lines: Vec<String> = value
        .documents
        .iter()
        .map(|item| {
            format!(
                "d|{}|{}|{}|{}",
                item.id,
                item.terms.join(","),
                item.tokens,
                item.topic
            )
        })
        .collect();
    lines.extend(value.queries.iter().map(|item| {
        format!(
            "q|{}|{}|{}|{}|{}",
            item.id,
            item.text,
            item.kind.as_str(),
            item.relevant.join(","),
            match item.split {
                Split::Tuning => "tuning",
                Split::HeldOut => "held-out",
            }
        )
    }));
    lines.extend(
        value
            .edges
            .iter()
            .map(|(left, right)| format!("e|{left}|{right}")),
    );
    lines.sort();

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lines.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn doc_by_id<'a>(value: &'a Fixture, id: &str) -> Option<&'a Document> {
    value.documents.iter().find(|item| item.id == id)
}

fn tokens_of(value: &Fixture, ids: &[String]) -> usize {
    ids.iter()
        .filter_map(|id| doc_by_id(value, id))
        .map(|item| item.tokens)
        .sum()
}

/// Full BM25 ordering (best first), over all documents. Deterministic: score
/// ties break by document id. No external search library is involved.
fn bm25_rank(value: &Fixture, item: &Query) -> Vec<String> {
    let n_docs = value.documents.len() as f64;
    if n_docs == 0.0 {
        return Vec::new();
    }
    let total_terms: usize = value.documents.iter().map(|entry| entry.terms.len()).sum();
    let avgdl = (total_terms as f64 / n_docs).max(1.0);
    let terms: Vec<&str> = item.text.split_whitespace().collect();
    let (k1, b) = (1.2_f64, 0.75_f64);

    let mut scored: Vec<(String, f64)> = value
        .documents
        .iter()
        .map(|entry| {
            let dl = entry.terms.len() as f64;
            let mut score = 0.0;
            for term in &terms {
                let tf = entry
                    .terms
                    .iter()
                    .filter(|candidate| candidate.as_str() == *term)
                    .count() as f64;
                if tf == 0.0 {
                    continue;
                }
                let df = value
                    .documents
                    .iter()
                    .filter(|other| {
                        other
                            .terms
                            .iter()
                            .any(|candidate| candidate.as_str() == *term)
                    })
                    .count() as f64;
                let idf = ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln();
                score += idf * (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b + b * dl / avgdl));
            }
            (entry.id.clone(), score)
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored.into_iter().map(|(id, _)| id).collect()
}

/// Bounded graph traversal from the lexical seed(s), then a deterministic
/// fill of unvisited documents by id. Every hop is typed only by the seeded
/// edge list; there is no global summary and no unbounded walk.
fn graph_rank(value: &Fixture, item: &Query) -> Vec<String> {
    let mut visited = BTreeSet::new();
    let mut frontier: Vec<String> = bm25_rank(value, item)
        .into_iter()
        .take(SEED_COUNT)
        .collect();
    let mut ordered: Vec<String> = Vec::new();

    for _ in 0..=MAX_GRAPH_HOPS {
        let mut layer: Vec<String> = frontier
            .into_iter()
            .filter(|id| visited.insert(id.clone()))
            .collect();
        layer.sort();
        ordered.extend(layer.iter().cloned());

        let mut next: Vec<String> = Vec::new();
        for id in &layer {
            for (left, right) in &value.edges {
                let neighbor = if left == id {
                    Some(right)
                } else if right == id {
                    Some(left)
                } else {
                    None
                };
                if let Some(neighbor) = neighbor
                    && !visited.contains(neighbor)
                {
                    next.push(neighbor.clone());
                }
            }
        }
        next.sort();
        next.dedup();
        frontier = next;
    }

    for entry in &value.documents {
        if visited.insert(entry.id.clone()) {
            ordered.push(entry.id.clone());
        }
    }
    ordered
}

fn topic_for(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    for (hint, topic) in TOPIC_HINTS {
        if lower.contains(hint) {
            return Some(*topic);
        }
    }
    None
}

/// Model-free dense stand-in: rank by a fixed keyword-to-topic mapping, then
/// by id. This is a replay fixture only; it does not embed anything.
fn dense_rank(value: &Fixture, item: &Query) -> Vec<String> {
    let topic = topic_for(&item.text);
    let mut scored: Vec<(String, u8)> = value
        .documents
        .iter()
        .map(|entry| {
            let match_topic = match topic {
                Some(value) if entry.topic == value => 1,
                _ => 0,
            };
            (entry.id.clone(), match_topic)
        })
        .collect();
    scored.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    scored.into_iter().map(|(id, _)| id).collect()
}

/// Equal-weight RRF. `arms[0]` is ranked first, `arms[1]` second; scores are
/// `sum(1 / (k + rank))` with rank starting at 1. Ties break by id.
pub fn rrf_fuse(arms: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let mut scores: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    for arm in arms {
        for (index, id) in arm.iter().enumerate() {
            let contribution = 1.0 / (k + index as f64 + 1.0);
            *scores.entry(id.clone()).or_insert(0.0) += contribution;
        }
    }
    let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked
}

/// Weights for the calibrated fusion arm. They are always normalized to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FusionWeights {
    pub lexical: f64,
    pub graph: f64,
}

impl Default for FusionWeights {
    fn default() -> Self {
        Self {
            lexical: 0.5,
            graph: 0.5,
        }
    }
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

/// Calibrate two non-negative fusion weights from offline mode qualities.
/// Pure: the same qualities always give the same weights, and the floor keeps
/// a zero-quality arm from being discarded entirely. The lexical weight is
/// rounded and the graph weight is its complement so they sum to exactly 1.
pub fn calibrate_weights(lexical_quality: f64, graph_quality: f64) -> FusionWeights {
    let lexical = (lexical_quality + CALIBRATION_FLOOR).max(CALIBRATION_FLOOR);
    let graph = (graph_quality + CALIBRATION_FLOOR).max(CALIBRATION_FLOOR);
    let lexical = round6(lexical / (lexical + graph));
    FusionWeights {
        lexical,
        graph: round6(1.0 - lexical),
    }
}

/// Weighted rank fusion: `sum(weight * 1 / rank)`, rank starting at 1.
/// Unlike [`rrf_fuse`] it needs no depth constant because the weights are
/// normalized from tuning-split quality rather than assumed equal.
pub fn weighted_fuse(arms: &[Vec<String>], weights: &FusionWeights) -> Vec<(String, f64)> {
    let by_arm = [weights.lexical, weights.graph];
    let mut scores: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    for (arm_index, arm) in arms.iter().enumerate() {
        let weight = by_arm.get(arm_index).copied().unwrap_or(0.0);
        for (index, id) in arm.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += weight / (index as f64 + 1.0);
        }
    }
    let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked
}

/// Pack a full ranking into the equal budget, preserving rank order.
fn pack(ranked: &[String], value: &Fixture, budget: &Budget) -> (Vec<String>, usize) {
    let mut packed = Vec::new();
    let mut tokens = 0;
    for id in ranked {
        if packed.len() >= budget.candidates {
            break;
        }
        let cost = doc_by_id(value, id).map_or(0, |entry| entry.tokens);
        if tokens + cost > budget.tokens {
            continue;
        }
        tokens += cost;
        packed.push(id.clone());
    }
    (packed, tokens)
}

fn recall_at_k(packed: &[String], relevant: &[String]) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    let hits = relevant.iter().filter(|id| packed.contains(id)).count();
    hits as f64 / relevant.len() as f64
}

fn reciprocal_rank(packed: &[String], relevant: &[String]) -> f64 {
    packed
        .iter()
        .position(|id| relevant.contains(id))
        .map_or(0.0, |index| 1.0 / (index as f64 + 1.0))
}

/// One stage's work. `tokens` counts candidates processed at this stage, so
/// the same candidate can contribute to more than one stage's total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StageAccount {
    pub stage: Stage,
    /// Candidates emitted by this stage.
    pub emitted: usize,
    /// Context tokens of the emitted candidates.
    pub tokens: usize,
}

/// Which retrieval stage an [`StageAccount`] accounts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Stage {
    Lexical,
    Graph,
    Fusion,
    Budget,
}

fn account(stage: Stage, ids: &[String], tokens: usize) -> StageAccount {
    StageAccount {
        stage,
        emitted: ids.len(),
        tokens,
    }
}

/// One arm's replay of one query.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryRun {
    pub query_id: String,
    pub task: TaskKind,
    pub arm: Arm,
    pub split: Split,
    /// The packed candidates, in rank order (at most `budget.candidates`).
    pub ranked: Vec<String>,
    /// Context tokens actually used by the packed candidates.
    pub context_tokens: usize,
    /// Per-stage work accounting.
    pub stages: Vec<StageAccount>,
    /// Sum of every stage's `tokens` (work, not context size).
    pub total_tokens: usize,
    /// Candidates emitted by the final stage.
    pub final_candidates: usize,
    pub recall_at_k: f64,
    pub reciprocal_rank: f64,
}

impl QueryRun {
    /// True when the stored totals agree with the per-stage rows and the final
    /// stage is the budget pack. Keeping this as an invariant makes a silent
    /// accounting regression visible without changing any caller.
    pub fn is_consistent(&self) -> bool {
        let sum: usize = self.stages.iter().map(|item| item.tokens).sum();
        let last = self.stages.last();
        self.total_tokens == sum
            && self.final_candidates == last.map_or(0, |item| item.emitted)
            && self.context_tokens == last.map_or(0, |item| item.tokens)
            && last.map(|item| item.stage) == Some(Stage::Budget)
    }
}

/// All arms, all queries, under one shared budget.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Experiment {
    pub fixture_id: String,
    pub fixture_hash: String,
    pub budget: Budget,
    /// Offline-calibrated fusion weights; a function of tuning data only.
    pub weights: FusionWeights,
    pub runs: Vec<QueryRun>,
}

/// The retrieval mode a learned-routing decision can select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RouteChoice {
    Lexical,
    Graph,
    Dense,
}

impl RouteChoice {
    pub const ALL: [RouteChoice; 3] =
        [RouteChoice::Lexical, RouteChoice::Graph, RouteChoice::Dense];

    pub fn as_str(self) -> &'static str {
        match self {
            RouteChoice::Lexical => "lexical",
            RouteChoice::Graph => "graph",
            RouteChoice::Dense => "dense",
        }
    }
}

impl fmt::Display for RouteChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A frozen policy: one mode per task type. Derived offline, never updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RoutingPolicy {
    pub lexical: RouteChoice,
    pub graph: RouteChoice,
    pub dense: RouteChoice,
}

/// Recall@k of a single mode on one query, under the shared budget. Used by
/// the offline policy learner and available to tests for an independent check.
pub fn mode_recall(value: &Fixture, budget: &Budget, item: &Query, choice: RouteChoice) -> f64 {
    let full = mode_ranking(value, item, choice);
    let (packed, _) = pack(&full, value, budget);
    recall_at_k(&packed, &item.relevant)
}

fn mode_ranking(value: &Fixture, item: &Query, choice: RouteChoice) -> Vec<String> {
    match choice {
        RouteChoice::Lexical => bm25_rank(value, item),
        RouteChoice::Graph => graph_rank(value, item),
        RouteChoice::Dense => dense_rank(value, item),
    }
}

fn preferred_choice(kind: TaskKind) -> RouteChoice {
    match kind {
        TaskKind::Lexical => RouteChoice::Lexical,
        TaskKind::Graph => RouteChoice::Graph,
        TaskKind::Dense => RouteChoice::Dense,
    }
}

/// Pick the best mode, breaking exact ties toward the mode named for the task.
fn best_choice(scores: &[(RouteChoice, f64)], kind: TaskKind) -> RouteChoice {
    let preferred = preferred_choice(kind);
    let mut best = scores[0].0;
    let mut best_score = scores[0].1;
    for &(choice, score) in &scores[1..] {
        if score > best_score + EPSILON
            || ((score - best_score).abs() <= EPSILON && choice == preferred)
        {
            best = choice;
            best_score = score;
        }
    }
    best
}

/// Learn a routing policy from the **tuning split only**. There is no runtime
/// feedback path: the returned value is a plain snapshot, and `route` is a
/// read-only lookup. Changing held-out queries cannot change the result.
pub fn learn_policy(value: &Fixture, budget: &Budget) -> RoutingPolicy {
    let mut policy = RoutingPolicy {
        lexical: RouteChoice::Lexical,
        graph: RouteChoice::Graph,
        dense: RouteChoice::Dense,
    };
    for kind in TaskKind::ALL {
        let tuning: Vec<&Query> = value
            .queries
            .iter()
            .filter(|item| item.split == Split::Tuning && item.kind == kind)
            .collect();
        let scores: Vec<(RouteChoice, f64)> = RouteChoice::ALL
            .iter()
            .map(|&choice| {
                let mean = if tuning.is_empty() {
                    0.0
                } else {
                    tuning
                        .iter()
                        .map(|item| mode_recall(value, budget, item, choice))
                        .sum::<f64>()
                        / tuning.len() as f64
                };
                (choice, mean)
            })
            .collect();
        let best = best_choice(&scores, kind);
        match kind {
            TaskKind::Lexical => policy.lexical = best,
            TaskKind::Graph => policy.graph = best,
            TaskKind::Dense => policy.dense = best,
        }
    }
    policy
}

/// Pure lookup into a frozen policy. No online signal is read or written.
pub fn route(policy: &RoutingPolicy, kind: TaskKind) -> RouteChoice {
    match kind {
        TaskKind::Lexical => policy.lexical,
        TaskKind::Graph => policy.graph,
        TaskKind::Dense => policy.dense,
    }
}

fn mean_score(runs: &[&QueryRun], arm: Arm) -> f64 {
    let values: Vec<f64> = runs
        .iter()
        .filter(|run| run.arm == arm)
        .map(|run| run.recall_at_k)
        .collect();
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn arm_scores(runs: &[&QueryRun]) -> Vec<(Arm, f64)> {
    Arm::ALL
        .iter()
        .map(|&arm| (arm, mean_score(runs, arm)))
        .collect()
}

/// Deterministic best arm: highest mean recall, then fixed priority.
fn best_arm(scores: &[(Arm, f64)]) -> Arm {
    let mut best = scores[0].0;
    let mut best_score = scores[0].1;
    for &(arm, score) in &scores[1..] {
        if score > best_score + EPSILON
            || ((score - best_score).abs() <= EPSILON && arm.priority() > best.priority())
        {
            best = arm;
            best_score = score;
        }
    }
    best
}

/// The decision: a win only when a held-out split exists and the held-out
/// best arm strictly beats the lexical baseline there.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum DecisionVerdict {
    /// A best arm is named from tuning, but no win may be claimed.
    RefusedNoHeldOut { best_on_tuning: Arm },
    /// Held-out confirmation: the named arm beat the lexical baseline.
    Win { arm: Arm, margin: f64 },
    /// Held-out ran, but no arm beat the baseline there.
    NoWin { arm: Arm },
}

impl DecisionVerdict {
    /// True only for a confirmed held-out win.
    pub fn claims_win(&self) -> bool {
        matches!(self, DecisionVerdict::Win { .. })
    }
}

/// A report that names the best arm but never overstates the evidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionReport {
    pub fixture_id: String,
    pub fixture_hash: String,
    pub budget: Budget,
    /// The best arm on the tuning split. Always named, even on refusal.
    pub tuning_best: Arm,
    pub tuning_scores: Vec<(Arm, f64)>,
    /// Empty when the fixture has no held-out queries.
    pub held_out_scores: Vec<(Arm, f64)>,
    pub held_out_best: Option<Arm>,
    pub verdict: DecisionVerdict,
    pub summary: String,
}

/// Replay every arm on every query under one shared, equal budget.
///
/// Fusion weights are calibrated from tuning queries only, then frozen before
/// any held-out query is touched. The returned runs are already budget-packed
/// and metric-scored, with per-stage accounting attached.
pub fn run_experiment(value: &Fixture, budget: &Budget) -> Experiment {
    // Calibration sees tuning data only.
    let tuning: Vec<&Query> = value
        .queries
        .iter()
        .filter(|item| item.split == Split::Tuning)
        .collect();
    let lexical_quality = if tuning.is_empty() {
        0.0
    } else {
        tuning
            .iter()
            .map(|item| mode_recall(value, budget, item, RouteChoice::Lexical))
            .sum::<f64>()
            / tuning.len() as f64
    };
    let graph_quality = if tuning.is_empty() {
        0.0
    } else {
        tuning
            .iter()
            .map(|item| mode_recall(value, budget, item, RouteChoice::Graph))
            .sum::<f64>()
            / tuning.len() as f64
    };
    let weights = calibrate_weights(lexical_quality, graph_quality);

    let mut runs = Vec::new();
    for item in &value.queries {
        let lexical = bm25_rank(value, item);
        let graph = graph_rank(value, item);
        let rrf: Vec<String> = rrf_fuse(&[lexical.clone(), graph.clone()], RRF_K)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let weighted: Vec<String> = weighted_fuse(&[lexical.clone(), graph.clone()], &weights)
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        for arm in Arm::ALL {
            let full = match arm {
                Arm::Bm25 => &lexical,
                Arm::Graph => &graph,
                Arm::Rrf => &rrf,
                Arm::WeightedFusion => &weighted,
            };
            let (packed, context_tokens) = pack(full, value, budget);
            let stages = match arm {
                Arm::Bm25 => vec![
                    account(Stage::Lexical, full, tokens_of(value, full)),
                    account(Stage::Budget, &packed, context_tokens),
                ],
                Arm::Graph => vec![
                    account(Stage::Graph, full, tokens_of(value, full)),
                    account(Stage::Budget, &packed, context_tokens),
                ],
                Arm::Rrf | Arm::WeightedFusion => vec![
                    account(Stage::Lexical, &lexical, tokens_of(value, &lexical)),
                    account(Stage::Graph, &graph, tokens_of(value, &graph)),
                    account(Stage::Fusion, full, tokens_of(value, full)),
                    account(Stage::Budget, &packed, context_tokens),
                ],
            };
            let total_tokens = stages.iter().map(|item| item.tokens).sum();
            let final_candidates = packed.len();
            runs.push(QueryRun {
                query_id: item.id.clone(),
                task: item.kind,
                arm,
                split: item.split,
                ranked: packed.clone(),
                context_tokens,
                stages,
                total_tokens,
                final_candidates,
                recall_at_k: recall_at_k(&packed, &item.relevant),
                reciprocal_rank: reciprocal_rank(&packed, &item.relevant),
            });
        }
    }

    Experiment {
        fixture_id: FIXTURE_ID.to_string(),
        fixture_hash: fixture_hash(value),
        budget: *budget,
        weights,
        runs,
    }
}

/// Build a decision report from a replay. A best arm is always named from the
/// tuning split, but a `Win` requires a non-empty held-out split where that
/// story survives; otherwise the report says so in plain language.
pub fn decide(experiment: &Experiment) -> DecisionReport {
    let tuning: Vec<&QueryRun> = experiment
        .runs
        .iter()
        .filter(|run| run.split == Split::Tuning)
        .collect();
    let held_out: Vec<&QueryRun> = experiment
        .runs
        .iter()
        .filter(|run| run.split == Split::HeldOut)
        .collect();

    // An absent split stays absent in the report: do not manufacture four
    // zero-score rows that could be mistaken for a measured held-out result.
    let tuning_scores = if tuning.is_empty() {
        Vec::new()
    } else {
        arm_scores(&tuning)
    };
    let held_out_scores = if held_out.is_empty() {
        Vec::new()
    } else {
        arm_scores(&held_out)
    };
    let tuning_best = if tuning.is_empty() {
        Arm::Bm25
    } else {
        best_arm(&tuning_scores)
    };

    let (held_out_best, verdict, summary) = if held_out.is_empty() {
        (
            None,
            DecisionVerdict::RefusedNoHeldOut {
                best_on_tuning: tuning_best,
            },
            format!(
                "best on tuning is {tuning_best}, but the fixture has no held-out split; \
                 refusing to claim a win from tuning data alone"
            ),
        )
    } else {
        let best = best_arm(&held_out_scores);
        let baseline = mean_score(&held_out, Arm::Bm25);
        let best_score = mean_score(&held_out, best);
        let margin = best_score - baseline;
        let verdict = if margin > EPSILON {
            DecisionVerdict::Win { arm: best, margin }
        } else {
            DecisionVerdict::NoWin { arm: best }
        };
        let summary = format!(
            "held-out best is {best} (mean recall@{} {best_score:.3} vs lexical baseline \
             {baseline:.3}); tuning nominee was {tuning_best}",
            experiment.budget.candidates
        );
        (Some(best), verdict, summary)
    };

    DecisionReport {
        fixture_id: experiment.fixture_id.clone(),
        fixture_hash: experiment.fixture_hash.clone(),
        budget: experiment.budget,
        tuning_best,
        tuning_scores,
        held_out_scores,
        held_out_best,
        verdict,
        summary,
    }
}

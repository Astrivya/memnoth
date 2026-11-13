//! Impact analysis — reverse reachability, risk, tests-for, and the repo map.
//!
//! Everything here answers with a [`Precision`] label and with counts, never
//! prose alone. The lexical scanner is the zero-dependency fallback: it resolves
//! same-file and same-name mentions rather than type information, so an answer
//! says `lexical` and never implies rust-analyzer parity.
//!
//! Impact is **reverse** reachability: starting from the symbol being changed,
//! follow `relations.target_id → relations.source_id` (who depends on it), not
//! the callee direction. The previous implementation counted `source_id →
//! target_id`, which is the blast radius of everything the symbol calls — the
//! opposite question.
//!
//! Confidence is a ladder multiplied along the path (G-03): every hop costs the
//! edge's base confidence, a name-only match is halved again, and a path is
//! `exact` only when **every** hop on it was resolved. A two-hop exact chain at
//! the lexical tier is therefore `0.9 × 0.9 = 0.81`, and it says so.

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::repomap::{DEFAULT_MAP_TOKENS, RepoMapEntry};
use crate::{GraphAnswer, Precision, Store, best_available, repomap};

pub use crate::precision::lsp_available;

/// Relation kinds this scanner treats as a resolved edge.
pub const RESOLVED_EDGE_KINDS: [&str; 3] = ["calls", "imports", "references"];

/// Path class for a fully resolved chain.
pub const EXACT: &str = "exact";

/// Path class for a chain containing at least one name-only hop.
pub const LIKELY: &str = "likely";

/// One path that depends on the changed symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactedPath {
    /// Symbol or file that depends on the target.
    pub qname: String,
    /// File the dependent lives in.
    pub file: String,
    /// Hops from the changed symbol (1 = direct dependent).
    pub depth: usize,
    /// Confidence that this path is real, in `[0, 1]`.
    pub confidence: f64,
    /// `exact` for a resolved chain, `likely` for a name-only one.
    pub class: String,
}

impl ImpactedPath {
    /// Whether every hop on this path was a resolved edge.
    pub fn is_exact(&self) -> bool {
        self.class == EXACT
    }
}

/// Structured counts, so `"12 exact, 28 likely"` is a field and not a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImpactCounts {
    /// Dependents reached through fully resolved chains.
    pub exact: usize,
    /// Dependents reached through at least one name-only hop.
    pub likely: usize,
    /// Dependents one hop away.
    pub direct: usize,
    /// Every dependent reached within the requested depth.
    pub transitive: usize,
}

impl ImpactCounts {
    /// Total dependents found.
    pub fn total(&self) -> usize {
        self.exact + self.likely
    }

    /// The label the answer prints, e.g. `12 exact, 28 likely`.
    pub fn label(&self) -> String {
        format!("{} exact, {} likely", self.exact, self.likely)
    }

    /// Whether nothing depends on the target.
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Inputs to the risk score, kept visible so the number can be checked.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RiskInputs {
    /// Direct dependents of the symbol.
    pub fan_in: usize,
    /// Commits touching the symbol's file in the last 90 days.
    pub churn_90d: u32,
    /// Fraction of the file's lines covered by tests, in `[0, 1]`.
    pub test_coverage: f64,
}

/// `fan_in × churn(90d) × (1 − test_coverage)`, with the inputs kept.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RiskScore {
    pub inputs: RiskInputs,
    /// The product above. Zero when there is no fan-in, no churn, or full
    /// test coverage — each is a real reason not to worry.
    pub score: f64,
}

impl RiskScore {
    /// Compute the score and keep every input beside it.
    ///
    /// `test_coverage` is clamped to `[0, 1]`; a negative or NaN coverage is
    /// treated as 0 (worst case) rather than silently lowering the score.
    pub fn compute(inputs: RiskInputs) -> Self {
        let coverage = if inputs.test_coverage.is_finite() {
            inputs.test_coverage.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let score = inputs.fan_in as f64 * f64::from(inputs.churn_90d) * (1.0 - coverage);
        Self { inputs, score }
    }

    /// Inputs rendered for the answer, e.g. `fan_in=12 churn_90d=4 coverage=0.50`.
    pub fn explain(&self) -> String {
        format!(
            "fan_in={} churn_90d={} test_coverage={:.2}",
            self.inputs.fan_in, self.inputs.churn_90d, self.inputs.test_coverage
        )
    }
}

/// A test file that exercises the target or one of its dependents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffectedTest {
    /// Test file, workspace-relative.
    pub file: String,
    /// Confidence that this test exercises the target, in `[0, 1]`.
    pub confidence: f64,
    /// `exact` when a resolved chain leads here, `likely` for a name match.
    pub class: String,
    /// Why the file was included: `dependent` or `name mention`.
    pub matched_on: String,
}

/// How wide a symbol may get before the answer discloses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WideSymbolPolicy {
    /// Direct dependents above this count mark the symbol as wide.
    pub max_fan_in: usize,
    /// Direct dependents above this count mark an ubiquitous name as wide.
    pub ubiquitous_min_direct: usize,
}

impl Default for WideSymbolPolicy {
    fn default() -> Self {
        Self {
            max_fan_in: 200,
            ubiquitous_min_direct: 20,
        }
    }
}

/// A disclosure that the target is referenced so widely that the rows are a
/// ranked sample, not a complete list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WideSymbol {
    /// Direct dependents found.
    pub direct_dependents: usize,
    /// The threshold that was crossed.
    pub threshold: usize,
    /// Whether the name itself is one of the ubiquitous ones.
    pub ubiquitous_name: bool,
    /// Why the answer is a sample, in one clause.
    pub reason: String,
    /// What the caller can do to narrow it.
    pub hint: String,
    /// Rows found but not returned after the cap.
    pub withheld: usize,
}

/// Names that appear almost everywhere, so a bare mention says little.
///
/// Impact on one of these is still answerable, but the report must say it is a
/// ranked sample rather than a complete list of call sites.
const UBIQUITOUS_LEAVES: [&str; 24] = [
    "new",
    "default",
    "clone",
    "drop",
    "fmt",
    "to_string",
    "from",
    "into",
    "main",
    "get",
    "set",
    "run",
    "call",
    "iter",
    "len",
    "open",
    "read",
    "write",
    "next",
    "unwrap",
    "expect",
    "parse",
    "send",
    "test",
];

/// Whether a qualified name's leaf is an ubiquitous one.
pub fn is_ubiquitous_name(qname: &str) -> bool {
    let leaf = qname
        .rsplit("::")
        .next()
        .unwrap_or(qname)
        .rsplit('.')
        .next()
        .unwrap_or(qname)
        .to_ascii_lowercase();
    UBIQUITOUS_LEAVES.contains(&leaf.as_str())
}

/// Options for one impact answer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactOptions {
    /// Hops to walk outward from the target.
    pub depth: usize,
    /// Commits touching the file in the last 90 days; `None` leaves risk out.
    pub churn_90d: Option<u32>,
    /// Fraction of the file covered by tests; `None` counts as uncovered.
    pub test_coverage: Option<f64>,
    /// Render a repo map for the returned files at this budget.
    pub map_tokens: Option<usize>,
    /// When a symbol counts as too wide to list exhaustively.
    pub wide_policy: WideSymbolPolicy,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        Self {
            depth: 3,
            churn_90d: None,
            test_coverage: None,
            map_tokens: None,
            wide_policy: WideSymbolPolicy::default(),
        }
    }
}

impl ImpactOptions {
    /// Options for an answer that also cites the repo map it used.
    pub fn with_map(depth: usize) -> Self {
        Self {
            depth,
            map_tokens: Some(DEFAULT_MAP_TOKENS),
            ..Self::default()
        }
    }
}

/// Blast radius of one symbol, as data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactReport {
    /// The symbol asked about, or the nearest match the lexical index found.
    pub target: String,
    /// Whether the target exists in the index.
    pub resolved: bool,
    /// How the edges were produced.
    pub precision: Precision,
    /// The store generation the answer was read from.
    pub generation: u64,
    /// Exact and likely counts, direct and transitive.
    pub counts: ImpactCounts,
    /// Resolved dependents.
    pub exact: Vec<ImpactedPath>,
    /// Name-only dependents that could not be resolved.
    pub likely: Vec<ImpactedPath>,
    /// Dependents reachable within the requested depth (deduplicated).
    pub transitive_count: usize,
    /// Ranked rows actually returned after the cap.
    pub impacted: Vec<ImpactedPath>,
    /// Test files that exercise the target or one of its dependents.
    pub affected_tests: Vec<AffectedTest>,
    /// The repo-map slice used to rank, if one was requested.
    pub repo_map_slice: Vec<RepoMapEntry>,
    /// Risk score with its inputs; `None` when no churn value was supplied.
    pub risk: Option<RiskScore>,
    /// Present when the target is referenced too widely to list exhaustively.
    pub wide: Option<WideSymbol>,
    /// Whether the row cap withheld dependents.
    pub truncated: bool,
}

/// Row cap so an impact answer stays a ranked answer, not a 500-row dump.
pub const IMPACT_ROW_CAP: usize = 50;

/// Rows cited from the repo map, so the slice stays a slice.
pub const IMPACT_MAP_SLICE_CAP: usize = 16;

impl GraphAnswer for ImpactReport {
    fn precision(&self) -> Precision {
        self.precision
    }
}

impl ImpactReport {
    /// `12 exact, 28 likely`, ready for a heading.
    pub fn count_label(&self) -> String {
        self.counts.label()
    }

    /// The one-line answer: counts, witnesses, precision. The structured fields
    /// remain the answer; this is the sentence beside them.
    pub fn summary(&self) -> String {
        let mut sentence = format!(
            "{} — {} ({} direct), precision {}",
            self.target,
            self.counts.label(),
            self.counts.direct,
            self.precision.label()
        );
        if !self.affected_tests.is_empty() {
            sentence.push_str(&format!(
                ", {} affected test{}",
                self.affected_tests.len(),
                if self.affected_tests.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        if let Some(risk) = &self.risk {
            sentence.push_str(&format!(", risk {:.2} ({})", risk.score, risk.explain()));
        }
        if self.truncated {
            sentence.push_str(", ranked sample");
        }
        sentence
    }

    /// Whether nothing depends on the target.
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// Affected test files, in ranking order.
    pub fn affected_test_files(&self) -> Vec<&str> {
        self.affected_tests
            .iter()
            .map(|test| test.file.as_str())
            .collect()
    }

    /// The repo-map lines the answer cites.
    pub fn repo_map_lines(&self) -> Vec<String> {
        self.repo_map_slice
            .iter()
            .map(RepoMapEntry::render_line)
            .collect()
    }
}

/// Blast radius of `qname`, by reverse reachability over resolved edges.
///
/// `churn_90d` and `test_coverage` are caller-supplied facts (git history is
/// not available to this crate); passing `None` leaves `risk` out rather than
/// inventing a number.
pub fn blast_radius(
    store: &Store,
    qname: &str,
    depth: usize,
    churn_90d: Option<u32>,
    test_coverage: Option<f64>,
) -> Result<ImpactReport> {
    blast_radius_with_options(
        store,
        qname,
        ImpactOptions {
            depth,
            churn_90d,
            test_coverage,
            ..ImpactOptions::default()
        },
    )
}

/// Blast radius of `qname` with every option named.
pub fn blast_radius_with_options(
    store: &Store,
    qname: &str,
    options: ImpactOptions,
) -> Result<ImpactReport> {
    let conn = store.conn();
    let precision = best_available();
    let generation = store.generation().unwrap_or(0);
    let depth = options.depth.max(1);

    let target: Option<(i64, String)> = conn
        .prepare("SELECT id, file_path FROM symbols WHERE qname = ? LIMIT 1")?
        .query(params![qname])?
        .next()?
        .map(|row| Ok::<_, rusqlite::Error>((row.get(0)?, row.get(1)?)))
        .transpose()?;

    let Some((target_id, target_file)) = target else {
        return Ok(empty_report(qname, precision, generation));
    };

    let (exact, likely) = reverse_reachability(conn, target_id, depth, precision)?;

    let direct = exact
        .iter()
        .chain(likely.iter())
        .filter(|path| path.depth == 1)
        .count();
    let counts = ImpactCounts {
        exact: exact.len(),
        likely: likely.len(),
        direct,
        transitive: exact.len() + likely.len(),
    };

    let mut impacted: Vec<ImpactedPath> = exact.iter().chain(likely.iter()).cloned().collect();
    impacted.sort_by(rank_paths);
    let truncated = impacted.len() > IMPACT_ROW_CAP;
    impacted.truncate(IMPACT_ROW_CAP);

    let all_paths: Vec<ImpactedPath> = exact.iter().chain(likely.iter()).cloned().collect();
    let affected_tests = affected_tests(conn, &all_paths, qname)?;

    let fan_in = exact
        .iter()
        .filter(|path| path.depth == 1 && path.class == EXACT)
        .count();
    let risk = options.churn_90d.map(|churn| {
        RiskScore::compute(RiskInputs {
            fan_in,
            churn_90d: churn,
            test_coverage: options.test_coverage.unwrap_or(0.0),
        })
    });

    let repo_map_slice = match options.map_tokens {
        Some(tokens) if tokens > 0 => {
            let map = repomap::build(store, tokens)?;
            let mut files: Vec<String> = impacted.iter().map(|path| path.file.clone()).collect();
            files.push(target_file);
            map.slice_for_files(&files)
                .into_iter()
                .take(IMPACT_MAP_SLICE_CAP)
                .collect()
        }
        _ => Vec::new(),
    };

    let wide = wide_symbol(qname, counts.direct, options.wide_policy, counts.transitive);

    Ok(ImpactReport {
        target: qname.to_string(),
        resolved: true,
        precision,
        generation,
        counts,
        exact,
        likely,
        transitive_count: counts.transitive,
        impacted,
        affected_tests,
        repo_map_slice,
        risk,
        wide,
        truncated,
    })
}

fn empty_report(target: &str, precision: Precision, generation: u64) -> ImpactReport {
    ImpactReport {
        target: target.to_string(),
        resolved: false,
        precision,
        generation,
        counts: ImpactCounts::default(),
        exact: Vec::new(),
        likely: Vec::new(),
        transitive_count: 0,
        impacted: Vec::new(),
        affected_tests: Vec::new(),
        repo_map_slice: Vec::new(),
        risk: None,
        wide: None,
        truncated: false,
    }
}

fn rank_paths(a: &ImpactedPath, b: &ImpactedPath) -> std::cmp::Ordering {
    let class = |path: &ImpactedPath| usize::from(!path.is_exact());
    a.depth
        .cmp(&b.depth)
        .then_with(|| class(a).cmp(&class(b)))
        .then_with(|| b.confidence.total_cmp(&a.confidence))
        .then_with(|| a.qname.cmp(&b.qname))
}

fn wide_symbol(
    qname: &str,
    direct: usize,
    policy: WideSymbolPolicy,
    total: usize,
) -> Option<WideSymbol> {
    let ubiquitous_name = is_ubiquitous_name(qname);
    let (threshold, reason) = if direct > policy.max_fan_in {
        (
            policy.max_fan_in,
            format!(
                "{direct} direct dependents exceed the {} row target",
                policy.max_fan_in
            ),
        )
    } else if ubiquitous_name && direct >= policy.ubiquitous_min_direct {
        (
            policy.ubiquitous_min_direct,
            format!(
                "the leaf name is ubiquitous in this tree, so {direct} mentions say less than a resolved call would"
            ),
        )
    } else {
        return None;
    };

    let withheld = total.saturating_sub(IMPACT_ROW_CAP);
    Some(WideSymbol {
        direct_dependents: direct,
        threshold,
        ubiquitous_name,
        reason,
        hint: "narrow with a file-scoped query, or raise the cap and read the ranked rows"
            .to_string(),
        withheld,
    })
}

/// Walk dependents (`target_id → source_id`) outward from `start_id`.
///
/// Returns `(exact, likely)`. A row is `exact` when every hop on its shortest
/// path was a resolved call/import/reference edge, `likely` otherwise. Each hop
/// multiplies the confidence by the edge's base value, so confidence decays with
/// distance instead of being asserted per depth.
fn reverse_reachability(
    conn: &rusqlite::Connection,
    start_id: i64,
    depth: usize,
    precision: Precision,
) -> Result<(Vec<ImpactedPath>, Vec<ImpactedPath>)> {
    use std::collections::{HashMap, HashSet};

    let mut seen: HashSet<i64> = HashSet::from([start_id]);
    let mut best: HashMap<i64, ImpactedPath> = HashMap::new();
    let mut level: Vec<(i64, f64, bool)> = vec![(start_id, 1.0, true)];

    let mut stmt = conn.prepare(
        "SELECT s.id, s.qname, s.file_path, r.kind FROM relations r
         JOIN symbols s ON r.source_id = s.id
         WHERE r.target_id = ?
         ORDER BY s.qname, s.id",
    )?;

    for hop in 1..=depth {
        let mut candidates: HashMap<i64, ImpactedPath> = HashMap::new();
        for (node, confidence, all_exact) in &level {
            let rows: Vec<(i64, String, String, String)> = stmt
                .query_map(params![node], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .filter_map(Result::ok)
                .collect();
            for (id, qname, file, kind) in rows {
                if seen.contains(&id) {
                    continue;
                }
                let resolved = RESOLVED_EDGE_KINDS.contains(&kind.as_str());
                let candidate = ImpactedPath {
                    qname,
                    file,
                    depth: hop,
                    confidence: (confidence * precision.edge_confidence_for(resolved)).min(1.0),
                    class: if *all_exact && resolved {
                        EXACT
                    } else {
                        LIKELY
                    }
                    .to_string(),
                };
                match candidates.get(&id) {
                    Some(existing) if !prefer(&candidate, existing) => {}
                    _ => {
                        candidates.insert(id, candidate);
                    }
                }
            }
        }

        if candidates.is_empty() {
            break;
        }

        let mut current: Vec<(i64, ImpactedPath)> = candidates.into_iter().collect();
        current.sort_by(|a, b| a.1.qname.cmp(&b.1.qname).then_with(|| a.0.cmp(&b.0)));
        level = Vec::with_capacity(current.len());
        for (id, path) in current {
            seen.insert(id);
            level.push((id, path.confidence, path.is_exact()));
            best.insert(id, path);
        }
    }

    let mut exact = Vec::new();
    let mut likely = Vec::new();
    for path in best.into_values() {
        if path.is_exact() {
            exact.push(path);
        } else {
            likely.push(path);
        }
    }
    exact.sort_by(rank_paths);
    likely.sort_by(rank_paths);
    Ok((exact, likely))
}

/// Keep the candidate with the better evidence: exact before likely, then the
/// higher confidence, then the lexicographically smaller name.
fn prefer(candidate: &ImpactedPath, existing: &ImpactedPath) -> bool {
    match (candidate.is_exact(), existing.is_exact()) {
        (true, false) => true,
        (false, true) => false,
        _ => candidate
            .confidence
            .total_cmp(&existing.confidence)
            .then_with(|| existing.qname.cmp(&candidate.qname))
            .is_gt(),
    }
}

/// Test files that exercise `target` or one of its dependents.
///
/// A file counts as a test when its path says so in one of the languages this
/// scanner walks (`tests/`, `spec/`, `__tests__/`, `src/test/`, `test_*`,
/// `_test.*`, `.test.`, `.spec.`, `Tests.swift`, `Test.php`, `_spec.rb`). This is
/// a filter over real rows, never a guess: no match is `[]`, which a caller must
/// read as "none found", not "none exist".
fn affected_tests(
    conn: &rusqlite::Connection,
    paths: &[ImpactedPath],
    target: &str,
) -> Result<Vec<AffectedTest>> {
    use std::collections::BTreeMap;

    let mut tests: BTreeMap<String, AffectedTest> = BTreeMap::new();
    for path in paths.iter().filter(|path| looks_like_test(&path.file)) {
        let entry = AffectedTest {
            file: path.file.clone(),
            confidence: path.confidence,
            class: path.class.clone(),
            matched_on: "dependent".to_string(),
        };
        let replace = match tests.get(&path.file) {
            Some(existing) => {
                entry.confidence > existing.confidence
                    || (entry.confidence == existing.confidence
                        && entry.is_exact()
                        && !existing.is_exact())
            }
            None => true,
        };
        if replace {
            tests.insert(path.file.clone(), entry);
        }
    }

    // A direct test symbol may live beside the target without a reverse edge
    // (lexical scanner); include symbols whose own path is a test and whose
    // qname mentions the target's leaf name.
    let leaf = leaf_name(target);
    let mut stmt = conn.prepare("SELECT qname, file_path FROM symbols")?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(Result::ok)
        .collect();
    for (qname, file) in rows {
        if looks_like_test(&file) && qname.contains(&leaf) {
            tests.entry(file.clone()).or_insert(AffectedTest {
                file,
                confidence: 0.5,
                class: LIKELY.to_string(),
                matched_on: "name mention".to_string(),
            });
        }
    }

    let mut tests: Vec<AffectedTest> = tests.into_values().collect();
    tests.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| a.file.cmp(&b.file))
    });
    Ok(tests)
}

impl AffectedTest {
    fn is_exact(&self) -> bool {
        self.class == EXACT
    }
}

fn leaf_name(qname: &str) -> String {
    qname
        .rsplit("::")
        .next()
        .unwrap_or(qname)
        .rsplit('.')
        .next()
        .unwrap_or(qname)
        .to_string()
}

/// `tests-for` as its own answer: which tests exercise the symbol, and how sure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestsForReport {
    /// The symbol asked about.
    pub target: String,
    /// Whether the index knows the symbol.
    pub resolved: bool,
    /// Test files, most confident first.
    pub tests: Vec<AffectedTest>,
    /// How the edges behind the answer were produced.
    pub precision: Precision,
}

impl GraphAnswer for TestsForReport {
    fn precision(&self) -> Precision {
        self.precision
    }
}

/// Which tests exercise `qname`, across the languages this scanner walks.
pub fn tests_for(store: &Store, qname: &str, depth: usize) -> Result<TestsForReport> {
    let report = blast_radius_with_options(
        store,
        qname,
        ImpactOptions {
            depth,
            ..ImpactOptions::default()
        },
    )?;
    Ok(TestsForReport {
        target: report.target,
        resolved: report.resolved,
        tests: report.affected_tests,
        precision: report.precision,
    })
}

fn looks_like_test(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_lowercase();
    normalized.contains("/tests/")
        || normalized.starts_with("tests/")
        || normalized.starts_with("test_")
        || normalized.contains("/test_")
        || normalized.contains("/test/")
        || normalized.starts_with("spec/")
        || normalized.contains("/spec/")
        || normalized.contains("__tests__/")
        || normalized.contains("_test.")
        || normalized.contains(".test.")
        || normalized.contains(".spec.")
        || normalized.ends_with("_spec.rb")
        || normalized.ends_with("tests.swift")
        || normalized.ends_with("test.php")
        || normalized.ends_with("tests.cs")
        || normalized.ends_with("test.kt")
}

/// Whether `qname` is a test-like symbol name.
pub fn is_test_symbol(qname: &str) -> bool {
    let lower = qname.to_lowercase();
    lower.contains("test") || lower.contains("spec")
}

/// Render a repo map that fits an estimated token budget.
///
/// Kept as the plain-`Vec<String>` entry point; [`repomap::build`] returns the
/// structured map with the ranks beside it.
pub fn repo_map(store: &Store, token_budget: usize) -> Result<Vec<String>> {
    Ok(repomap::build(store, token_budget)?.lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let path = std::env::temp_dir().join(format!(
            "mn-impact-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        store
            .conn()
            .execute_batch(
                "INSERT INTO symbols (id, qname, kind, file_path, span_start, span_end)
                    VALUES (1, 'app::helper', 'function', 'src/app.rs', 1, 5),
                           (2, 'app::middle',  'function', 'src/app.rs', 6, 9),
                           (3, 'app::entry',   'function', 'src/lib.rs', 1, 9),
                           (4, 'tests::helper_is_right', 'function', 'tests/app_test.rs', 1, 9);
                 INSERT INTO relations (source_id, target_id, kind)
                    VALUES (2, 1, 'calls'), (3, 2, 'calls'), (4, 1, 'calls');",
            )
            .unwrap();
        store
    }

    #[test]
    fn impact_walks_dependents_not_callees() {
        let store = store();
        // `app::entry` calls `app::middle`; changing `helper` must not report
        // `entry` as a direct dependent, but must reach it transitively.
        let report = blast_radius(&store, "app::helper", 3, None, None).unwrap();
        assert_eq!(report.precision.label(), "tree-sitter");
        let direct: Vec<&str> = report
            .exact
            .iter()
            .filter(|p| p.depth == 1)
            .map(|p| p.qname.as_str())
            .collect();
        assert!(direct.contains(&"app::middle"));
        assert!(direct.contains(&"tests::helper_is_right"));
        let transitive: Vec<&str> = report.impacted.iter().map(|p| p.qname.as_str()).collect();
        assert!(transitive.contains(&"app::entry"));
    }

    #[test]
    fn impact_lists_affected_tests_and_never_invents_them() {
        let store = store();
        let report = blast_radius(&store, "app::helper", 2, None, None).unwrap();
        assert!(
            report
                .affected_tests
                .iter()
                .any(|test| test.file.contains("tests/app_test.rs")),
            "{:?}",
            report.affected_tests
        );
        let none = blast_radius(&store, "app::missing", 2, None, None).unwrap();
        assert!(none.affected_tests.is_empty());
        assert!(none.impacted.is_empty());
        assert!(!none.resolved);
    }

    #[test]
    fn impact_is_capped_and_ranked() {
        let store = store();
        let report = blast_radius(&store, "app::helper", 1, Some(4), Some(0.5)).unwrap();
        assert!(report.impacted.len() <= IMPACT_ROW_CAP);
        assert!(report.impacted.windows(2).all(|w| w[0].depth <= w[1].depth));
        let risk = report.risk.expect("risk is present when churn is supplied");
        assert_eq!(risk.inputs.fan_in, 2);
        assert!((risk.score - 2.0 * 4.0 * 0.5).abs() < f64::EPSILON);
        assert!(risk.explain().contains("fan_in=2"));
    }

    #[test]
    fn risk_is_zero_for_fully_tested_or_unused_symbols() {
        let tested = RiskScore::compute(RiskInputs {
            fan_in: 9,
            churn_90d: 9,
            test_coverage: 1.0,
        });
        assert_eq!(tested.score, 0.0);
        let unused = RiskScore::compute(RiskInputs {
            fan_in: 0,
            churn_90d: 9,
            test_coverage: 0.0,
        });
        assert_eq!(unused.score, 0.0);
        let nan = RiskScore::compute(RiskInputs {
            fan_in: 1,
            churn_90d: 1,
            test_coverage: f64::NAN,
        });
        assert_eq!(nan.score, 1.0, "NaN coverage is worst case, not free");
    }

    #[test]
    fn repo_map_fits_its_token_budget() {
        let store = store();
        let map = repo_map(&store, 40).unwrap();
        let used: usize = map
            .iter()
            .map(|line| crate::repomap::estimate_tokens(line))
            .sum();
        assert!(used <= 40, "used {used} tokens: {map:?}");
        assert!(!map.is_empty());
        // A smaller budget never returns more than a larger one.
        assert!(repo_map(&store, 10).unwrap().len() <= map.len());
    }

    #[test]
    fn test_paths_are_recognised_across_languages() {
        for path in [
            "tests/app.rs",
            "src/test_utils.py",
            "pkg/foo_test.go",
            "web/a.test.ts",
            "web/a.spec.ts",
            "src/test/java/AppTest.java",
            "spec/models/user_spec.rb",
            "web/__tests__/button.tsx",
            "ios/Tests/AppTests.swift",
            "php/src/AppTest.php",
            "dotnet/App.Tests.cs",
            "android/src/test/AppTest.kt",
        ] {
            assert!(looks_like_test(path), "{path}");
        }
        assert!(!looks_like_test("src/app.rs"));
        assert!(!looks_like_test("src/contested.rs"));
    }

    #[test]
    fn confidence_is_a_ladder_multiplied_along_the_path() {
        let store = store();
        let report = blast_radius(&store, "app::helper", 3, None, None).unwrap();
        let direct = report
            .exact
            .iter()
            .find(|p| p.qname == "app::middle")
            .expect("direct dependent");
        let transitive = report
            .impacted
            .iter()
            .find(|p| p.qname == "app::entry")
            .expect("transitive dependent");
        // Tree-sitter tier: one edge is 0.85, two hops multiply to 0.7225.
        assert_eq!(direct.confidence, 0.85);
        assert!((transitive.confidence - 0.7225).abs() < 1e-9);
        assert!(transitive.confidence < direct.confidence);
        assert!(transitive.is_exact(), "both hops were resolved");
    }

    #[test]
    fn counts_are_data_and_the_summary_is_only_a_sentence() {
        let store = store();
        let report = blast_radius(&store, "app::helper", 3, None, None).unwrap();
        assert_eq!(report.count_label(), "3 exact, 0 likely");
        assert_eq!(report.counts.direct, 2);
        assert_eq!(report.counts.transitive, 3);
        assert_eq!(report.counts.total(), 3);
        assert!(report.summary().contains("3 exact, 0 likely"));
        assert!(report.summary().contains("precision tree-sitter"));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["counts"]["exact"], 3);
        assert_eq!(json["precision"], "tree-sitter");
        assert_eq!(json["resolved"], true);
    }

    #[test]
    fn a_name_only_hop_downgrades_the_class_and_halves_confidence() {
        let store = store();
        store
            .conn()
            .execute_batch(
                "INSERT INTO symbols (id, qname, kind, file_path, span_start, span_end)
                    VALUES (5, 'app::indirect', 'function', 'src/other.rs', 1, 4);
                 INSERT INTO relations (source_id, target_id, kind) VALUES (5, 3, 'mentions');",
            )
            .unwrap();
        let report = blast_radius(&store, "app::helper", 3, None, None).unwrap();
        let indirect = report
            .likely
            .iter()
            .find(|p| p.qname == "app::indirect")
            .expect("a name-only hop is reported as likely");
        assert_eq!(indirect.class, "likely");
        assert_eq!(report.counts.likely, 1);
        // Three hops: two resolved calls, then the name-only `mentions` hop
        // (whose discounted factor already carries one base edge).
        let ladder = Precision::TreeSitter.edge_confidence_for(true).powi(2)
            * Precision::TreeSitter.edge_confidence_for(false);
        assert!((indirect.confidence - ladder).abs() < 1e-9);
    }

    #[test]
    fn a_wide_symbol_is_disclosed_rather_than_paged() {
        let store = store();
        // `new` is ubiquitous, and 20 direct mentions is the default floor.
        store
            .conn()
            .execute(
                "INSERT INTO symbols (id, qname, kind, file_path, span_start, span_end)
                 VALUES (99, 'app::new', 'function', 'src/app.rs', 20, 24)",
                [],
            )
            .unwrap();
        for id in 100..160 {
            store
                .conn()
                .execute(
                    "INSERT INTO symbols (id, qname, kind, file_path, span_start, span_end)
                     VALUES (?, ?, 'function', 'src/caller.rs', 1, 9)",
                    params![id, format!("app::caller{id}")],
                )
                .unwrap();
            store
                .conn()
                .execute(
                    "INSERT INTO relations (source_id, target_id, kind) VALUES (?, 99, 'calls')",
                    params![id],
                )
                .unwrap();
        }
        let report = blast_radius(&store, "app::new", 1, None, None).unwrap();
        let truncated = report.truncated;
        let impacted_len = report.impacted.len();
        let summary = report.summary();
        let wide = report
            .wide
            .expect("a ubiquitous name with 60 callers is wide");
        assert!(wide.ubiquitous_name);
        assert_eq!(wide.direct_dependents, 60);
        assert!(wide.reason.contains("ubiquitous"));
        assert!(truncated);
        assert_eq!(impacted_len, IMPACT_ROW_CAP);
        assert_eq!(wide.withheld, 10);
        assert!(summary.contains("ranked sample"));
    }

    #[test]
    fn tests_for_answers_on_its_own() {
        let store = store();
        let report = tests_for(&store, "app::helper", 2).unwrap();
        assert!(report.resolved);
        assert_eq!(report.precision.label(), "tree-sitter");
        assert!(!report.tests.is_empty());
        assert_eq!(report.tests[0].class, "exact");
    }

    #[test]
    fn a_map_slice_names_the_files_it_ranked_with() {
        let store = store();
        let report = blast_radius_with_options(
            &store,
            "app::helper",
            ImpactOptions {
                depth: 2,
                map_tokens: Some(256),
                ..ImpactOptions::default()
            },
        )
        .unwrap();
        assert!(!report.repo_map_slice.is_empty());
        let touched: Vec<&str> = report.impacted.iter().map(|p| p.file.as_str()).collect();
        assert!(
            report
                .repo_map_slice
                .iter()
                .all(|entry| touched.contains(&entry.file.as_str()) || entry.file == "src/app.rs")
        );
        assert!(!report.repo_map_lines().is_empty());
    }
}

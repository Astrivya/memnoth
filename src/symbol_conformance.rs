//! ALG08 — per-language symbol conformance grading.
//!
//! This module does not extract anything. It holds a small, hand-written
//! conformance set for three languages and grades a symbol extractor against it
//! through a narrow read-only subject trait. The CLI wires `memnoth::kg::def`,
//! `refs`, `callers` and `callees` into that trait from the integration test;
//! nothing here reaches into `src/kg/` or `crates/memnoth-codegraph/`.
//!
//! Three rules the grader enforces, all of them about honesty rather than speed:
//!
//! 1. **A tier is not a quality guess.** The only tier a case may claim is the
//!    one its edges actually use. Every expected hit is labelled
//!    [`Resolution::UnresolvedLexical`]: the CLI graph matches names and call
//!    shapes, not types. No case can claim [`Resolution::ResolvedByIdentity`]
//!    without a type-resolved oracle, and no report can claim `lsp` without a
//!    ground-truth dump ([`Oracle::Absent`]).
//! 2. **Unresolved edges are data, not silence.** A dynamic call through a
//!    parameter (`callback(3)`) produces no row in a name-based graph. Each case
//!    carries an explicit [`UnresolvedEdge`] for it, with the reason recorded.
//!    These are reported beside precision/recall, never quietly dropped from the
//!    denominator and never counted as a correct edge.
//! 3. **A mismatch fails the gate.** [`assert_exact`] compares observed and
//!    expected `(path, line, call_shaped)` sets and panics with the diff. The
//!    precision/recall numbers in [`CategoryGrade`] are computed from those same
//!    sets, so a number below 1.0 always corresponds to a failed assertion.
//!
//! Grading is read-only: the subject is queried after indexing is complete. The
//! module has no I/O and no dependency on the network, a model, or a compiler.

use std::collections::BTreeSet;

use memnoth_codegraph::precision::Precision;
use serde::Serialize;

/// One observed or expected hit, reduced to the identity the public API exposes.
///
/// `ImpactHit` carries a path, a line, a kind and a `call_shaped` flag, but no
/// symbol id and no resolved target. That absence is exactly why this grader
/// cannot claim type-resolved identity: there is no field in which it could be
/// returned. Keeping the key to what the API actually gives makes the grade
/// comparable across languages and backends.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct HitKey {
    /// Workspace-relative path, `/` separators.
    pub path: String,
    /// 1-based source line.
    pub line: usize,
    /// Whether the source line reads as `name(...)`.
    pub call_shaped: bool,
}

impl HitKey {
    pub fn new(path: &str, line: usize, call_shaped: bool) -> Self {
        Self {
            path: path.to_string(),
            line,
            call_shaped,
        }
    }
}

impl std::fmt::Display for HitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{} ({})",
            self.path,
            self.line,
            if self.call_shaped { "call" } else { "mention" }
        )
    }
}

/// What binds an expected edge to a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// A name match with no type information. The only identity this build has.
    UnresolvedLexical,
    /// A call through a value (a parameter or callback). There is no declaration
    /// whose name could be matched, so a name-based graph emits no edge at all.
    UnresolvedDynamic,
    /// Would require a type resolver or a language-server oracle. Never present
    /// in a conformance set that has no oracle dump.
    ResolvedByIdentity,
}

impl Resolution {
    /// Whether this resolution carries a type-resolved identity.
    pub fn is_identity_resolved(self) -> bool {
        matches!(self, Self::ResolvedByIdentity)
    }
}

/// Is there an independent oracle that could justify a higher tier?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Oracle {
    /// No ground-truth dump and no language server was consulted.
    Absent,
    /// A pinned ground-truth dump exists. The label names it so a grade can be
    /// reproduced; no such dump is shipped with this conformance set.
    GroundTruthDump(&'static str),
}

impl Oracle {
    pub fn allows_lsp(self) -> bool {
        matches!(self, Self::GroundTruthDump(_))
    }
}

/// One hand-written expected hit.
#[derive(Debug, Clone, Copy)]
pub struct ExpectedHit {
    pub path: &'static str,
    pub line: usize,
    /// The identifier the hit is about; kept in the set as documentation even
    /// though `ImpactHit` does not return it.
    pub name: &'static str,
    pub call_shaped: bool,
    pub resolution: Resolution,
}

impl ExpectedHit {
    pub fn key(&self) -> HitKey {
        HitKey::new(self.path, self.line, self.call_shaped)
    }
}

/// An edge the fixture deliberately contains but the graded tier cannot see.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct UnresolvedEdge {
    pub path: &'static str,
    pub line: usize,
    /// What the source does, in words a reviewer can check by opening the file.
    pub description: &'static str,
    /// Why no edge can be produced without a stronger backend.
    pub reason: Resolution,
}

/// A hand-written conformance case for one language.
#[derive(Debug, Clone, Copy)]
pub struct ConformanceCase {
    pub language: &'static str,
    /// Directory under `tests/fixtures/symbol-conformance/`.
    pub fixture: &'static str,
    /// Query used for `def`.
    pub definition_query: &'static str,
    /// Query used for `refs` and `callers`.
    pub reference_query: &'static str,
    /// Query used for `callees`.
    pub callee_query: &'static str,
    pub definitions: &'static [ExpectedHit],
    pub references: &'static [ExpectedHit],
    pub callers: &'static [ExpectedHit],
    pub callees: &'static [ExpectedHit],
    /// Dynamic call sites that no lexical extractor can bind.
    pub unresolved: &'static [UnresolvedEdge],
}

// The expected sets below were written from the source lines in the three
// fixture files, not captured from a run. A change to either side is a mismatch
// and fails the gate. Every row is UnresolvedLexical because the CLI graph has
// no type information; every dynamic call is listed under `unresolved`.
static RUST_DEFS: &[ExpectedHit] = &[ExpectedHit {
    path: "src/lib.rs",
    line: 3,
    name: "compute",
    call_shaped: false,
    resolution: Resolution::UnresolvedLexical,
}];
static RUST_REFS: &[ExpectedHit] = &[
    ExpectedHit {
        path: "src/lib.rs",
        line: 8,
        name: "compute",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/lib.rs",
        line: 9,
        name: "compute",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
];
static RUST_CALLERS: &[ExpectedHit] = &[
    ExpectedHit {
        path: "src/lib.rs",
        line: 8,
        name: "compute",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/lib.rs",
        line: 9,
        name: "compute",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
];
static RUST_CALLEES: &[ExpectedHit] = &[
    ExpectedHit {
        path: "src/lib.rs",
        line: 8,
        name: "compute",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/lib.rs",
        line: 9,
        name: "compute",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
];
static RUST_UNRESOLVED: &[UnresolvedEdge] = &[UnresolvedEdge {
    path: "src/lib.rs",
    line: 14,
    description: "dispatch invokes its `callback` parameter; no declaration binds that name",
    reason: Resolution::UnresolvedDynamic,
}];

static PY_DEFS: &[ExpectedHit] = &[ExpectedHit {
    path: "pkg.py",
    line: 10,
    name: "helper",
    call_shaped: false,
    resolution: Resolution::UnresolvedLexical,
}];
static PY_REFS: &[ExpectedHit] = &[
    ExpectedHit {
        path: "pkg.py",
        line: 6,
        name: "helper",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "pkg.py",
        line: 7,
        name: "helper",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
];
static PY_CALLERS: &[ExpectedHit] = &[
    ExpectedHit {
        path: "pkg.py",
        line: 6,
        name: "helper",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "pkg.py",
        line: 7,
        name: "helper",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
];
static PY_CALLEES: &[ExpectedHit] = &[
    ExpectedHit {
        path: "pkg.py",
        line: 6,
        name: "helper",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "pkg.py",
        line: 7,
        name: "helper",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
];
static PY_UNRESOLVED: &[UnresolvedEdge] = &[UnresolvedEdge {
    path: "pkg.py",
    line: 15,
    description: "dispatch invokes its `callback` parameter; no declaration binds that name",
    reason: Resolution::UnresolvedDynamic,
}];

static TS_DEFS: &[ExpectedHit] = &[ExpectedHit {
    path: "src/service.ts",
    line: 1,
    name: "fetchRecord",
    call_shaped: false,
    resolution: Resolution::UnresolvedLexical,
}];
static TS_REFS: &[ExpectedHit] = &[
    ExpectedHit {
        path: "src/service.ts",
        line: 7,
        name: "fetchRecord",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/service.ts",
        line: 11,
        name: "fetchRecord",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/service.ts",
        line: 12,
        name: "fetchRecord",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
];
static TS_CALLERS: &[ExpectedHit] = &[
    ExpectedHit {
        path: "src/service.ts",
        line: 7,
        name: "fetchRecord",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/service.ts",
        line: 11,
        name: "fetchRecord",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/service.ts",
        line: 12,
        name: "fetchRecord",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
];
static TS_CALLEES: &[ExpectedHit] = &[
    ExpectedHit {
        path: "src/service.ts",
        line: 7,
        name: "fetchRecord",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/service.ts",
        line: 11,
        name: "fetchRecord",
        call_shaped: false,
        resolution: Resolution::UnresolvedLexical,
    },
    ExpectedHit {
        path: "src/service.ts",
        line: 12,
        name: "fetchRecord",
        call_shaped: true,
        resolution: Resolution::UnresolvedLexical,
    },
];
static TS_UNRESOLVED: &[UnresolvedEdge] = &[UnresolvedEdge {
    path: "src/service.ts",
    line: 17,
    description: "dispatch invokes its `callback` parameter; no declaration binds that name",
    reason: Resolution::UnresolvedDynamic,
}];

/// The three hand-written conformance cases.
static CASES: &[ConformanceCase] = &[
    ConformanceCase {
        language: "rust",
        fixture: "rust",
        definition_query: "compute",
        reference_query: "compute",
        callee_query: "run",
        definitions: RUST_DEFS,
        references: RUST_REFS,
        callers: RUST_CALLERS,
        callees: RUST_CALLEES,
        unresolved: RUST_UNRESOLVED,
    },
    ConformanceCase {
        language: "python",
        fixture: "python",
        definition_query: "helper",
        reference_query: "helper",
        callee_query: "save",
        definitions: PY_DEFS,
        references: PY_REFS,
        callers: PY_CALLERS,
        callees: PY_CALLEES,
        unresolved: PY_UNRESOLVED,
    },
    ConformanceCase {
        language: "typescript",
        fixture: "typescript",
        definition_query: "fetchRecord",
        reference_query: "fetchRecord",
        callee_query: "load",
        definitions: TS_DEFS,
        references: TS_REFS,
        callers: TS_CALLERS,
        callees: TS_CALLEES,
        unresolved: TS_UNRESOLVED,
    },
];

/// The three hand-written conformance cases, one per graded language.
pub fn cases() -> &'static [ConformanceCase] {
    CASES
}

/// The read-only surface under test. The CLI test implements this with the four
/// public `memnoth::kg` question verbs plus the precision stamp on `impact`.
pub trait SymbolSubject {
    fn definitions(&self, symbol: &str) -> Result<Vec<HitKey>, String>;
    fn references(&self, symbol: &str) -> Result<Vec<HitKey>, String>;
    fn callers(&self, symbol: &str) -> Result<Vec<HitKey>, String>;
    fn callees(&self, symbol: &str) -> Result<Vec<HitKey>, String>;
    /// The tier the answer itself claims, read from the answer, not guessed.
    fn precision(&self, symbol: &str) -> Precision;
}

/// Precision/recall for one question category of one language.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryGrade {
    pub category: &'static str,
    pub expected: usize,
    pub observed: usize,
    pub matched: usize,
    pub precision: f64,
    pub recall: f64,
    pub missing: Vec<HitKey>,
    pub unexpected: Vec<HitKey>,
}

impl CategoryGrade {
    fn new(category: &'static str, expected: &[ExpectedHit], observed: Vec<HitKey>) -> Self {
        let want: BTreeSet<HitKey> = expected.iter().map(ExpectedHit::key).collect();
        let got: BTreeSet<HitKey> = observed.into_iter().collect();
        let matched = want.intersection(&got).count();
        let missing: Vec<HitKey> = want.difference(&got).cloned().collect();
        let unexpected: Vec<HitKey> = got.difference(&want).cloned().collect();
        let ratio = |part: usize, whole: usize| {
            if whole == 0 {
                if part == 0 { 1.0 } else { 0.0 }
            } else {
                part as f64 / whole as f64
            }
        };
        Self {
            category,
            expected: want.len(),
            observed: got.len(),
            matched,
            precision: ratio(matched, got.len()),
            recall: ratio(matched, want.len()),
            missing,
            unexpected,
        }
    }

    /// Whether observed and expected sets are identical.
    pub fn exact(&self) -> bool {
        self.missing.is_empty() && self.unexpected.is_empty()
    }
}

/// The graded result for one language.
#[derive(Debug, Clone, Serialize)]
pub struct LanguageReport {
    pub language: &'static str,
    /// The tier stamped on the answer under test.
    pub precision: Precision,
    pub oracle: Oracle,
    pub categories: Vec<CategoryGrade>,
    /// Explicitly labelled edges the graded tier cannot produce.
    pub unresolved: Vec<UnresolvedEdge>,
    /// Expected hits that claim type-resolved identity. Must be zero without an
    /// oracle; carried as a count so the gate can assert it.
    pub identity_resolved: usize,
}

impl LanguageReport {
    pub fn exact(&self) -> bool {
        self.categories.iter().all(CategoryGrade::exact)
    }

    /// A one-line per-language report, safe to print from a test or a CLI verb.
    pub fn summary(&self) -> String {
        let parts: Vec<String> = self
            .categories
            .iter()
            .map(|grade| {
                format!(
                    "{} {:.3}/{:.3} ({}/{})",
                    grade.category, grade.precision, grade.recall, grade.matched, grade.expected
                )
            })
            .collect();
        format!(
            "{}: {} | tier={} oracle={} unresolved_labelled={} identity_resolved={}",
            self.language,
            parts.join(", "),
            self.precision.label(),
            match self.oracle {
                Oracle::Absent => "absent",
                Oracle::GroundTruthDump(name) => name,
            },
            self.unresolved.len(),
            self.identity_resolved
        )
    }
}

/// All languages graded in one run.
#[derive(Debug, Clone, Serialize)]
pub struct ConformanceReport {
    pub languages: Vec<LanguageReport>,
}

impl ConformanceReport {
    pub fn exact(&self) -> bool {
        self.languages.iter().all(LanguageReport::exact)
    }

    pub fn summary(&self) -> String {
        self.languages
            .iter()
            .map(LanguageReport::summary)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Grade one case by asking the subject the four public questions.
///
/// Indexing is the caller's job; this function only reads. No oracle is invented
/// here: the caller states which one exists, and the honest tier check below
/// refuses to let a stamp outrun it.
pub fn grade(
    subject: &dyn SymbolSubject,
    case: &ConformanceCase,
    oracle: Oracle,
) -> LanguageReport {
    let observed = vec![
        CategoryGrade::new(
            "definitions",
            case.definitions,
            read(subject.definitions(case.definition_query)),
        ),
        CategoryGrade::new(
            "references",
            case.references,
            read(subject.references(case.reference_query)),
        ),
        CategoryGrade::new(
            "callers",
            case.callers,
            read(subject.callers(case.reference_query)),
        ),
        CategoryGrade::new(
            "callees",
            case.callees,
            read(subject.callees(case.callee_query)),
        ),
    ];
    let identity_resolved = case
        .definitions
        .iter()
        .chain(case.references)
        .chain(case.callers)
        .chain(case.callees)
        .filter(|hit| hit.resolution.is_identity_resolved())
        .count();
    LanguageReport {
        language: case.language,
        precision: subject.precision(case.definition_query),
        oracle,
        categories: observed,
        unresolved: case.unresolved.to_vec(),
        identity_resolved,
    }
}

fn read(result: Result<Vec<HitKey>, String>) -> Vec<HitKey> {
    result.unwrap_or_else(|error| panic!("the public extractor API failed: {error}"))
}

/// Fail the gate on any observed/expected mismatch, with the exact diff.
pub fn assert_exact(report: &ConformanceReport) {
    let mut problems = Vec::new();
    for language in &report.languages {
        for grade in &language.categories {
            if grade.exact() {
                continue;
            }
            let missing: Vec<String> = grade.missing.iter().map(ToString::to_string).collect();
            let unexpected: Vec<String> =
                grade.unexpected.iter().map(ToString::to_string).collect();
            problems.push(format!(
                "{} {} precision={:.3} recall={:.3} missing=[{}] unexpected=[{}]",
                language.language,
                grade.category,
                grade.precision,
                grade.recall,
                missing.join(", "),
                unexpected.join(", ")
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "conformance mismatch:\n{}\n{}",
        problems.join("\n"),
        report.summary()
    );
}

/// A stamp can never outrun the evidence behind it.
///
/// The fixture set has no type-resolved oracle, so the only honest tier here is
/// `lexical`: the scanner matches names and call shapes. Even if a later build
/// compiles tree-sitter or an LSP client in, `best_available()` is a build
/// capability, not a property of these edges, and must not be used as a label.
pub fn assert_honest_tier(report: &ConformanceReport) {
    for language in &report.languages {
        if !language.oracle.allows_lsp() {
            assert_ne!(
                language.precision,
                Precision::Lsp,
                "{} claims lsp without a ground-truth oracle",
                language.language
            );
            assert_eq!(
                language.precision,
                Precision::Lexical,
                "{} claims a tier the lexical fixture did not exercise",
                language.language
            );
            assert_eq!(
                language.identity_resolved, 0,
                "{} claims type-resolved identity without an oracle",
                language.language
            );
        }
        assert!(
            language.precision <= Precision::TreeSitter,
            "{} claims a tier no backend this fixture exercises can produce",
            language.language
        );
    }
}

/// Each language must exercise at least one explicitly labelled unresolved edge,
/// and that label must never be upgraded to identity resolution.
pub fn assert_dynamic_gaps_labelled(report: &ConformanceReport) {
    for language in &report.languages {
        assert!(
            !language.unresolved.is_empty(),
            "{} exercises no unresolved/dynamic edge",
            language.language
        );
        for edge in &language.unresolved {
            assert_eq!(
                edge.reason,
                Resolution::UnresolvedDynamic,
                "{}: {} is not labelled as a dynamic gap",
                language.language,
                edge.description
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-written set must say what each row is about and how it is bound.
    #[test]
    fn every_expected_hit_carries_a_name_and_a_resolution() {
        for case in cases() {
            for hit in case
                .definitions
                .iter()
                .chain(case.references)
                .chain(case.callers)
                .chain(case.callees)
            {
                assert!(!hit.name.is_empty(), "{} has an unnamed hit", case.language);
                assert_eq!(
                    hit.resolution,
                    Resolution::UnresolvedLexical,
                    "{} cannot claim identity without an oracle",
                    case.language
                );
            }
            assert!(
                !case.unresolved.is_empty(),
                "{} has no labelled dynamic gap",
                case.language
            );
        }
    }

    /// The positive side of each honesty predicate, so a future inversion of
    /// either predicate is caught rather than silently making the gate vacuous.
    #[test]
    fn resolution_and_oracle_predicates_are_exercised() {
        assert!(Resolution::ResolvedByIdentity.is_identity_resolved());
        assert!(!Resolution::UnresolvedLexical.is_identity_resolved());
        assert!(!Resolution::UnresolvedDynamic.is_identity_resolved());
        assert!(Oracle::GroundTruthDump("oracle/v1.json").allows_lsp());
        assert!(!Oracle::Absent.allows_lsp());
    }

    #[test]
    fn exact_is_true_only_when_the_sets_match() {
        let empty = ConformanceReport {
            languages: vec![LanguageReport {
                language: "rust",
                precision: Precision::Lexical,
                oracle: Oracle::Absent,
                categories: vec![CategoryGrade::new("definitions", &[], Vec::new())],
                unresolved: Vec::new(),
                identity_resolved: 0,
            }],
        };
        assert!(empty.exact());
        assert!(empty.languages[0].exact());
        assert!(empty.languages[0].categories[0].exact());

        let mut wrong = empty.clone();
        wrong.languages[0].categories[0]
            .unexpected
            .push(HitKey::new("src/lib.rs", 42, false));
        assert!(!wrong.exact());
        assert!(!wrong.languages[0].exact());
        assert!(!wrong.languages[0].categories[0].exact());
    }
}

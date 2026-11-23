//! ALG08 conformance gate — grade the public symbol API against hand-written sets.
//!
//! The module under test is `src/symbol_conformance.rs`, included by path so
//! this gate runs without editing `src/lib.rs`. It is otherwise a normal library
//! module; the exact wiring diff is in the handoff report.
//!
//! The extractor is reached only through `memnoth::kg`'s public question verbs
//! (`def`, `refs`, `callers`, `callees`) after `kg::index` has populated a graph
//! in a throwaway copy of each fixture. No private module is imported, no file
//! in `src/kg/` or `crates/memnoth-codegraph/` is changed, and no model, network
//! or language server is involved.

#[path = "../src/symbol_conformance.rs"]
mod symbol_conformance;

use std::path::{Path, PathBuf};

use memnoth::kg;
use memnoth::{AppContext, Config};
use memnoth_codegraph::precision::Precision;
use symbol_conformance::{
    ConformanceCase, ConformanceReport, HitKey, LanguageReport, Oracle, Resolution, SymbolSubject,
    UnresolvedEdge, assert_dynamic_gaps_labelled, assert_exact, assert_honest_tier, cases, grade,
};

/// Adapt the four public `kg` question verbs to the read-only grading trait.
struct KgSubject<'a> {
    ctx: &'a AppContext,
}

impl KgSubject<'_> {
    fn hits(&self, report_hits: Vec<kg::ImpactHit>) -> Vec<HitKey> {
        report_hits
            .into_iter()
            .map(|hit| HitKey {
                path: hit.path,
                line: hit.line,
                call_shaped: hit.call_shaped,
            })
            .collect()
    }
}

impl SymbolSubject for KgSubject<'_> {
    fn definitions(&self, symbol: &str) -> Result<Vec<HitKey>, String> {
        let report = kg::def(self.ctx, symbol).map_err(|error| error.to_string())?;
        Ok(report
            .definition
            .into_iter()
            .map(|definition| HitKey {
                path: definition.path,
                line: definition.line,
                call_shaped: false,
            })
            .collect())
    }

    fn references(&self, symbol: &str) -> Result<Vec<HitKey>, String> {
        let report = kg::refs(self.ctx, symbol, 500).map_err(|error| error.to_string())?;
        Ok(self.hits(report.references))
    }

    fn callers(&self, symbol: &str) -> Result<Vec<HitKey>, String> {
        let report = kg::callers(self.ctx, symbol, 500).map_err(|error| error.to_string())?;
        Ok(self.hits(report.callers))
    }

    fn callees(&self, symbol: &str) -> Result<Vec<HitKey>, String> {
        let report = kg::callees(self.ctx, symbol, 500).map_err(|error| error.to_string())?;
        Ok(self.hits(report.callees))
    }

    fn precision(&self, symbol: &str) -> Precision {
        // Read the stamp from the answer itself. `best_available()` is a build
        // capability and is deliberately never used as the tier of a lexical
        // answer, even when tree-sitter is compiled into the workspace.
        kg::impact(self.ctx, symbol, 1)
            .map(|answer| answer.precision)
            .unwrap_or(Precision::Lexical)
    }
}

/// A context pointing at a throwaway fixture copy, as `memnoth kg` resolves it.
fn context_at(root: &Path) -> AppContext {
    AppContext {
        root: root.to_path_buf(),
        home: root.to_path_buf(),
        vault_path: root.join("vault.db"),
        namespace: "default".to_string(),
        graph_path: root.join(".memnoth").join("graph.db"),
        sessions_dir: root.join("sessions"),
        config_path: root.join("config.toml"),
        config: Config::default(),
        json: false,
        quiet: true,
        progress_tx: None,
        exe_path: Path::new("mn").to_path_buf(),
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in std::fs::read_dir(source).expect("fixture directory is readable") {
        let entry = entry.expect("fixture entry is readable");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("entry type").is_dir() {
            std::fs::create_dir_all(&target).expect("create fixture subdirectory");
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// Index a private copy of one fixture so the graded graph never lands in the
/// committed fixture directory.
fn indexed_case(case: &ConformanceCase) -> (tempfile::TempDir, AppContext) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("symbol-conformance")
        .join(case.fixture);
    assert!(
        fixture_root.is_dir(),
        "missing fixture for {}: {}",
        case.language,
        fixture_root.display()
    );

    let dir = tempfile::tempdir().expect("temporary fixture workspace");
    copy_tree(&fixture_root, dir.path());
    let ctx = context_at(dir.path());
    let report = kg::index(&ctx, false, true, &mut |_| {}).expect("index the fixture");
    assert!(
        report.symbols > 0 && report.references > 0,
        "{} must produce real edges, otherwise the grade proves nothing: {report:?}",
        case.language
    );
    (dir, ctx)
}

/// Grade every language through the public API, with no oracle.
fn graded_report() -> (Vec<tempfile::TempDir>, ConformanceReport) {
    let mut guards = Vec::new();
    let mut languages = Vec::new();
    for case in cases() {
        let (guard, ctx) = indexed_case(case);
        let subject = KgSubject { ctx: &ctx };
        languages.push(grade(&subject, case, Oracle::Absent));
        guards.push(guard);
    }
    (guards, ConformanceReport { languages })
}

#[test]
fn three_languages_are_graded_against_hand_written_sets() {
    let (_guards, report) = graded_report();

    // The report is printed so `cargo test --test symbol_conformance -- --nocapture`
    // shows precision/recall per language, per question category.
    println!("{}", report.summary());

    assert_eq!(
        report.languages.len(),
        3,
        "rust, python and typescript are graded"
    );
    let graded: Vec<&str> = report.languages.iter().map(|item| item.language).collect();
    assert_eq!(graded, vec!["rust", "python", "typescript"]);

    for language in &report.languages {
        assert_eq!(
            language.categories.len(),
            4,
            "{} graded four questions",
            language.language
        );
        for grade in &language.categories {
            assert!(
                grade.expected > 0 && grade.observed > 0,
                "{} {} must exercise real rows",
                language.language,
                grade.category
            );
        }
    }

    assert_exact(&report);
    assert_honest_tier(&report);
    assert_dynamic_gaps_labelled(&report);
}

#[test]
fn precision_and_recall_are_reported_per_language() {
    let (_guards, report) = graded_report();
    let summary = report.summary();
    for language in ["rust", "python", "typescript"] {
        assert!(summary.contains(language), "{language} missing from report");
    }
    for category in ["definitions", "references", "callers", "callees"] {
        assert!(summary.contains(category), "{category} missing from report");
    }
    assert!(summary.contains("tier=lexical"), "tier must be stated");
    assert!(
        summary.contains("oracle=absent"),
        "oracle state must be stated"
    );
}

/// Negative control: a set that does not match what the API returned must fail
/// the gate, not lower a threshold until it passes.
#[test]
#[should_panic(expected = "conformance mismatch")]
fn a_mismatch_fails_the_gate() {
    let case = &cases()[0];
    let (_guard, ctx) = indexed_case(case);
    let subject = KgSubject { ctx: &ctx };
    let mut report = ConformanceReport {
        languages: vec![grade(&subject, case, Oracle::Absent)],
    };
    report.languages[0].categories[0]
        .unexpected
        .push(HitKey::new("src/lib.rs", 9999, false));
    assert_exact(&report);
}

/// Negative control: without a ground-truth dump, an `lsp` stamp is refused even
/// if a future build happens to compile a language-server client in.
#[test]
#[should_panic(expected = "claims lsp without a ground-truth oracle")]
fn an_lsp_stamp_without_an_oracle_is_refused() {
    let report = ConformanceReport {
        languages: vec![LanguageReport {
            language: "rust",
            precision: Precision::Lsp,
            oracle: Oracle::Absent,
            categories: Vec::new(),
            unresolved: vec![UnresolvedEdge {
                path: "src/lib.rs",
                line: 14,
                description: "synthetic negative control",
                reason: Resolution::UnresolvedDynamic,
            }],
            identity_resolved: 0,
        }],
    };
    assert_honest_tier(&report);
}

/// A conformance set with no labelled dynamic edge is not a conformance set.
#[test]
#[should_panic(expected = "exercises no unresolved/dynamic edge")]
fn a_case_without_a_labelled_unresolved_edge_is_refused() {
    let report = ConformanceReport {
        languages: vec![LanguageReport {
            language: "rust",
            precision: Precision::Lexical,
            oracle: Oracle::Absent,
            categories: Vec::new(),
            unresolved: Vec::new(),
            identity_resolved: 0,
        }],
    };
    assert_dynamic_gaps_labelled(&report);
}

/// Guard on the fixture layout: every case names a distinct language and a real
/// directory. Keeps a copy/paste edit from grading one language three times.
#[test]
fn fixture_paths_are_distinct_and_present() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/symbol-conformance");
    let mut languages = std::collections::BTreeSet::new();
    let mut fixtures = std::collections::BTreeSet::new();
    for case in cases() {
        assert!(
            languages.insert(case.language),
            "duplicate language {}",
            case.language
        );
        assert!(
            fixtures.insert(case.fixture),
            "duplicate fixture {}",
            case.fixture
        );
        assert!(
            root.join(case.fixture).is_dir(),
            "missing fixture directory {}",
            case.fixture
        );
    }
}

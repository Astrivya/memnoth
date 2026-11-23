//! KG precision is a fact about the edges, not about the build.
//!
//! The CLI graph is produced by the regex/lexical scanner in `src/kg/extract.rs`
//! and read back from SQLite by the verbs in `src/kg/mod.rs`. None of those
//! verbs call the tree-sitter parser compiled into `memnoth-codegraph`, and no
//! language server is involved. So even though `best_available()` reports a
//! stronger tier, every answer these verbs return must carry `lexical` on the
//! wire — and must never claim `lsp` while the probe says LSP is absent.
//!
//! This file exercises the real public verbs against a real indexed workspace,
//! so a future change that re-stamps an answer from `best_available()` fails a
//! test instead of quietly overstating the evidence.

use std::path::Path;

use memnoth::kg;
use memnoth::{AppContext, Config};
use memnoth_codegraph::precision::{Precision, best_available, capabilities};
use serde_json::json;

/// A context with the graph beside the fixture, like `memnoth kg` resolves it.
fn test_context(root: &Path) -> AppContext {
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
        exe_path: Path::new("mn").to_path_buf(),
    }
}

/// A workspace whose declarations and mentions all come from the line scanner.
///
/// The index must actually find the symbol, otherwise an "answer" with no edges
/// would prove nothing about the label on a lexical-sourced answer.
fn indexed_fixture() -> (tempfile::TempDir, AppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn truth_probe(value: u32) -> u32 {\n    value + 1\n}\n\npub fn caller() -> u32 {\n    truth_probe(1)\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("tests/it.rs"),
        "fn it_covers_truth_probe() {\n    assert_eq!(crate::truth_probe(1), 2);\n}\n",
    )
    .unwrap();

    let ctx = test_context(dir.path());
    let report = kg::index(&ctx, false, true, &mut |_| {}).unwrap();
    assert!(report.symbols >= 3, "the fixture must produce real edges");
    assert!(
        report.references >= 1,
        "the fixture must produce real edges"
    );
    (dir, ctx)
}

/// Every precision-bearing answer produced by `kg`, resolved and unresolved.
///
/// Kept in one place so a new verb that carries a `precision` field has to be
/// added here too (or will be noticed when this list is reviewed).
fn answer_stamps(ctx: &AppContext) -> Vec<(&'static str, Precision)> {
    let resolved = kg::impact(ctx, "truth_probe", 1).unwrap();
    assert!(resolved.resolved, "impact must resolve the fixture symbol");
    assert!(
        !resolved.definitions.is_empty() && !resolved.references.is_empty(),
        "the stamp is only meaningful on an answer that actually has edges"
    );

    let tests = kg::tests_for(ctx, "truth_probe").unwrap();
    assert!(tests.resolved, "tests-for must resolve the fixture symbol");

    let miss = kg::impact(ctx, "no_such_symbol_in_this_workspace", 1).unwrap();
    assert!(!miss.resolved, "the unresolved branch must be exercised");
    let test_miss = kg::tests_for(ctx, "no_such_symbol_in_this_workspace").unwrap();
    assert!(
        !test_miss.resolved,
        "the unresolved branch must be exercised"
    );

    vec![
        ("impact/resolved", resolved.precision),
        ("tests-for/resolved", tests.precision),
        ("impact/unresolved", miss.precision),
        ("tests-for/unresolved", test_miss.precision),
    ]
}

#[test]
fn an_lsp_absent_build_never_stamps_lsp_on_a_kg_answer() {
    // Probe first: the first assertion is stated for builds where LSP is absent.
    // The second holds for every build, because these verbs never ask a language
    // server for an answer.
    let lsp = capabilities().lsp;
    let (_dir, ctx) = indexed_fixture();

    for (name, stamp) in answer_stamps(&ctx) {
        if !lsp {
            assert_ne!(
                stamp,
                Precision::Lsp,
                "{name} claims lsp while capabilities().lsp is false"
            );
        }
        assert!(
            stamp <= Precision::TreeSitter,
            "{name} claims a tier no backend these verbs use can produce"
        );
    }
}

#[test]
fn a_lexical_sourced_answer_is_labelled_lexical() {
    let (_dir, ctx) = indexed_fixture();

    // `best_available()` is the tier the probe says this build could reach —
    // tree-sitter here, possibly lsp in a later build. It is deliberately NOT
    // what a scanner-produced KG answer may claim.
    let probe_best = best_available();

    for (name, stamp) in answer_stamps(&ctx) {
        assert_eq!(stamp, Precision::Lexical, "{name} is scanner-produced");
        assert_eq!(stamp.label(), "lexical", "{name} wire label");
        assert_eq!(
            serde_json::to_value(stamp).unwrap(),
            json!("lexical"),
            "{name} JSON label"
        );
        if probe_best != Precision::Lexical {
            assert_ne!(
                stamp, probe_best,
                "{name} stamped the probe's best tier instead of the edge source"
            );
        }
    }

    // The label travels with the answer object, not just the enum.
    let impact = kg::impact(&ctx, "truth_probe", 1).unwrap();
    assert_eq!(
        serde_json::to_value(&impact).unwrap()["precision"],
        json!("lexical")
    );
    let tests = kg::tests_for(&ctx, "truth_probe").unwrap();
    assert_eq!(
        serde_json::to_value(&tests).unwrap()["precision"],
        json!("lexical")
    );
}

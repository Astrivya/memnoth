//! Plain-text rendering for the graph verbs.
//!
//! No colour and no ANSI: this is the same string the TUI prints into its
//! transcript, the same string a `| grep` sees, and the same string `--json`
//! replaces with the object beside it. A renderer that needs a terminal would
//! make three surfaces disagree.
//!
//! Numbers are abbreviated (`12.4k`) where the eye is scanning a column, and
//! written out where the exact value is the point.

use super::{
    CalleesReport, CallersReport, CallpathReport, CyclesReport, DeadCodeReport, DefReport,
    DepsReport, DiffImpactReport, ExploreReport, HotspotsReport, ImpactReport, IndexReport,
    IndexStatus, MAX_REFS, OutlineReport, OutlineSymbol, OwnersReport, QueryReport, RefsReport,
    ReviewReport, SymbolHit, TestsForReport, TreeReport, UnusedExportsReport,
};
use crate::output::{ellipsize, human_age, human_count, rfc3339};

/// The report `kg index` prints.
pub fn render_index(report: &IndexReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("code graph · {}\n", report.root));
    out.push_str(&format!("  files       {}\n", human_count(report.files)));
    out.push_str(&format!("  symbols     {}\n", human_count(report.symbols)));
    out.push_str(&format!(
        "  references  {}\n",
        human_count(report.references)
    ));
    out.push_str(&format!(
        "  elapsed     {} ms · {}\n",
        report.elapsed_ms,
        if report.full {
            "rebuilt from scratch".to_string()
        } else {
            format!("incremental · built {}", rfc3339(report.built_at_ms))
        }
    ));

    if !report.languages.is_empty() {
        out.push_str("\nlanguages\n");
        for language in &report.languages {
            out.push_str(&format!(
                "  {:<12} {:>6} file(s)  {:>7} symbol(s)\n",
                language.language,
                human_count(language.files),
                human_count(language.symbols)
            ));
        }
    }

    if report.references >= MAX_REFS {
        out.push_str(&format!(
            "\nreference index capped at {} mentions; impact beyond that is truncated.\n",
            human_count(MAX_REFS)
        ));
    }
    out.push_str(
        "\nsearch is lexical: this build has no embedding step, so `kg search` matches names, not meaning.\n",
    );
    out
}

/// The report `kg status` prints.
pub fn render_status(status: &IndexStatus) -> String {
    let mut out = format!("code graph · {}\n", status.path);
    if !status.indexed {
        out.push_str("  state       not indexed — run `memnoth kg index`\n");
        return out;
    }
    out.push_str(&format!(
        "  state       indexed {} ({})\n",
        human_age(status.built_at_ms, memnoth_engine::now_ms()),
        rfc3339(status.built_at_ms)
    ));
    out.push_str(&format!("  root        {}\n", status.root));
    out.push_str(&format!("  files       {}\n", human_count(status.files)));
    out.push_str(&format!("  symbols     {}\n", human_count(status.symbols)));
    out.push_str(&format!(
        "  references  {}\n",
        human_count(status.references)
    ));
    if status.stale_files == 0 {
        out.push_str("  stale       0 file(s) — the graph matches the working tree\n");
    } else {
        out.push_str(&format!(
            "  stale       {} file(s) changed since the index — run `memnoth kg index`\n",
            human_count(status.stale_files)
        ));
    }

    if !status.languages.is_empty() {
        out.push_str("\nlanguages\n");
        for language in &status.languages {
            out.push_str(&format!(
                "  {:<12} {:>6} file(s)  {:>7} symbol(s)\n",
                language.language,
                human_count(language.files),
                human_count(language.symbols)
            ));
        }
    }
    out
}

/// The report `kg impact` prints.
pub fn render_impact(report: &ImpactReport) -> String {
    let mut out = format!("impact · {}\n", report.symbol);
    if !report.resolved {
        out.push_str(&format!(
            "  no symbol named {:?} in the graph\n",
            report.symbol
        ));
        // A miss is a definite answer, so say so in the machine shape too: a
        // script grepping the text output must not have to infer resolution
        // from prose it cannot parse.
        out.push_str("  {\"resolved\": false, \"definitions\": []}\n");
        out.push_str(
            "  check the name, or run `memnoth kg index` if the workspace changed since it was built\n",
        );
        return out;
    }

    // The split is the difference between "this is used in two places" and
    // "a second hop through unrelated declarations found 380 rows". One number
    // for both read as a blast radius nobody could act on.
    out.push_str(&format!(
        "  definitions  {} · references  {} in {} file(s)\n",
        report.definitions.len(),
        report.references_direct,
        report.files
    ));
    if report.references_transitive > 0 {
        out.push_str(&format!(
            "  plus {} row(s) at depth 2 (symbols declared in the files above, not this name)\n",
            report.references_transitive
        ));
    }

    if report.definitions.is_empty() {
        out.push_str("\n  (no declaration row: only mentions were found)\n");
    } else {
        out.push_str("\ndefinitions\n");
        for hit in &report.definitions {
            out.push_str(&format!(
                "  {}:{}  {}\n",
                hit.path,
                hit.line,
                ellipsize(&hit.context, 100)
            ));
        }
    }

    if !report.references.is_empty() {
        out.push_str("\nreferences\n");
        for hit in &report.references {
            out.push_str(&format!(
                "  {}:{}  {}\n",
                hit.path,
                hit.line,
                ellipsize(&hit.context, 100)
            ));
        }
    }

    if !report.tests.is_empty() {
        out.push_str("\ntests\n");
        for path in &report.tests {
            out.push_str(&format!("  {path}\n"));
        }
    }

    if report.truncated {
        out.push_str("\nreference list truncated; narrow the name or the change set.\n");
    }
    out.push_str(
        "\nreferences are textual: a shadowed local with the same name is counted. Treat this as blast-radius evidence, not a resolved call graph.\n",
    );
    out
}

/// The report `kg search` prints.
pub fn render_search(hits: &[SymbolHit]) -> String {
    if hits.is_empty() {
        return "no symbol name matches; `kg search` matches names only — a lexical text search is `grep`.\n"
            .to_string();
    }
    let mut out = format!("symbols ({})\n", hits.len());
    for hit in hits {
        let container = hit
            .container
            .as_deref()
            .map(|name| format!("  in {name}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {}:{}  {:<36} [{}]{}\n",
            hit.path,
            hit.line,
            ellipsize(&hit.name, 36),
            hit.kind,
            container
        ));
    }
    out
}

/// The report `kg explore` prints.
pub fn render_explore(report: &ExploreReport) -> String {
    if !report.resolved {
        return format!(
            "no symbol named {:?} in the graph\n  check the name, or run `memnoth kg index` if the workspace changed since it was built\n",
            report.symbol
        );
    }

    let mut out = format!("explore · {}\n", report.symbol);

    if let Some(def) = &report.definition {
        out.push_str("\ndefinition\n");
        out.push_str(&format!("  {}:{}  [{}]\n", def.path, def.line, def.kind));
        if let Some(container) = &def.container {
            out.push_str(&format!("  in {container}\n"));
        }
        out.push_str("  snippet:\n");
        for (i, line) in def.snippet.iter().enumerate() {
            let line_num = def.line.saturating_sub(3) + i + 1;
            let marker = if line_num == def.line { "→" } else { " " };
            out.push_str(&format!(
                "  {}{} {:>4} | {}\n",
                marker,
                if marker == "→" { "" } else { " " },
                line_num,
                line
            ));
        }
    } else {
        out.push_str("\n(no declaration found in the graph)\n");
    }

    if !report.references.is_empty() {
        out.push_str(&format!("\nreferences ({})\n", report.references.len()));
        for hit in &report.references {
            out.push_str(&format!(
                "  {}:{}  {}\n",
                hit.path,
                hit.line,
                ellipsize(&hit.context, 100)
            ));
        }
    }

    if !report.callers.is_empty() {
        out.push_str(&format!("\ncallers ({})\n", report.callers.len()));
        for hit in &report.callers {
            out.push_str(&format!(
                "  {}:{}  {}\n",
                hit.path,
                hit.line,
                ellipsize(&hit.context, 100)
            ));
        }
    }

    if !report.callees.is_empty() {
        out.push_str(&format!("\ncallees ({})\n", report.callees.len()));
        for hit in &report.callees {
            out.push_str(&format!(
                "  {}:{}  {}\n",
                hit.path,
                hit.line,
                ellipsize(&hit.context, 100)
            ));
        }
    }

    if report.truncated {
        out.push_str("\nlists truncated; narrow the name or the change set.\n");
    }

    out.push_str(
        "\nreferences are textual: a shadowed local with the same name is counted. Treat this as blast-radius evidence, not a resolved call graph.\n",
    );
    out
}

/// The report `kg refs` prints.
pub fn render_refs(report: &RefsReport) -> String {
    if !report.resolved {
        return format!(
            "no symbol named {:?} in the graph\n  check the name, or run `memnoth kg index` if the workspace changed since it was built\n",
            report.symbol
        );
    }

    if report.references.is_empty() {
        return format!("no references to {:?} found\n", report.symbol);
    }

    let mut out = format!("refs · {} ({})\n", report.symbol, report.references.len());
    for hit in &report.references {
        out.push_str(&format!(
            "  {}:{}  {}\n",
            hit.path,
            hit.line,
            ellipsize(&hit.context, 100)
        ));
    }

    if report.truncated {
        out.push_str("\nreference list truncated.\n");
    }
    out
}

/// The report `kg callers` prints.
pub fn render_callers(report: &CallersReport) -> String {
    if !report.resolved {
        return format!(
            "no symbol named {:?} in the graph\n  check the name, or run `memnoth kg index` if the workspace changed since it was built\n",
            report.symbol
        );
    }

    if report.callers.is_empty() {
        return format!("no callers of {:?} found\n", report.symbol);
    }

    let mut out = format!("callers · {} ({})\n", report.symbol, report.callers.len());
    for hit in &report.callers {
        out.push_str(&format!(
            "  {}:{}  {}\n",
            hit.path,
            hit.line,
            ellipsize(&hit.context, 100)
        ));
    }

    if report.truncated {
        out.push_str("\ncaller list truncated.\n");
    }
    out
}

/// The report `kg callees` prints.
pub fn render_callees(report: &CalleesReport) -> String {
    if !report.resolved {
        return format!(
            "no symbol named {:?} in the graph\n  check the name, or run `memnoth kg index` if the workspace changed since it was built\n",
            report.symbol
        );
    }

    if report.callees.is_empty() {
        return format!("no callees of {:?} found\n", report.symbol);
    }

    let mut out = format!("callees · {} ({})\n", report.symbol, report.callees.len());
    for hit in &report.callees {
        out.push_str(&format!(
            "  {}:{}  {}\n",
            hit.path,
            hit.line,
            ellipsize(&hit.context, 100)
        ));
    }

    if report.truncated {
        out.push_str("\ncallee list truncated.\n");
    }
    out
}

/// The report `kg def` prints.
pub fn render_def(report: &DefReport) -> String {
    if !report.resolved {
        return format!(
            "no symbol named {:?} in the graph\n  check the name, or run `memnoth kg index` if the workspace changed since it was built\n",
            report.symbol
        );
    }

    let Some(def) = &report.definition else {
        return format!("no definition for {:?} in the graph\n", report.symbol);
    };

    let mut out = format!("def · {}\n", report.symbol);
    out.push_str(&format!("  {}:{}  [{}]\n", def.path, def.line, def.kind));
    if let Some(container) = &def.container {
        out.push_str(&format!("  in {container}\n"));
    }
    out.push_str("  snippet:\n");
    for (i, line) in def.snippet.iter().enumerate() {
        let line_num = def.line.saturating_sub(3) + i + 1;
        let marker = if line_num == def.line { "→" } else { " " };
        out.push_str(&format!(
            "  {}{} {:>4} | {}\n",
            marker,
            if marker == "→" { "" } else { " " },
            line_num,
            line
        ));
    }
    out
}

/// The list section every renderer ends with: no dump, just the tail count.
fn more(total: usize, shown: usize) -> String {
    if total > shown {
        format!("  ... and {} more\n", total - shown)
    } else {
        String::new()
    }
}

/// The report `kg callpath` prints.
pub fn render_callpath(report: &CallpathReport) -> String {
    if !report.resolved {
        return format!(
            "no call path from {:?} to {:?}\n  one of the names did not resolve; check it, or run `memnoth kg index` if the workspace changed\n",
            report.from, report.to
        );
    }
    if report.path.is_empty() {
        return format!(
            "no call path from {:?} to {:?} (0 hops)\n",
            report.from, report.to
        );
    }

    let mut out = format!(
        "callpath - {} -> {} ({} hop(s))\n",
        report.from, report.to, report.hops
    );
    for (index, node) in report.path.iter().enumerate() {
        let kind = if node.kind.is_empty() {
            "mention"
        } else {
            node.kind.as_str()
        };
        out.push_str(&format!(
            "  {}. {}  [{}]  {}:{}\n",
            index + 1,
            node.symbol,
            kind,
            node.file,
            node.line
        ));
    }
    out
}

/// The report `kg outline` prints.
pub fn render_outline(report: &OutlineReport) -> String {
    let mut out = format!(
        "outline - {} ({})\n",
        report.file,
        if report.language.is_empty() {
            "unknown language"
        } else {
            report.language.as_str()
        }
    );
    if report.symbols.is_empty() {
        out.push_str("  no symbols found in this file\n");
        return out;
    }

    let mut rows = Vec::new();
    flatten_outline(&report.symbols, 0, &mut rows);
    for (symbol, depth) in rows.iter().take(40) {
        let indent = "  ".repeat(*depth * 2);
        out.push_str(&format!(
            "  {}{}  [{}]  {}\n",
            indent, symbol.name, symbol.kind, symbol.line
        ));
    }
    out.push_str(&more(rows.len(), rows.len().min(40)));
    out
}

fn flatten_outline<'a>(
    symbols: &'a [OutlineSymbol],
    depth: usize,
    rows: &mut Vec<(&'a OutlineSymbol, usize)>,
) {
    for symbol in symbols {
        rows.push((symbol, depth));
        flatten_outline(&symbol.children, depth + 1, rows);
    }
}

/// The report `kg tree` prints.
pub fn render_tree(report: &TreeReport) -> String {
    let mut out = format!("tree - {}\n", report.dir);
    if report.entries.is_empty() {
        out.push_str("  no source files with symbols found under this directory\n");
        return out;
    }

    for entry in report.entries.iter().take(20) {
        out.push_str(&format!(
            "\n  {} ({})\n",
            entry.path,
            if entry.language.is_empty() {
                "unknown language"
            } else {
                entry.language.as_str()
            }
        ));
        for symbol in entry.symbols.iter().take(8) {
            let container = symbol
                .container
                .as_deref()
                .map(|name| format!("  in {name}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "    {}  [{}]  {}{}\n",
                symbol.name, symbol.kind, symbol.line, container
            ));
        }
        out.push_str(&more(entry.symbols.len(), entry.symbols.len().min(8)));
    }
    out.push_str(&more(report.entries.len(), report.entries.len().min(20)));
    out
}

/// The report `kg review` prints.
pub fn render_review(report: &ReviewReport) -> String {
    // The range *is* the question: "12 files" under `--base main` is a different
    // claim from 12 unstaged files, so the header names the revision it used.
    let label = match (&report.base, report.staged) {
        (Some(rev), _) => format!("since {rev}"),
        (None, true) => "staged".to_string(),
        (None, false) => "unstaged".to_string(),
    };
    let mut out = format!("review - {label}\n");
    if let Some(rev) = &report.base {
        out.push_str(&format!(
            "  range {rev}...HEAD  what this branch adds since they diverged\n"
        ));
    }
    let delta = report.summary.test_coverage_delta;
    out.push_str(&format!(
        "  files {}  symbols {}  references {}  test coverage {}{}\n",
        report.summary.files_changed,
        report.summary.symbols_changed,
        report.summary.references_affected,
        if delta > 0 { "+" } else { "" },
        delta
    ));

    if report.changes.is_empty() {
        out.push_str("\n  no graph symbols in these changes\n");
        return out;
    }

    for change in report.changes.iter().take(20) {
        out.push_str(&format!("\n  {} {}\n", change.change_type, change.file));
        for symbol in change.symbols.iter().take(10) {
            out.push_str(&format!(
                "    {}  [{}]  line {}  {} reference(s)\n",
                symbol.name, symbol.kind, symbol.line, symbol.references
            ));
        }
        out.push_str(&more(change.symbols.len(), change.symbols.len().min(10)));
        if !change.impacted_tests.is_empty() {
            out.push_str(&format!(
                "    tests: {}\n",
                change.impacted_tests.join(", ")
            ));
        }
    }
    out.push_str(&more(report.changes.len(), report.changes.len().min(20)));
    out
}

/// The report `kg tests-for` prints.
pub fn render_tests_for(report: &TestsForReport) -> String {
    if !report.resolved {
        return format!(
            "no symbol named {:?} in the graph\n  check the name, or run `memnoth kg index` if the workspace changed\n",
            report.symbol
        );
    }
    if report.tests.is_empty() {
        // `total_coverage` counts the test files found, so it is 0 whenever the
        // list is empty; quoting it as "0 searched" said nothing true.
        return format!(
            "no test file references {:?}\n  tests declared inline in a source file are not counted\n",
            report.symbol
        );
    }

    let mut out = format!(
        "tests-for - {} ({} test file(s))\n",
        report.symbol, report.total_coverage
    );
    for test in report.tests.iter().take(20) {
        out.push_str(&format!(
            "  {}:{}  {}  [{}]\n",
            test.path, test.line, test.test_name, test.coverage_type
        ));
    }
    out.push_str(&more(report.tests.len(), report.tests.len().min(20)));
    out
}

/// The report `kg owners` prints.
pub fn render_owners(report: &OwnersReport) -> String {
    let mut out = format!("owners - {}\n", report.path);
    if report.codeowners.is_empty() {
        out.push_str("\n  no CODEOWNERS entry matches this path\n");
    } else {
        out.push_str("\ncodeowners\n");
        for entry in report.codeowners.iter().take(10) {
            out.push_str(&format!(
                "  {:<40} {}\n",
                entry.pattern,
                entry.owners.join(", ")
            ));
        }
        out.push_str(&more(
            report.codeowners.len(),
            report.codeowners.len().min(10),
        ));
    }

    if report.blame.is_empty() {
        out.push_str("\n  no git blame available for this path\n");
        return out;
    }
    out.push_str("\nblame (newest lines first as reported)\n");
    for line in report.blame.iter().take(20) {
        out.push_str(&format!(
            "  {:>5}  {:<16} {:<10} {}\n",
            line.line,
            ellipsize(&line.author, 16),
            ellipsize(&line.commit, 10),
            ellipsize(line.content.trim(), 80)
        ));
    }
    out.push_str(&more(report.blame.len(), report.blame.len().min(20)));
    out
}

/// The report `kg dead-code` prints.
pub fn render_dead_code(report: &DeadCodeReport) -> String {
    if report.exports.is_empty() {
        return "dead-code - no unreferenced exports found\n".to_string();
    }
    let mut out = format!("dead-code - {} unreferenced export(s)\n", report.total);
    for export in report.exports.iter().take(20) {
        out.push_str(&format!(
            "  {}:{}  {}  [{}]  {}\n",
            export.file,
            export.line,
            export.name,
            export.kind,
            if export.is_public {
                "public"
            } else {
                "private"
            }
        ));
    }
    out.push_str(&more(report.exports.len(), report.exports.len().min(20)));
    out
}

/// The report `kg cycles` prints.
pub fn render_cycles(report: &CyclesReport) -> String {
    if report.cycles.is_empty() {
        return "cycles - no import cycles found\n".to_string();
    }
    let mut out = format!("cycles - {} cycle(s)\n", report.total);
    for (index, cycle) in report.cycles.iter().take(10).enumerate() {
        out.push_str(&format!("\n  {}. length {}\n", index + 1, cycle.length));
        if cycle.files.is_empty() {
            out.push_str("     (files not reported)\n");
        } else {
            let mut path = cycle.files.clone();
            if let Some(first) = path.first().cloned() {
                path.push(first);
            }
            out.push_str(&format!("     {}\n", path.join(" -> ")));
        }
    }
    out.push_str(&more(report.cycles.len(), report.cycles.len().min(10)));
    out
}

/// The report `kg hotspots` prints.
pub fn render_hotspots(report: &HotspotsReport) -> String {
    if report.hotspots.is_empty() {
        return "hotspots - no churn or complexity data found\n".to_string();
    }
    let mut out = format!("hotspots - {} file(s)\n", report.total);
    for hotspot in report.hotspots.iter().take(15) {
        out.push_str(&format!(
            "  {:<44} score {:>6.2}  churn {:>4}  complexity {:>4}\n",
            ellipsize(&hotspot.file, 44),
            hotspot.score,
            hotspot.churn,
            hotspot.complexity
        ));
        if !hotspot.symbols.is_empty() {
            out.push_str(&format!(
                "      {}\n",
                ellipsize(&hotspot.symbols.join(", "), 100)
            ));
        }
    }
    out.push_str(&more(report.hotspots.len(), report.hotspots.len().min(15)));
    out
}

/// The report `kg unused-exports` prints.
pub fn render_unused_exports(report: &UnusedExportsReport) -> String {
    if report.exports.is_empty() {
        return "unused-exports - no unused public exports found\n".to_string();
    }
    let mut out = format!("unused-exports - {} export(s)\n", report.total);
    for export in report.exports.iter().take(20) {
        out.push_str(&format!(
            "  {}:{}  {}  [{}]  {}\n",
            export.file, export.line, export.name, export.kind, export.visibility
        ));
    }
    out.push_str(&more(report.exports.len(), report.exports.len().min(20)));
    out
}

/// The report `kg deps` prints.
pub fn render_deps(report: &DepsReport) -> String {
    let mut out = format!("deps - {}\n", report.file);
    if report.imports.is_empty() {
        out.push_str("\n  imports: none\n");
    } else {
        out.push_str(&format!("\nimports ({})\n", report.imports.len()));
        for edge in report.imports.iter().take(20) {
            out.push_str(&format!(
                "  {} -> {}  ({}:{})  [{}]\n",
                edge.from, edge.to, report.file, edge.line, edge.kind
            ));
        }
        out.push_str(&more(report.imports.len(), report.imports.len().min(20)));
    }

    if report.imported_by.is_empty() {
        out.push_str("\n  imported by: none\n");
    } else {
        out.push_str(&format!("\nimported by ({})\n", report.imported_by.len()));
        for edge in report.imported_by.iter().take(20) {
            out.push_str(&format!(
                "  {} -> {}  (line {})  [{}]\n",
                edge.from, edge.to, edge.line, edge.kind
            ));
        }
        out.push_str(&more(
            report.imported_by.len(),
            report.imported_by.len().min(20),
        ));
    }
    out
}

/// The report `kg query` prints.
pub fn render_query(report: &QueryReport) -> String {
    if report.results.is_empty() {
        return format!("query - {} - no matches\n", report.expr);
    }
    let mut out = format!("query - {} ({} match(es))\n", report.expr, report.count);
    for result in report.results.iter().take(20) {
        let metadata = if result.metadata.is_null() {
            String::new()
        } else {
            let text = serde_json::to_string(&result.metadata).unwrap_or_default();
            format!("  {}", ellipsize(&text, 60))
        };
        out.push_str(&format!(
            "  {}:{}  {}  [{}]{}\n",
            result.file, result.line, result.symbol, result.kind, metadata
        ));
    }
    out.push_str(&more(report.results.len(), report.results.len().min(20)));
    out
}

/// The report `kg diff-impact` prints.
pub fn render_diff_impact(report: &DiffImpactReport) -> String {
    let label = if report.staged { "staged" } else { "unstaged" };
    if report.changes.is_empty() {
        return format!("no {} changes with graph symbols\n", label);
    }

    let mut out = format!(
        "diff-impact · {} ({} symbol(s), {} reference(s))\n",
        label,
        report.changes.len(),
        report.total_references
    );

    for change in &report.changes {
        out.push_str(&format!(
            "\n  {} {}:{}  [{}]\n",
            change.change_type, change.path, change.line, change.symbol
        ));
        if !change.references.is_empty() {
            out.push_str(&format!("    {} reference(s):\n", change.references.len()));
            for hit in &change.references {
                out.push_str(&format!(
                    "      {}:{}  {}\n",
                    hit.path,
                    hit.line,
                    ellipsize(&hit.context, 80)
                ));
            }
        }
    }

    out
}

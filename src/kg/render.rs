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
    CalleesReport, CallersReport, DefReport, DiffImpactReport, ExploreReport, ImpactReport,
    IndexReport, IndexStatus, MAX_REFS, RefsReport, SymbolHit,
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
        out.push_str(
            "  check the name, or run `memnoth kg index` if the workspace changed since it was built\n",
        );
        return out;
    }

    out.push_str(&format!(
        "  definitions  {} · references  {} in {} file(s)\n",
        report.definitions.len(),
        report.references.len(),
        report.files
    ));

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

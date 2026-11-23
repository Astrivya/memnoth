//! Release-note honesty gate, mirrored in Rust so `cargo test` enforces it too.
//!
//! These tests parallel `scripts/check-release-notes.py` for the three
//! release-specific drift cases the gate owns:
//!   (a) an open `docs/operations/known-issues` entry must not be claimed
//!       closed in the shipping version's CHANGELOG block;
//!   (b) the forbidden claim phrases (plans/positioning-2026-11.md §4) must not
//!       appear in that block, in either the CLI changelog or the web copy;
//!   (c) the version surfaces (root package, workspace package, crate
//!       manifests, newest changelog heading) must all state one version.
//!
//! Web and workspace-doc paths are optional: a CLI-only checkout skips the
//! checks whose inputs it does not have instead of failing them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_root() -> PathBuf {
    repo_root()
        .parent()
        .expect("CLI has a workspace parent")
        .to_path_buf()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The `version` inside one table, and only that table: a dependency pin is
/// not this crate's version.
fn table_version(text: &str, table: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == table;
            continue;
        }
        if inside && let Some(rest) = trimmed.strip_prefix("version") {
            let rest = rest.trim_start().strip_prefix('=')?.trim();
            return rest
                .strip_prefix('"')
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string);
        }
    }
    None
}

fn shipping_version() -> String {
    let manifest = read(&repo_root().join("Cargo.toml")).expect("root Cargo.toml");
    table_version(&manifest, "[package]").expect("root [package] version")
}

/// The `## [...]` section for one version, up to the next `## ` heading.
/// Tolerates both `## [0.1.2] - date` (CHANGELOG) and `## 0.1.2 — date` (site).
fn version_block(text: &str, version: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut start = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            let label = rest
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or(rest);
            let label = label
                .split([' ', '-', '–', '—'])
                .next()
                .unwrap_or(label)
                .trim();
            if label == version {
                start = Some(index + 1);
                break;
            }
        }
    }
    let Some(start) = start else {
        return String::new();
    };
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate().skip(start) {
        if line.trim_start().starts_with("## ") {
            end = index;
            break;
        }
    }
    lines[start..end].join("\n")
}

fn flatten(block: &str) -> String {
    block.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn contains_negation(sentence: &str) -> bool {
    [
        "not ",
        "no ",
        "never",
        "unsigned",
        "unverified",
        "conditional",
        "caveat",
        "diagnostic",
        "dry run",
        "does not",
        "doesn't",
        "cannot",
        "remains",
        "still",
        "partial",
        "open",
    ]
    .iter()
    .any(|needle| sentence.contains(needle))
}

/// Forbidden claim phrases present as affirmative claims in a release block.
fn forbidden_claims(block: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for sentence in flatten(block).to_lowercase().split(". ") {
        let sentence = sentence.trim();
        // Truthful, named attestations are carved out before scanning.
        let mut scrubbed = sentence.replace("minisign-signed", " ");
        while let Some(start) = scrubbed.find("signed with") {
            let Some(offset) = scrubbed[start..].find("minisign key") else {
                break;
            };
            let end = start + offset + "minisign key".len();
            scrubbed.replace_range(start..end, " ");
        }
        if contains_negation(sentence) {
            continue;
        }
        for phrase in [
            "team memory",
            "signed",
            "attested",
            "official benchmark",
            "nobody does this",
            "verified by cloud",
        ] {
            if scrubbed.contains(phrase) {
                hits.push(phrase.to_string());
            }
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

/// Numbered known-issue entries that are not explicitly marked closed.
fn open_known_issues(text: &str) -> Vec<String> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                if let Some(entry) = current.take() {
                    entries.push(entry);
                }
                let title = rest
                    .split_once(". ")
                    .map(|(_, title)| title)
                    .unwrap_or(rest)
                    .trim()
                    .to_string();
                current = Some((title, String::new()));
            }
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(trimmed);
            body.push('\n');
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
        .into_iter()
        .filter_map(|(title, body)| {
            let status = format!("{title}\n{body}").to_lowercase();
            let closed = ["status: closed", "status: resolved", "status: fixed"]
                .iter()
                .any(|marker| status.contains(marker));
            (!closed).then_some(title)
        })
        .collect()
}

const CLAIM_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "was", "were", "been", "not", "cannot", "will",
    "would", "shall", "should", "does", "have", "has", "had", "which", "when", "where", "how",
    "why", "but", "then", "than", "such", "more", "most", "other", "into", "about", "above",
    "below", "under", "over", "again", "once", "there", "these", "those", "some", "each", "only",
    "very", "may", "might", "must", "depends", "self", "update",
];

fn closure_claims(block: &str, open_entries: &[String]) -> Vec<String> {
    let flat = flatten(block);
    if flat.is_empty() {
        return Vec::new();
    }
    let lower = flat.to_lowercase();
    let mut hits = Vec::new();
    if lower.contains("no known issues") {
        hits.push("no known issues".to_string());
    }
    let closure_verbs = [
        "fixed",
        "resolved",
        "closed",
        "addressed",
        "completed",
        "shipped",
        "done",
        "no longer",
        "now works",
    ];
    if closure_verbs.iter().any(|verb| lower.contains(verb)) && !contains_negation(&lower) {
        if lower.contains("all known issues") || lower.contains("every known issue") {
            hits.push("all known issues resolved".to_string());
        }
        for title in open_entries {
            let lowered = title.to_lowercase();
            let tokens: Vec<&str> = lowered
                .split(|c: char| !c.is_alphanumeric() && c != '.')
                .filter(|word| word.len() >= 3 && !CLAIM_STOPWORDS.contains(word))
                .collect();
            if !tokens.is_empty() && tokens.iter().all(|token| lower.contains(token)) {
                hits.push(format!("open issue '{title}' claimed closed"));
            }
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

#[test]
fn forbidden_claims_are_absent_from_the_shipping_release_notes() {
    let version = shipping_version();
    let changelog = read(&repo_root().join("CHANGELOG.md")).expect("CHANGELOG.md");
    let block = version_block(&changelog, &version);
    assert!(!block.is_empty(), "no v{version} block in CHANGELOG.md");
    let hits = forbidden_claims(&block);
    assert!(hits.is_empty(), "forbidden claims in v{version}: {hits:?}");

    if let Some(site) = read(&workspace_root().join("web/content/docs/changelog.mdx")) {
        let hits = forbidden_claims(&version_block(&site, &version));
        assert!(
            hits.is_empty(),
            "forbidden claims in web v{version}: {hits:?}"
        );
    }
}

#[test]
fn open_known_issues_are_not_claimed_closed() {
    let Some(known) = read(&workspace_root().join("docs/operations/known-issues.md")) else {
        return; // CLI-only checkout: no workspace docs.
    };
    let open = open_known_issues(&known);
    assert!(!open.is_empty(), "expected at least one open known issue");

    let version = shipping_version();
    let changelog = read(&repo_root().join("CHANGELOG.md")).expect("CHANGELOG.md");
    let hits = closure_claims(&version_block(&changelog, &version), &open);
    assert!(
        hits.is_empty(),
        "false closure claims in v{version}: {hits:?}"
    );

    if let Some(site) = read(&workspace_root().join("web/content/docs/changelog.mdx")) {
        let hits = closure_claims(&version_block(&site, &version), &open);
        assert!(
            hits.is_empty(),
            "false closure claims in web v{version}: {hits:?}"
        );
    }
}

#[test]
fn version_surfaces_agree() {
    let root = repo_root();
    let manifest = read(&root.join("Cargo.toml")).expect("root Cargo.toml");
    let package = table_version(&manifest, "[package]").expect("root package version");
    let workspace = table_version(&manifest, "[workspace.package]").expect("workspace version");
    assert_eq!(
        package, workspace,
        "root package and workspace versions disagree"
    );

    let mut manifests: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .expect("crates directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("Cargo.toml"))
        .filter(|path| path.is_file())
        .collect();
    manifests.sort();
    assert!(manifests.len() > 20, "expected the workspace crates");
    for manifest in manifests {
        let text = read(&manifest).expect("crate Cargo.toml");
        if let Some(version) = table_version(&text, "[package]") {
            assert_eq!(
                version,
                package,
                "{} states {version}, root states {package}",
                manifest.display()
            );
        }
    }

    let changelog = read(&root.join("CHANGELOG.md")).expect("CHANGELOG.md");
    let newest = changelog
        .lines()
        .filter(|line| line.starts_with("## ["))
        .map(|line| {
            line.trim_start_matches("## [")
                .split(']')
                .next()
                .unwrap()
                .trim()
        })
        .find(|label| !label.eq_ignore_ascii_case("unreleased"))
        .expect("a versioned changelog heading");
    assert_eq!(
        newest, package,
        "newest changelog heading and Cargo disagree"
    );
}

fn python() -> Option<&'static str> {
    ["python3", "python"].iter().copied().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    })
}

#[test]
fn the_python_gate_self_test_passes() {
    let Some(python) = python() else {
        return; // No Python on this host; the Rust tests above still enforce the gate.
    };
    let output = Command::new(python)
        .arg(repo_root().join("scripts/check-release-notes.py"))
        .arg("--self-test")
        .current_dir(repo_root())
        .output()
        .expect("run the release-notes self-test");
    assert!(
        output.status.success(),
        "gate self-test failed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

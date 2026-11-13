//! One version, in every manifest that states one.
//!
//! W12-12's gate: the version was written down in the root package, in
//! `[workspace.package]`, and in all 30 crate manifests by hand, so a release
//! could bump four of them and ship a binary whose `--version` disagreed with the
//! archive name. This test fails the moment those copies drift, which is cheaper
//! than a release script that remembers to edit them.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// The `version` inside a table, and only that table: a crate's `[dependencies]`
/// can pin another crate's version and that is not this crate's version.
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

#[test]
fn every_manifest_states_the_same_version() {
    let root = repo_root();
    let root_manifest = read(&root.join("Cargo.toml"));
    let package = table_version(&root_manifest, "[package]").expect("root package version");
    let workspace =
        table_version(&root_manifest, "[workspace.package]").expect("workspace package version");
    assert_eq!(
        package, workspace,
        "the binary's version and the inherited version disagree"
    );

    let mut manifests: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .expect("crates directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("Cargo.toml"))
        .filter(|path| path.is_file())
        .collect();
    manifests.sort();
    assert!(
        manifests.len() > 20,
        "expected the workspace's crates, found {}",
        manifests.len()
    );

    for manifest in manifests {
        let text = read(&manifest);
        let Some(version) = table_version(&text, "[package]") else {
            continue;
        };
        assert_eq!(
            version,
            package,
            "{} states {version}, the root package states {package}",
            manifest.display()
        );
        assert!(
            !text.contains("version.workspace = true"),
            "{} inherits the workspace version; this test only guards copies, so keep one shape",
            manifest.display()
        );
    }
}

#[test]
fn the_changelog_names_the_shipping_version_or_unreleased() {
    let changelog = read(&repo_root().join("CHANGELOG.md"));
    let first = changelog
        .lines()
        .find(|line| line.starts_with("## ["))
        .expect("a changelog heading");
    assert!(
        first.contains("[Unreleased]") || first.contains("0.1.0-beta.2"),
        "the newest changelog section is {first:?}, which names neither the shipping version nor Unreleased"
    );
}

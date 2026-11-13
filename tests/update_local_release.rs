//! The update path against a release staged on disk.
//!
//! `MEMNOTH_UPDATE_BASE` already accepts a path instead of a host, so a
//! release can be cut locally as a directory shaped exactly like the published
//! one. These tests take that seriously: they build `latest/manifest.json` the
//! way the release script writes it, install it onto a copy of the real binary,
//! and check what the swap left behind. A published release goes through the
//! same download, checksum, `--version` and rename — only the transport differs.
//!
//! Run with `cargo test --test update_local_release`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use memnoth_update::{
    Channel, UpdateConfig, apply_update_to, backup_path, fetch_manifest, target_triple,
};
use sha2::{Digest, Sha256};

/// The binary this build produced, staged as if it were the published artifact.
const BUILT: &str = env!("CARGO_BIN_EXE_memnoth");

/// The version the built binary reports, which is what the manifest must claim.
fn built_version() -> String {
    let output = Command::new(BUILT).arg("--version").output().unwrap();
    assert!(output.status.success(), "{} --version failed", BUILT);
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("no version in {text:?}"))
        .to_string()
}

fn sha256(path: &Path) -> String {
    hex::encode(Sha256::digest(fs::read(path).unwrap()))
}

/// A release directory exactly as `dist.sh` publishes one: `latest/` holding
/// the manifest and the per-platform archive, with the digest of each.
fn stage_release(root: &Path, version: &str) -> (String, PathBuf) {
    let triple = target_triple();
    let filename = format!("memnoth-{version}-{triple}.zip");
    let latest = root.join("latest");
    fs::create_dir_all(&latest).unwrap();
    let archive = latest.join(&filename);

    let file = fs::File::create(&archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    let entry = if cfg!(windows) {
        "memnoth.exe"
    } else {
        "memnoth"
    };
    zip.start_file(entry, options).unwrap();
    zip.write_all(&fs::read(BUILT).unwrap()).unwrap();
    zip.finish().unwrap();

    let manifest = format!(
        r#"{{
  "version": "{version}",
  "channel": "stable",
  "targets": [
    {{ "triple": "{triple}", "filename": "{filename}", "sha256": "{digest}" }}
  ],
  "checksums": {{ "sha256": "{digest}" }},
  "signatures": {{ "minisign": null, "cosign": null }},
  "uninstall": null,
  "previous_versions": [],
  "published_at": "2026-11-13T00:00:00Z",
  "notes": "staged from a local build for the update test"
}}"#,
        digest = sha256(&archive)
    );
    fs::write(latest.join("manifest.json"), manifest).unwrap();
    (filename, archive)
}

/// A configuration that reads the staged directory instead of a host.
///
/// `current_version` is older than the staged release on purpose: that is the
/// question an installed build asks, and a same-version manifest would be
/// refused as no update at all.
fn config_for(root: &Path) -> UpdateConfig {
    UpdateConfig {
        dist_base_url: root.display().to_string(),
        current_version: "0.0.0".to_string(),
        channel: Channel::Stable,
        passive_disabled: true,
        quiet: true,
        minisign_pubkey: None,
        cosign_pubkey: None,
    }
}

/// An installed binary that is not the one being tested, so "replaced" is a
/// fact the assertions can see rather than something the hashes cannot tell.
fn installed_copy(dir: &Path) -> PathBuf {
    let name = if cfg!(windows) { "mn.exe" } else { "mn" };
    let path = dir.join(name);
    fs::write(&path, b"the build that was installed before\n").unwrap();
    path
}

#[test]
fn a_staged_release_installs_onto_an_installed_copy_and_keeps_the_old_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let version = built_version();
    stage_release(dir.path(), &version);

    let install_dir = dir.path().join("install");
    fs::create_dir_all(&install_dir).unwrap();
    let installed = installed_copy(&install_dir);

    let config = config_for(dir.path());
    // Read through the source, not a literal path: this is the same manifest
    // the updater fetches, including the digest it will verify with.
    let manifest = fetch_manifest(&config, Channel::Stable).unwrap();
    assert_eq!(manifest.version, version);

    apply_update_to(&installed, &config, &manifest, false, "latest", true)
        .expect("a staged release installs onto a copy");

    // The file that is now installed is the artifact, proven by asking it.
    let output = Command::new(&installed).arg("--version").output().unwrap();
    assert!(output.status.success(), "the installed copy does not run");
    let reported = String::from_utf8_lossy(&output.stdout);
    assert!(
        reported.contains(&version),
        "installed copy reports {reported:?}, expected {version}"
    );

    // Rollback material is the build that was there, byte for byte.
    let kept = backup_path(&installed);
    assert_eq!(
        fs::read(&kept).unwrap(),
        b"the build that was installed before\n",
        "the previous binary must survive the swap at {}",
        kept.display()
    );
}

#[test]
fn a_manifest_that_walks_the_machine_backwards_is_refused_before_the_swap() {
    let dir = tempfile::tempdir().unwrap();
    let version = built_version();
    stage_release(dir.path(), &version);

    let install_dir = dir.path().join("install");
    fs::create_dir_all(&install_dir).unwrap();
    let installed = installed_copy(&install_dir);

    let mut config = config_for(dir.path());
    config.current_version = "99.0.0".to_string();
    let manifest = fetch_manifest(&config, Channel::Stable).unwrap();

    let error = apply_update_to(&installed, &config, &manifest, false, "latest", true)
        .expect_err("a downgrade is not an update");
    assert!(
        format!("{error:#}").contains("Refusing to downgrade"),
        "{error:#}"
    );
    assert_eq!(
        fs::read(&installed).unwrap(),
        b"the build that was installed before\n",
        "a refused update must not touch the installed binary"
    );
    assert!(
        !backup_path(&installed).exists(),
        "a refused update must not leave a backup behind"
    );
}

#[test]
fn a_local_source_is_read_as_a_path_not_a_host() {
    let dir = tempfile::tempdir().unwrap();
    let version = built_version();
    stage_release(dir.path(), &version);

    // A plain path and a `file://` URL name the same directory. Windows paths
    // carry a drive letter, which is exactly what would be read as a scheme if
    // the check were "contains a colon".
    let plain = config_for(dir.path());
    let url = UpdateConfig {
        dist_base_url: format!("file://{}", dir.path().display()),
        ..config_for(dir.path())
    };
    for config in [plain, url] {
        let manifest = fetch_manifest(&config, Channel::Stable).unwrap();
        assert_eq!(manifest.version, version);
        assert_eq!(manifest.targets.len(), 1);
    }
}

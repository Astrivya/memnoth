//! `vault backup --compress` writes a real gzip backup and `vault restore`
//! round-trips it (detected by magic bytes, not by file name).

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .env("MEMNOTH_HOME", home)
        .env_remove("MEMNOTH_VAULT")
        .output()
        .expect("run memnoth")
}

#[test]
fn a_compressed_backup_round_trips_through_restore() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");

    let init = run(&["vault", "init"], &home);
    assert!(init.status.success(), "vault init failed");

    let target = dir.path().join("vault.bak.gz");
    let target = target.to_string_lossy().into_owned();
    let compressed = run(
        &["vault", "backup", "--output", &target, "--compress"],
        &home,
    );
    assert!(
        compressed.status.success(),
        "gzip backup must work: {}",
        String::from_utf8_lossy(&compressed.stderr)
    );

    // The file is a gzip stream, not a SQLite copy.
    let bytes = std::fs::read(&target).unwrap();
    assert!(
        bytes.starts_with(&[0x1f, 0x8b]),
        "a --compress backup must start with the gzip magic"
    );

    // Point the vault elsewhere and restore from the gzip file.
    let home2 = dir.path().join("home2");
    let init2 = run(&["vault", "init"], &home2);
    assert!(init2.status.success(), "second vault init failed");
    // Replace it, so --force is required.
    let restored = run(&["vault", "restore", "--input", &target, "--force"], &home2);
    assert!(
        restored.status.success(),
        "restore from a gzip backup must work: {}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let stats = run(&["vault", "stats"], &home2);
    assert!(
        stats.status.success(),
        "the restored vault must open: {}",
        String::from_utf8_lossy(&stats.stderr)
    );

    // And the plain path still works.
    let plain = run(&["vault", "backup", "--output", &target], &home);
    assert!(
        plain.status.success(),
        "a backup without --compress must work: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let plain_bytes = std::fs::read(&target).unwrap();
    assert!(
        plain_bytes.starts_with(b"SQLite format 3"),
        "a backup without --compress stays a byte-for-byte copy"
    );
}

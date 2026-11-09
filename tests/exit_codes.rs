//! Exit 8 is distinct from exit 2, and this is the test that keeps it so.
//!
//! A bad flag and a missing feature look the same to a naive CLI: both are "the
//! command did not run". They are not the same to a script. Exit 2 means the
//! caller passed something wrong and must change the request; exit 8 means the
//! request was right and this build cannot serve it, so the same command works
//! elsewhere. `vault backup --compress` is the one flag that is parsed,
//! documented and not compiled in.

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
fn a_feature_this_build_lacks_exits_8() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");

    // The flag needs a vault to act on; create one so the compression refusal is
    // the only thing standing between the call and a backup.
    let init = run(&["vault", "init"], &home);
    assert!(init.status.success(), "vault init failed");

    let target = dir.path().join("vault.bak");
    let target = target.to_string_lossy().into_owned();
    let refused = run(
        &["vault", "backup", "--output", &target, "--compress"],
        &home,
    );
    assert_eq!(
        refused.status.code(),
        Some(8),
        "an unbuilt feature must exit 8, not 2: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not available in this build"),
        "the error should say what is missing: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--compress"),
        "the fix should name the flag to drop"
    );

    // The same verb without the flag still works, so 8 is about the flag.
    let plain = run(&["vault", "backup", "--output", &target], &home);
    assert!(
        plain.status.success(),
        "a backup without --compress must work: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
}

//! The man page is generated from clap, so it has a drift mode like the rest.
//!
//! A page that is written but never compared is a page that describes last
//! release's flags. These tests pin write, match and stale.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn run(args: &[&str], home: &std::path::Path) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .env("MEMNOTH_HOME", home)
        .output()
        .expect("run memnoth")
}

#[test]
fn a_written_man_page_matches_and_an_edited_one_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let path = dir.path().join("memnoth.1");
    let text = path.to_string_lossy().into_owned();

    let written = run(&["docs", "--man", "--output", &text], &home);
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );
    let page = std::fs::read_to_string(&path).unwrap();
    assert!(page.contains(".TH"), "roff starts with a title:\n{page}");
    assert!(page.contains("memnoth"), "the page names the binary");

    let fresh = run(&["docs", "--man", "--output", &text, "--check"], &home);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );

    std::fs::write(&path, "not a man page\n").unwrap();
    let stale = run(&["docs", "--man", "--output", &text, "--check"], &home);
    assert_eq!(
        stale.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("stale"),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
}

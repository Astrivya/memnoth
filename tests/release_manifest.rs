//! The release manifest is the single description of the five published
//! targets. The installers must not grow a second list.

use std::collections::BTreeSet;

const MANIFEST: &str = r#"
{
  "version": "9.9.9",
  "channel": "stable",
  "targets": [
    {"triple": "aarch64-apple-darwin", "filename": "memnoth-aarch64-apple-darwin.tar.gz", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
    {"triple": "aarch64-unknown-linux-gnu", "filename": "memnoth-aarch64-unknown-linux-gnu.tar.gz", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
    {"triple": "x86_64-apple-darwin", "filename": "memnoth-x86_64-apple-darwin.tar.gz", "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"},
    {"triple": "x86_64-pc-windows-msvc", "filename": "memnoth-x86_64-pc-windows-msvc.zip", "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"},
    {"triple": "x86_64-unknown-linux-gnu", "filename": "memnoth-x86_64-unknown-linux-gnu.tar.gz", "sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}
  ],
  "checksums": {"sha256": ""},
  "signatures": {"minisign": null, "cosign": null},
  "published_at": "2026-11-09T00:00:00Z",
  "notes": null
}
"#;

#[test]
fn release_manifest_and_consumers_agree_on_five_targets() {
    let manifest: memnoth_update::Manifest =
        serde_json::from_str(MANIFEST).expect("the release manifest must parse");
    let manifest_targets: BTreeSet<String> =
        manifest.targets.iter().map(|t| t.triple.clone()).collect();

    let expected: BTreeSet<String> = [
        "aarch64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert_eq!(manifest_targets, expected);

    for target in &manifest.targets {
        assert!(
            !target.filename.is_empty(),
            "{} has no artifact filename",
            target.triple
        );
        assert_eq!(
            target.sha256.len(),
            64,
            "{} does not carry a full digest",
            target.triple
        );
    }

    let install_sh = include_str!("../scripts/install.sh");
    for target in &expected {
        assert!(
            install_sh.contains(target),
            "install.sh does not support {target}"
        );
    }

    let install_ps1 = include_str!("../scripts/install.ps1");
    assert!(
        install_ps1.contains("x86_64-pc-windows-msvc"),
        "install.ps1 does not support the Windows target"
    );

    assert!(
        expected.contains(&memnoth_update::target_triple()),
        "the updater asks for a target the release does not publish"
    );

    let dist = include_str!("../scripts/dist.sh");
    for field in ["\"targets\"", "\"filename\"", "\"sha256\""] {
        assert!(dist.contains(field), "dist.sh no longer writes {field}");
    }
}

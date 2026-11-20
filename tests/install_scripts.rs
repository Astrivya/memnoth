//! Offline checks for scripts/install.sh.
//!
//! These build a synthetic dist tree on disk and run the real installer against
//! it with `MEMNOTH_INSTALL_BASE` pointing at that tree. No network, no signing
//! key and no real release are involved. The fake payload is a tiny shell
//! script named `memnoth` that answers `--version`; the installer is allowed to
//! run that, and the test never asks it to do anything else.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Git Bash's `sh` understands `/d/...`; the Windows native tools do not. Pass
/// POSIX-style paths to the installer on every platform.
fn posix(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' && bytes[0].is_ascii_alphabetic() {
        format!("/{}{}", (bytes[0] as char).to_ascii_lowercase(), &text[2..])
    } else {
        text
    }
}

fn find_shells() -> Option<(PathBuf, PathBuf)> {
    if let Ok(bash) = which("bash") {
        let sh = if cfg!(windows) {
            bash.with_file_name("sh.exe")
        } else {
            bash.with_file_name("sh")
        };
        if sh.is_file() {
            return Some((bash, sh));
        }
    }
    // Windows: Git Bash is not always on PATH, but its install location is.
    for dir in [
        r"C:\Program Files\Git\bin",
        r"C:\Program Files\Git\usr\bin",
        r"C:\Program Files (x86)\Git\bin",
    ] {
        let bash = Path::new(dir).join("bash.exe");
        let sh = Path::new(dir).join("sh.exe");
        if bash.is_file() && sh.is_file() {
            return Some((bash, sh));
        }
    }
    None
}

fn which(name: &str) -> std::io::Result<PathBuf> {
    let output = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", &format!("where {name}")])
            .output()?
    } else {
        Command::new("sh")
            .args(["-c", &format!("command -v {name}")])
            .output()?
    };
    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{name} not found"),
        ));
    }
    let first = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if first.is_empty() {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{name} not found"),
        ))
    } else {
        Ok(PathBuf::from(first))
    }
}

/// Run a command line inside the same shell the installer will use, so `tar`
/// and `sha256sum` resolve exactly as they do for the installer itself.
fn sh_run(sh: &Path, script: &str) -> Output {
    Command::new(sh)
        .arg("-c")
        .arg(script)
        .output()
        .expect("run sh")
}

fn fixture_root() -> PathBuf {
    let base = Path::new(r"D:\memnoth-builds-cache");
    let base = if base.is_dir() {
        base.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let unique = format!(
        "memnoth-install-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    base.join(unique)
}

/// Write a dist tree shaped exactly like `dist.sh` publishes: `<v>/…` plus a
/// `latest/` copy, with a manifest that carries per-target digests.
fn make_dist(sh: &Path, root: &Path, version: &str) -> PathBuf {
    let archive_name = "memnoth-x86_64-unknown-linux-gnu.tar.gz";
    let payload_dir = root.join("payload");
    let version_dir = root.join("dist").join(format!("v{version}"));
    let latest_dir = root.join("dist").join("latest");
    fs::create_dir_all(&payload_dir).expect("payload dir");
    fs::create_dir_all(&version_dir).expect("version dir");
    fs::create_dir_all(&latest_dir).expect("latest dir");

    let fake = payload_dir.join("memnoth");
    fs::write(
        &fake,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'memnoth 9.8.7'; exit 0; fi\necho 'fake payload'\nexit 0\n",
    )
    .expect("write fake binary");

    let archive = version_dir.join(archive_name);
    let made = sh_run(
        sh,
        &format!(
            "tar -czf '{}' -C '{}' memnoth",
            posix(&archive),
            posix(&payload_dir)
        ),
    );
    assert!(
        made.status.success(),
        "tar failed: {}",
        String::from_utf8_lossy(&made.stderr)
    );

    let hashed = sh_run(sh, &format!("sha256sum '{}'", posix(&archive)));
    assert!(
        hashed.status.success(),
        "sha256sum failed: {}",
        String::from_utf8_lossy(&hashed.stderr)
    );
    let digest = String::from_utf8_lossy(&hashed.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(digest.len(), 64, "expected a 64-hex digest, got {digest:?}");

    fs::write(
        version_dir.join(format!("{archive_name}.sha256")),
        format!("{digest}  {archive_name}\n"),
    )
    .expect("write sidecar");

    let manifest = format!(
        concat!(
            "{{\n",
            "  \"version\": \"{version}\",\n",
            "  \"channel\": \"stable\",\n",
            "  \"previous_versions\": [],\n",
            "  \"targets\": [\n",
            "    {{\"triple\": \"x86_64-unknown-linux-gnu\", \"filename\": \"{name}\", \"sha256\": \"{digest}\"}}\n",
            "  ],\n",
            "  \"checksums\": {{\"sha256\": \"{digest}\"}},\n",
            "  \"signatures\": {{\"minisign\": null, \"cosign\": null}},\n",
            "  \"published_at\": \"2026-11-20T00:00:00Z\",\n",
            "  \"notes\": null\n",
            "}}\n"
        ),
        version = version,
        name = archive_name,
        digest = digest,
    );
    fs::write(version_dir.join("manifest.json"), &manifest).expect("write manifest");

    for name in [
        archive_name,
        &format!("{archive_name}.sha256"),
        "manifest.json",
    ] {
        fs::copy(version_dir.join(name), latest_dir.join(name)).expect("copy to latest");
    }

    root.join("dist")
}

fn run_installer(
    sh: &Path,
    dist: &Path,
    version: &str,
    install_dir: &Path,
    temp_dir: &Path,
) -> Output {
    let script = repo_root().join("scripts").join("install.sh");
    Command::new(sh)
        .arg(posix(&script))
        .env("MEMNOTH_INSTALL_BASE", posix(dist))
        .env("MEMNOTH_VERSION", version)
        .env("MEMNOTH_TARGET", "x86_64-unknown-linux-gnu")
        .env("MEMNOTH_INSTALL_DIR", posix(install_dir))
        .env("MEMNOTH_TEMP_DIR", posix(temp_dir))
        .output()
        .expect("run install.sh")
}

#[test]
fn install_from_local_dist_prefers_manifest_and_installs_both_names() {
    let Some((bash, sh)) = find_shells() else {
        eprintln!("skipping: no bash/sh found; install.sh needs a POSIX shell");
        return;
    };
    let _ = bash;
    let fixture = Fixture {
        root: fixture_root(),
    };
    let dist = make_dist(&sh, &fixture.root, "9.8.7");
    let bin = fixture.root.join("bin");
    let tmp = fixture.root.join("tmp");
    fs::create_dir_all(&tmp).expect("temp dir");

    let output = run_installer(&sh, &dist, "latest", &bin, &tmp);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "installer failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("using manifest target"), "{stdout}");
    assert!(stdout.contains("checksum from manifest"), "{stdout}");
    assert!(stdout.contains("checksum ok"), "{stdout}");
    // An unsigned release must be announced, not silently accepted as signed.
    assert!(stderr.contains("unsigned"), "{stderr}");

    let installed = bin.join("memnoth");
    let alias = bin.join("mn");
    assert!(installed.is_file(), "missing {installed:?}");
    assert!(alias.exists(), "missing {alias:?}");

    for name in [&installed, &alias] {
        let ran = Command::new(&sh)
            .arg(posix(name))
            .arg("--version")
            .output()
            .expect("run installed payload");
        assert!(
            ran.status.success(),
            "{} did not run: {}",
            name.display(),
            String::from_utf8_lossy(&ran.stderr)
        );
        assert!(
            String::from_utf8_lossy(&ran.stdout).contains("9.8.7"),
            "{} reported the wrong version",
            name.display()
        );
    }

    // The manifest digest describes the archive; the installed file must be
    // the payload that archive carried, byte for byte.
    let payload = fs::read(fixture.root.join("payload").join("memnoth")).expect("read payload");
    assert_eq!(
        fs::read(&installed).expect("read installed"),
        payload,
        "installed file is not the archive payload"
    );
}

#[test]
fn a_corrupted_archive_is_rejected_and_nothing_is_installed() {
    let Some((_bash, sh)) = find_shells() else {
        eprintln!("skipping: no bash/sh found; install.sh needs a POSIX shell");
        return;
    };
    let fixture = Fixture {
        root: fixture_root(),
    };
    let dist = make_dist(&sh, &fixture.root, "9.8.7");
    let archive = dist
        .join("latest")
        .join("memnoth-x86_64-unknown-linux-gnu.tar.gz");
    fs::write(
        &archive,
        b"this is not the archive the manifest describes\n",
    )
    .expect("corrupt archive");

    let bin = fixture.root.join("corrupt-bin");
    let tmp = fixture.root.join("corrupt-tmp");
    fs::create_dir_all(&tmp).expect("temp dir");
    let output = run_installer(&sh, &dist, "latest", &bin, &tmp);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a corrupted archive must not install\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("checksum mismatch"),
        "expected an explicit checksum failure\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !bin.join("memnoth").exists(),
        "corrupt binary was installed"
    );
    assert!(!bin.join("mn").exists(), "corrupt alias was installed");
}

#[test]
fn a_bare_semver_finds_the_v_prefixed_release_directory() {
    let Some((_bash, sh)) = find_shells() else {
        eprintln!("skipping: no bash/sh found; install.sh needs a POSIX shell");
        return;
    };
    let fixture = Fixture {
        root: fixture_root(),
    };
    let dist = make_dist(&sh, &fixture.root, "9.8.7");
    let bin = fixture.root.join("explicit-bin");
    let tmp = fixture.root.join("explicit-tmp");
    fs::create_dir_all(&tmp).expect("temp dir");

    let output = run_installer(&sh, &dist, "9.8.7", &bin, &tmp);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "bare version did not resolve\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(bin.join("memnoth").is_file());
    assert!(bin.join("mn").exists());
}

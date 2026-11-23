//! Clean-install acceptance harness — STRUCTURAL CHECKS ONLY.
//!
//! NOT A CLEAN-MACHINE RUN. No fresh OS is installed, no `curl | bash` or
//! `irm | iex` is executed, no network is contacted and no real home directory
//! or PATH is touched. Every test below is labelled in its own output and
//! proves a property of the source and of the locally built binary:
//!
//!   * the advertised one-liners have the right shape in both shells;
//!   * Cargo, the release manifest and the installed payload agree on a version;
//!   * `doctor --fix` is idempotent in an isolated home;
//!   * `uninstall` removes the installed binary but keeps the vault;
//!   * an unsigned manifest is refused by the signature gate.
//!
//! These are necessary structural checks, not consumer acceptance. Closing
//! P22-003 / P22-017 / P22-019 / P22-020 / P22-021 still needs a real signed
//! release and a transcript from a clean machine per target. See
//! `docs/operations/clean-machine-install.md` and `scripts/check-clean-install.sh`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use chrono::Utc;
use memnoth_update::{
    Channel, Checksums, Manifest, Signatures, Target, UpdateConfig, fetch_manifest, target_triple,
    verify_signatures,
};
use serde_json::Value;

const LABEL: &str = "STRUCTURAL CHECK - NOT A CLEAN-MACHINE RUN; not consumer acceptance";

fn label() {
    eprintln!("clean_install_acceptance: {LABEL}");
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Git Bash's `sh` understands `/d/...`; native Windows tools do not.
fn posix(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' && bytes[0].is_ascii_alphabetic() {
        format!("/{}{}", (bytes[0] as char).to_ascii_lowercase(), &text[2..])
    } else {
        text
    }
}

fn sh_run(sh: &Path, script: &str) -> Output {
    Command::new(sh)
        .arg("-c")
        .arg(script)
        .output()
        .expect("run sh")
}

fn find_bash() -> Option<PathBuf> {
    for dir in [
        r"C:\Program Files\Git\bin",
        r"C:\Program Files\Git\usr\bin",
        r"C:\Program Files (x86)\Git\bin",
    ] {
        let candidate = Path::new(dir).join("bash.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let output = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "where bash"])
            .output()
            .ok()?
    } else {
        Command::new("sh")
            .args(["-c", "command -v bash"])
            .output()
            .ok()?
    };
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(|line| PathBuf::from(line.trim()))
        .filter(|path| path.is_file())
}

/// The `version` inside a table, and only that table.
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

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The same dist shape `scripts/dist.sh` publishes: a `v<version>/` directory
/// plus a `latest/` copy, with a manifest that carries the per-target digest.
/// The fake payload only answers `--version`, so the installer runs exactly
/// one command from the archive it verified.
fn make_dist(sh: &Path, root: &Path, version: &str, payload_version: &str) -> PathBuf {
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
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'memnoth {payload_version}'; exit 0; fi\nexit 0\n"
        ),
    )
    .expect("write fake payload");

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
            "  \"published_at\": \"2026-11-21T00:00:00Z\",\n",
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

fn run_installer(sh: &Path, dist: &Path, install_dir: &Path, temp_dir: &Path) -> Output {
    let script = repo_root().join("scripts").join("install.sh");
    Command::new(sh)
        .arg(posix(&script))
        .env("MEMNOTH_INSTALL_BASE", posix(dist))
        .env("MEMNOTH_VERSION", "latest")
        .env("MEMNOTH_TARGET", "x86_64-unknown-linux-gnu")
        .env("MEMNOTH_INSTALL_DIR", posix(install_dir))
        .env("MEMNOTH_TEMP_DIR", posix(temp_dir))
        .output()
        .expect("run install.sh")
}

fn fixture_root() -> PathBuf {
    let base = Path::new(r"D:\memnoth-builds-cache");
    let base = if base.is_dir() {
        base.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let unique = format!(
        "memnoth-clean-install-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    base.join(unique)
}

// ---------------------------------------------------------------------------
// 0. The harness itself must be honest about what it is.
// ---------------------------------------------------------------------------

#[test]
fn the_harness_is_labelled_structural_and_stays_off_real_state() {
    label();
    let script = read(&repo_root().join("scripts").join("check-clean-install.sh"));
    assert!(
        script.contains("NOT A CLEAN-MACHINE RUN"),
        "the harness must name what it is not"
    );
    assert!(
        script.contains("not consumer acceptance"),
        "the harness must not claim consumer acceptance"
    );
    for required in [
        "MEMNOTH_HOME=",
        "USERPROFILE=",
        "HOME=",
        "mktemp -d",
        "trap ",
        "grep -F",
        "--require-signature",
    ] {
        assert!(
            script.contains(required),
            "the harness no longer contains {required:?}"
        );
    }
    assert!(
        !script.contains("curl -fsSL https://memnoth.astrivya.ai/install | bash\n"),
        "the harness must describe the one-liner, not pipe it into a shell"
    );
}

#[test]
fn bash_n_accepts_the_harness() {
    label();
    let script = repo_root().join("scripts").join("check-clean-install.sh");
    let Some(bash) = find_bash() else {
        eprintln!("SKIP: no bash found; bash -n could not be run");
        return;
    };
    let output = Command::new(bash)
        .arg("-n")
        .arg(&script)
        .output()
        .expect("run bash -n");
    assert!(
        output.status.success(),
        "bash -n rejected the harness:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// 1. One-line install shape, both shells.
// ---------------------------------------------------------------------------

#[test]
fn one_line_install_shape_is_correct_for_both_shells() {
    label();
    let sh = read(&repo_root().join("scripts").join("install.sh"));
    assert!(
        sh.contains("curl -fsSL https://memnoth.astrivya.ai/install | bash"),
        "the POSIX one-liner drifted"
    );
    for marker in [
        "manifest.json",
        "checksum from manifest",
        "checksum mismatch",
        "this release is unsigned",
    ] {
        assert!(sh.contains(marker), "install.sh lost {marker:?}");
    }

    let ps1 = read(&repo_root().join("scripts").join("install.ps1"));
    for marker in [
        "/install.ps1",
        "| iex",
        "irm ",
        "manifest.json",
        "sha256",
        "This release is unsigned",
    ] {
        assert!(ps1.contains(marker), "install.ps1 lost {marker:?}");
    }

    // The web checkout is a sibling repository; check it when present but skip
    // by name when it is not, rather than making this CLI test depend on it.
    let site = repo_root().parent().map(|p| p.join("web/src/lib/site.ts"));
    match site {
        Some(path) if path.is_file() => {
            let site = read(&path);
            assert!(
                site.contains("curl -fsSL ${siteUrl(\"/install\")} | bash"),
                "the advertised POSIX one-liner drifted"
            );
            // The marketing site currently advertises only the POSIX one-liner.
            // If it advertises a PowerShell one-liner, that link must still
            // resolve to the shipped /install.ps1 installer.
            if site.contains("iex") || site.contains("install.ps1") {
                assert!(
                    site.contains("${siteUrl(\"/install.ps1\")}"),
                    "a Windows one-liner is advertised but does not target /install.ps1"
                );
            }
        }
        _ => eprintln!("SKIP: web/src/lib/site.ts not present; only installer shape checked"),
    }
}

// ---------------------------------------------------------------------------
// 2. Version alignment, including a real installer against a synthetic dist.
// ---------------------------------------------------------------------------

#[test]
fn cargo_manifest_and_installed_payload_agree_on_the_version() {
    label();
    let root = repo_root();
    let cargo =
        table_version(&read(&root.join("Cargo.toml")), "[package]").expect("root package version");

    let built = Command::new(binary())
        .arg("--version")
        .output()
        .expect("run built binary --version");
    assert!(built.status.success());
    let built_version = String::from_utf8_lossy(&built.stdout);
    assert!(
        built_version.contains(&cargo),
        "binary says {built_version:?}, Cargo.toml says {cargo}"
    );

    let Some(bash) = find_bash() else {
        eprintln!("SKIP: no bash found; installer version alignment not exercised");
        return;
    };
    let sh = if cfg!(windows) {
        bash.with_file_name("sh.exe")
    } else {
        bash.with_file_name("sh")
    };
    if !sh.is_file() {
        eprintln!("SKIP: no sh beside bash; installer version alignment not exercised");
        return;
    }
    let fixture = Fixture {
        root: fixture_root(),
    };
    let dist = make_dist(&sh, &fixture.root, &cargo, &cargo);
    let bin = fixture.root.join("bin");
    let tmp = fixture.root.join("tmp");
    fs::create_dir_all(&tmp).expect("temp dir");

    let output = run_installer(&sh, &dist, &bin, &tmp);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "installer failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("using manifest target"), "{stdout}");
    assert!(stdout.contains("checksum from manifest"), "{stdout}");
    assert!(stderr.contains("unsigned"), "{stderr}");

    let installed = bin.join("memnoth");
    assert!(installed.is_file(), "missing {installed:?}");
    let ran = Command::new(&sh)
        .arg(posix(&installed))
        .arg("--version")
        .output()
        .expect("run installed payload");
    assert!(ran.status.success());
    let reported = String::from_utf8_lossy(&ran.stdout);
    assert!(
        reported.contains(&cargo),
        "manifest said {cargo} but the installed payload said {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. doctor --fix idempotence in an isolated home.
// ---------------------------------------------------------------------------

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(dir.path().join("ws/src")).expect("workspace");
        fs::write(
            dir.path().join("ws/src/lib.rs"),
            "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n",
        )
        .expect("source file");
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn run(&self, args: &[&str]) -> Output {
        let root = self.dir.path().join("ws");
        Command::new(binary())
            .arg("--trust-folder")
            .args(args)
            .arg("--root")
            .arg(&root)
            .env("MEMNOTH_HOME", self.home())
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            // On Windows `dirs::home_dir()` ignores USERPROFILE, so isolate the
            // completion path explicitly to avoid racing the real user profile.
            .env("MEMNOTH_COMPLETION_HOME", self.home())
            .env_remove("MEMNOTH_VAULT")
            .env_remove("MEMNOTH_GRAPH")
            .env_remove("MEMNOTH_MCP_BIN")
            .env_remove("MEMNOTH_MCP_TOKEN")
            .env_remove("SHELL")
            .output()
            .expect("spawn memnoth doctor")
    }
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}):\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn doctor_fix_is_idempotent_in_an_isolated_home() {
    label();
    let sandbox = Sandbox::new();

    let first = sandbox.run(&["doctor", "--fix", "--json"]);
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    let first = json(&first)["data"].clone();
    assert!(
        !first["fixed"].as_array().unwrap().is_empty(),
        "a fresh home must have fixes to apply: {first}"
    );

    let second = sandbox.run(&["doctor", "--fix", "--json"]);
    assert_eq!(
        second.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    let second = json(&second)["data"].clone();
    assert!(
        second["fixed"].as_array().unwrap().is_empty(),
        "the second run reapplied a fix: {second}"
    );
    assert_eq!(
        second["before"], second["checks"],
        "the second run changed a check state"
    );
    assert_eq!(
        first["checks"], second["checks"],
        "the check set drifted between the two runs"
    );
}

// ---------------------------------------------------------------------------
// 4. Uninstall removes the installed binary but keeps the vault.
// ---------------------------------------------------------------------------

#[test]
fn uninstall_preserves_the_vault() {
    label();
    let dir = tempfile::tempdir().expect("temp dir");
    let home = dir.path().join("home");
    let install_dir = dir.path().join("install");
    let workspace = dir.path().join("ws");
    for path in [&home, &install_dir, &workspace] {
        fs::create_dir_all(path).expect("isolated dir");
    }

    let installed = install_dir.join(format!("memnoth{}", std::env::consts::EXE_SUFFIX));
    fs::copy(binary(), &installed).expect("stage the binary as an install");
    let sentinel = b"vault-sentinel-do-not-delete";
    let vault = home.join("vault.db");
    fs::write(&vault, sentinel).expect("write a sentinel vault");

    let output = Command::new(&installed)
        .arg("--trust-folder")
        .arg("--root")
        .arg(&workspace)
        .args(["uninstall", "--yes"])
        .env("MEMNOTH_HOME", &home)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        .env_remove("SHELL")
        .output()
        .expect("run uninstall from the staged install");
    assert!(
        output.status.success(),
        "uninstall failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Uninstalled"),
        "uninstall did not report success"
    );

    assert!(vault.is_file(), "uninstall deleted the vault");
    assert_eq!(
        fs::read(&vault).expect("read vault after uninstall"),
        sentinel,
        "uninstall altered the vault"
    );

    // Windows cannot unlink the running image synchronously; the command
    // defers removal for a moment. Poll rather than asserting an instant delete.
    let start = Instant::now();
    while installed.exists() && start.elapsed() < Duration::from_secs(12) {
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        !installed.exists(),
        "uninstall left the installed binary behind"
    );
}

// ---------------------------------------------------------------------------
// 5. A manifest with no signature fails closed.
// ---------------------------------------------------------------------------

/// Public verification key from the `minisign-verify` crate's published test
/// vector. It is not a secret and no signing key is used here: the manifest is
/// unsigned, so verification must refuse it before any signature is checked.
const TEST_PUBKEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

#[test]
fn an_unsigned_manifest_fails_closed() {
    label();
    let dir = tempfile::tempdir().expect("temp dir");
    let latest = dir.path().join("latest");
    fs::create_dir_all(&latest).expect("latest dir");
    fs::write(dir.path().join("SHA256SUMS"), b"test").expect("SHA256SUMS");

    let manifest = Manifest {
        version: "9.9.9".to_string(),
        channel: Channel::Stable,
        targets: vec![Target {
            triple: target_triple(),
            filename: "memnoth-unsigned-test.zip".to_string(),
            sha256: String::new(),
        }],
        checksums: Checksums {
            sha256: String::new(),
        },
        signatures: Signatures {
            minisign: None,
            cosign: None,
        },
        uninstall: None,
        previous_versions: Vec::new(),
        published_at: Utc::now(),
        notes: None,
    };
    fs::write(
        latest.join("manifest.json"),
        serde_json::to_vec(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");

    let config = UpdateConfig {
        dist_base_url: dir.path().display().to_string(),
        minisign_pubkey: Some(TEST_PUBKEY.to_string()),
        cosign_pubkey: None,
        channel: Channel::Stable,
        current_version: "0.0.0".to_string(),
        passive_disabled: true,
        quiet: true,
    };
    // Read through the real manifest reader, so this is the document the
    // updater would actually hand to the signature gate.
    let fetched = fetch_manifest(&config, Channel::Stable).expect("fetch staged manifest");
    assert!(fetched.signatures.minisign.is_none());
    let error = verify_signatures(&dir.path().join("SHA256SUMS"), &fetched, &config)
        .expect_err("a checksum from the same host as the artifact is not a signature");
    assert!(
        error.to_string().contains("unsigned"),
        "expected an explicit unsigned refusal, got {error:#}"
    );

    // The release-surface gate makes the same claim at the manifest layer.
    let surface = read(&repo_root().join("scripts").join("check-install-surface.py"));
    assert!(
        surface.contains("--require-signature") && surface.contains("report.fail(\"signature\""),
        "--require-signature no longer fails a null minisign signature"
    );
}

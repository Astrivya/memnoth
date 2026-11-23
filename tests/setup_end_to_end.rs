//! `mn setup --yes` on a clean machine: the safe steps run without a person,
//! nothing signs in, and the summary never calls a skipped step ready.
//!
//! Every run uses an isolated workspace, an isolated `MEMNOTH_HOME` and no API
//! key variables. Completions are either installed into an isolated home or
//! reported honestly when no supported shell can be detected.

use std::path::PathBuf;
use std::process::{Command, Output};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    let home = dir.path().join("state");
    std::fs::create_dir_all(&root).expect("workspace");
    Fixture {
        _dir: dir,
        root,
        home,
    }
}

fn command(fx: &Fixture, skip: Option<&str>, json: bool) -> Command {
    let mut cmd = Command::new(binary());
    cmd.arg("setup").arg("--yes").arg("--trust-folder");
    if json {
        cmd.arg("--json");
    }
    if let Some(skip) = skip {
        cmd.arg("--skip").arg(skip);
    }
    cmd.current_dir(&fx.root)
        .env("MEMNOTH_HOME", &fx.home)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        .env_remove("MEMNOTH_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("KILO_API_KEY")
        .env_remove("KILOCODE_API_KEY")
        // An unsupported shell makes the completion step report honestly
        // instead of writing into the real user's completion directory.
        .env("SHELL", "/definitely/not/a/shell");
    cmd
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn data(out: &Output) -> serde_json::Value {
    ok(out);
    let envelope: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("setup --json prints a JSON envelope");
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope["data"].clone()
}

fn step<'a>(data: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    data["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("no step named {name}"))
}

fn credential_fingerprint() -> Option<(u64, std::time::SystemTime)> {
    let path = memnoth_auth::default_credentials_path();
    std::fs::metadata(&path)
        .ok()
        .and_then(|meta| meta.modified().ok().map(|modified| (meta.len(), modified)))
}

#[test]
fn yes_with_skips_creates_vault_and_graph_without_signing_in() {
    let fx = fixture();
    let credentials_before = credential_fingerprint();

    let out = command(&fx, Some("auth,provider"), true)
        .output()
        .expect("run setup");
    let report = data(&out);

    assert!(
        fx.home.join("vault.db").is_file(),
        "the vault must exist after setup --yes"
    );
    assert!(
        fx.root.join(".memnoth").join("graph.db").is_file(),
        "the graph must exist after setup --yes"
    );
    assert_eq!(step(&report, "vault")["completed"], true);
    assert_eq!(step(&report, "graph")["completed"], true);

    for name in ["auth", "provider"] {
        assert_eq!(
            step(&report, name)["skipped"],
            true,
            "{name} was not skipped"
        );
        assert_eq!(
            step(&report, name)["completed"],
            false,
            "{name} must not be claimed ready"
        );
    }

    // Completions were not skipped by flag, but no shell was detected, so the
    // step must report that instead of claiming a script was written.
    let completions = step(&report, "completions");
    assert_eq!(completions["skipped"], false);
    assert_eq!(completions["completed"], false);
    assert!(
        completions["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("no shell detected"),
        "{completions}"
    );

    let ready: Vec<&str> = report["ready"]
        .as_array()
        .expect("ready array")
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(!ready.contains(&"auth"), "{ready:?}");
    assert!(!ready.contains(&"provider"), "{ready:?}");
    assert!(!ready.contains(&"completions"), "{ready:?}");

    let verdict = report["verdict"].as_str().expect("verdict text");
    let ready_line = verdict.lines().next().expect("ready line");
    assert!(ready_line.contains("vault ✓"), "{ready_line}");
    assert!(ready_line.contains("graph ✓"), "{ready_line}");
    assert!(!ready_line.contains("auth"), "{ready_line}");
    assert!(!ready_line.contains("provider"), "{ready_line}");
    assert!(!ready_line.contains("completions"), "{ready_line}");

    let not_ready = verdict.lines().nth(1).expect("not ready line");
    assert!(not_ready.contains("auth — skipped (--skip)"), "{not_ready}");
    assert!(
        not_ready.contains("provider — skipped (--skip)"),
        "{not_ready}"
    );
    assert!(
        not_ready.contains("completions — no shell detected"),
        "{not_ready}"
    );

    assert_eq!(credentials_before, credential_fingerprint());
    assert!(
        !fx.home.join("credentials").exists(),
        "no credential file belongs in the state home"
    );
    assert_eq!(report["config_saved"], false);
    assert!(!fx.home.join("config.toml").exists());
}

#[test]
fn yes_never_starts_auth_or_configures_a_provider() {
    let fx = fixture();
    let credentials_before = credential_fingerprint();

    let out = command(&fx, None, true).output().expect("run setup");
    let report = data(&out);

    let auth = step(&report, "auth");
    if auth["completed"].as_bool() == Some(true) {
        // A token was already cached; setup only observed it, it did not sign
        // in. `reason()` is a note for *incomplete* steps, so a completed step
        // has no note ("not attempted") — assert the completion, not a note
        // string that intentionally never exists in that state.
        assert_eq!(auth["skipped"], false);
    } else {
        assert!(
            auth["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("not authenticated"),
            "{auth}"
        );
    }

    let provider = step(&report, "provider");
    assert_eq!(provider["completed"], false, "{provider}");
    assert!(
        provider["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("no provider configured"),
        "{provider}"
    );

    assert_eq!(report["config_saved"], false);
    assert!(!fx.home.join("config.toml").exists());
    assert_eq!(credentials_before, credential_fingerprint());
}

#[test]
fn human_summary_reports_readiness_and_the_reasons() {
    let fx = fixture();
    let out = command(&fx, Some("auth,provider"), false)
        .output()
        .expect("run setup");
    ok(&out);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("ready: vault ✓ graph ✓"), "{text}");
    assert!(
        text.contains("not ready: auth — skipped (--skip)"),
        "{text}"
    );
    assert!(
        text.contains("not ready:") && text.contains("provider — skipped (--skip)"),
        "{text}"
    );
}

#[cfg(not(windows))]
#[test]
fn yes_installs_bash_completions_into_an_isolated_home_and_is_idempotent() {
    let fx = fixture();
    let mut cmd = command(&fx, Some("auth,provider"), true);
    cmd.env("HOME", &fx.home).env("SHELL", "/bin/bash");

    let out = cmd.output().expect("run setup");
    let report = data(&out);

    let script = fx
        .home
        .join(".local/share/bash-completion/completions/memnoth.bash");
    assert!(
        script.is_file(),
        "completion script missing at {}",
        script.display()
    );
    assert_eq!(step(&report, "completions")["completed"], true);
    let ready: Vec<&str> = report["ready"]
        .as_array()
        .expect("ready array")
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(ready.contains(&"completions"), "{ready:?}");

    let mut again = command(&fx, Some("auth,provider"), true);
    again.env("HOME", &fx.home).env("SHELL", "/bin/bash");
    data(&again.output().expect("run setup again"));

    assert!(
        !script.with_extension("bash.bak").exists(),
        "a second setup must not rewrite or back up an installed script"
    );
}

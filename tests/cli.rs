//! The binary's contract, exercised the way a script uses it.
//!
//! Unit tests call the modules; these call `memnoth` itself, because the things
//! most likely to break are the seams the modules cannot see: clap's derived
//! subcommand names (`asof`, not `as-of`), the JSON envelope, exit codes, and the
//! fact that `--root` is accepted *after* the verb, which is how `SKILL.md` tells
//! agents to call `kg index`.
//!
//! Every test gets its own `MEMNOTH_HOME` and workspace, so nothing reads or
//! writes the developer's real vault.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_memnoth")
}

struct Sandbox {
    dir: tempfile::TempDir,
}

struct Run {
    output: Output,
}

impl Run {
    fn code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.output.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout was not JSON ({error}):\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&self.output.stdout),
                String::from_utf8_lossy(&self.output.stderr)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("ws/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("ws/tests")).unwrap();
        std::fs::write(
            dir.path().join("ws/src/lib.rs"),
            "pub fn callee(value: u32) -> u32 {\n    value + 1\n}\n\npub fn caller() -> u32 {\n    callee(1)\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("ws/tests/it.rs"),
            "fn it_works() {\n    assert_eq!(crate::callee(1), 2);\n}\n",
        )
        .unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("ws")
    }

    /// Run `memnoth` with an isolated home. The verb and its flags are passed as
    /// given, so the test proves the exact command line a user would type.
    fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = Command::new(binary());
        command
            .args(args)
            .env("MEMNOTH_HOME", self.home())
            .env_remove("MEMNOTH_VAULT")
            .env_remove("MEMNOTH_GRAPH")
            .env_remove("MEMNOTH_MCP_BIN")
            .env_remove("MEMNOTH_MCP_TOKEN");
        for (key, value) in env {
            command.env(key, value);
        }
        Run {
            output: command.output().expect("spawn memnoth"),
        }
    }

    fn root_arg(&self) -> String {
        self.root().display().to_string()
    }

    fn init(&self) {
        let run = self.run(&["vault", "init"]);
        assert_eq!(run.code(), 0, "vault init failed: {}", run.text());
    }

    fn index(&self) {
        let root = self.root_arg();
        let run = self.run(&["kg", "index", "--root", &root]);
        assert_eq!(run.code(), 0, "kg index failed: {}", run.text());
    }
}

#[test]
fn version_and_help_exit_zero() {
    let sandbox = Sandbox::new();
    let version = sandbox.run(&["--version"]);
    assert_eq!(version.code(), 0);
    assert!(
        version.text().starts_with("memnoth 0.1.0"),
        "{}",
        version.text()
    );
    let help = sandbox.run(&["--help"]);
    assert_eq!(help.code(), 0);
    for verb in ["vault", "kg", "mcp", "doctor", "session"] {
        assert!(help.text().contains(verb), "help omits {verb}");
    }
}

#[test]
fn vault_init_is_idempotent_and_reports_the_real_path() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let again = sandbox.run(&["vault", "init", "--json"]);
    assert_eq!(again.code(), 0);
    let value = again.json();
    assert_eq!(value["ok"], true);
    assert_eq!(value["command"], "vault.init");
    assert_eq!(value["data"]["namespace"], "default");
    assert!(
        value["data"]["vault"]
            .as_str()
            .unwrap()
            .ends_with("vault.db"),
        "{}",
        value["data"]["vault"]
    );
}

#[test]
fn a_fact_carries_its_date_and_asof_reconstructs_it() {
    let sandbox = Sandbox::new();
    sandbox.init();

    let remember = sandbox.run(&[
        "vault",
        "remember",
        "auth",
        "--kind",
        "decision",
        "--confidence",
        "0.8",
        "--json",
        "httpOnly cookies for sessions",
    ]);
    assert_eq!(remember.code(), 0, "{}", remember.text());
    let remembered = remember.json();
    assert_eq!(remembered["data"]["kind"], "decision");

    // A date before the fact: nothing was believed then. This is the whole
    // point of a temporal store, so it must be an empty answer, not an error.
    let before = sandbox.run(&[
        "vault",
        "asof",
        "--at",
        "2020-01-01",
        "--name",
        "auth",
        "--json",
    ]);
    assert_eq!(before.code(), 0, "{}", before.text());
    assert_eq!(before.json()["data"]["count"], 0);

    let now = sandbox.run(&["vault", "asof", "--name", "auth", "--json"]);
    assert_eq!(now.code(), 0, "{}", now.text());
    let value = now.json();
    assert_eq!(value["data"]["count"], 1);
    assert_eq!(
        value["data"]["facts"][0]["content"],
        "httpOnly cookies for sessions"
    );
    assert_eq!(value["data"]["facts"][0]["kind"], "decision");
}

#[test]
fn an_unknown_time_is_a_usage_failure_with_a_json_error() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let run = sandbox.run(&["vault", "asof", "--at", "last tuesday", "--json"]);
    assert_eq!(run.code(), 1);
    let value = run.json();
    assert_eq!(value["ok"], false);
    assert!(
        value["error"].as_str().unwrap().contains("RFC 3339"),
        "{}",
        value["error"]
    );
}

#[test]
fn kg_indexes_a_fixture_and_impact_finds_the_caller_and_the_test() {
    let sandbox = Sandbox::new();
    sandbox.index();

    let root = sandbox.root_arg();
    let status = sandbox.run(&["kg", "status", "--root", &root, "--json"]);
    assert_eq!(status.code(), 0, "{}", status.text());
    let value = status.json();
    assert_eq!(value["data"]["indexed"], true);
    assert!(value["data"]["files"].as_u64().unwrap() >= 2);
    assert!(value["data"]["symbols"].as_u64().unwrap() >= 2);
    assert_eq!(value["data"]["stale_files"], 0);
    assert_eq!(value["data"]["embed_model"], Value::Null);

    let impact = sandbox.run(&["kg", "impact", "callee", "--root", &root, "--json"]);
    assert_eq!(impact.code(), 0, "{}", impact.text());
    let value = impact.json();
    assert_eq!(value["data"]["resolved"], true);
    let definitions = value["data"]["definitions"].as_array().unwrap();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["path"], "src/lib.rs");
    let paths: Vec<&str> = value["data"]["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
    assert!(paths.contains(&"tests/it.rs"), "{paths:?}");
    let tests: Vec<&str> = value["data"]["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit.as_str().unwrap())
        .collect();
    assert_eq!(tests, vec!["tests/it.rs"]);
}

#[test]
fn an_unknown_symbol_is_reported_as_unresolved_not_as_a_crash() {
    let sandbox = Sandbox::new();
    sandbox.index();
    let root = sandbox.root_arg();
    let run = sandbox.run(&["kg", "impact", "no_such_symbol", "--root", &root, "--json"]);
    assert_eq!(run.code(), 0, "a miss is an answer, not a failure");
    let value = run.json();
    assert_eq!(value["data"]["resolved"], false);
    assert_eq!(value["data"]["definitions"].as_array().unwrap().len(), 0);
    assert!(run.text().contains("\"resolved\": false"));
}

#[test]
fn status_reports_staleness_after_the_working_tree_changes() {
    let sandbox = Sandbox::new();
    sandbox.index();
    std::fs::write(
        sandbox.root().join("src/lib.rs"),
        "pub fn callee(value: u32) -> u32 {\n    value + 2\n}\n",
    )
    .unwrap();
    let root = sandbox.root_arg();
    let run = sandbox.run(&["kg", "status", "--root", &root, "--json"]);
    assert_eq!(run.code(), 0);
    assert_eq!(run.json()["data"]["stale_files"], 1);
}

#[test]
fn doctor_answers_before_anything_is_initialised_and_creates_nothing() {
    let sandbox = Sandbox::new();
    let root = sandbox.root_arg();
    let run = sandbox.run(&["doctor", "--root", &root, "--json"]);
    assert_eq!(run.code(), 0, "{}", run.text());
    let value = run.json();
    assert_eq!(value["data"]["vault"]["initialised"], false);
    assert_eq!(value["data"]["graph"]["indexed"], false);
    assert!(
        !sandbox.home().join("vault.db").exists(),
        "a diagnostic must not create the vault it diagnoses"
    );
}

#[test]
fn mcp_add_without_a_token_reports_that_registration_cannot_authenticate() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["mcp", "add", "--clients", "cursor", "--dry-run", "--json"]);
    assert_eq!(run.code(), 0, "{}", run.text());
    let value = run.json();
    assert_eq!(value["data"]["dry_run"], true);
    assert_eq!(value["data"]["token_used"], false);
    let results = value["data"]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        results[0]["detail"]
            .as_str()
            .unwrap()
            .contains("cannot authenticate"),
        "{}",
        results[0]["detail"]
    );
}

#[test]
fn a_profile_typo_fails_before_a_server_is_started() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["mcp", "doctor", "--profile", "codde", "--json"]);
    // `mcp doctor` has no --profile flag: this must be a usage error, not a
    // silent fallback to `full`.
    assert_ne!(run.code(), 0);
    let serve = sandbox.run(&["mcp", "serve", "--profile", "codde"]);
    assert_ne!(serve.code(), 0);
    assert!(
        String::from_utf8_lossy(&serve.output.stderr).contains("unknown tool profile"),
        "{}",
        String::from_utf8_lossy(&serve.output.stderr)
    );
}

#[test]
fn a_vault_and_a_namespace_cannot_name_two_different_stores() {
    let sandbox = Sandbox::new();
    let other = sandbox.home().join("other.db");
    let run = sandbox.run(&[
        "vault",
        "status",
        "--vault",
        &other.display().to_string(),
        "--namespace",
        "team",
    ]);
    assert_ne!(run.code(), 0);
    assert!(
        String::from_utf8_lossy(&run.output.stderr).contains("two different vaults"),
        "{}",
        String::from_utf8_lossy(&run.output.stderr)
    );
}

#[test]
fn a_namespace_keeps_its_own_vault_beside_the_default() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let run = sandbox.run(&["vault", "init", "--namespace", "team-1", "--json"]);
    assert_eq!(run.code(), 0, "{}", run.text());
    assert!(Path::new(run.json()["data"]["vault"].as_str().unwrap()).ends_with("vault-team-1.db"));
}

//! Focused regression tests for `mcp add`, the one-click client registration.
//!
//! Every run uses a real process with HOME/USERPROFILE/APPDATA/XDG_CONFIG_HOME
//! pointed at a tempdir and an empty PATH. That matters: the Zed path used to
//! consult the ambient `%APPDATA%`, which sent a sandbox caller to the real
//! user's settings file. No config outside the tempdir is read or written, no
//! host CLI is found unless a test installs a fake one, and no token or network
//! is involved.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_memnoth");

struct Sandbox {
    dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        let home = dir.path().join("home");
        let bin = home.join("bin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        // A file that exists but is not executable: `verify_written` picks it as
        // MEMNOTH_MCP_BIN and fails fast, so a test never spawns a real server.
        let fake_server = bin.join("not-a-server");
        std::fs::write(&fake_server, b"not executable\n").unwrap();
        Self {
            dir,
            root,
            home,
            bin,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("--trust-folder")
            .arg("--root")
            .arg(&self.root)
            .args(args)
            .env("MEMNOTH_HOME", self.home.join(".memnoth"))
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", self.home.join("AppData").join("Roaming"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("PATH", &self.bin)
            .env("MEMNOTH_MCP_BIN", self.bin.join("not-a-server"))
            .output()
            .unwrap()
    }

    fn text(output: &Output) -> String {
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text
    }

    fn json(output: &Output) -> serde_json::Value {
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {}", Self::text(output)))
    }

    fn zed_settings(&self) -> PathBuf {
        if cfg!(windows) {
            self.home
                .join("AppData")
                .join("Roaming")
                .join("Zed")
                .join("settings.json")
        } else {
            self.home.join(".config").join("zed").join("settings.json")
        }
    }
}

/// The four clients named on the marketing site must all accept the one-click
/// `--local` path and get a stdio entry with the resolved vault and the
/// read-only flag. No token is involved, so none can leak into a shared config.
#[test]
fn local_registration_writes_all_four_advertised_clients() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[
        "mcp",
        "add",
        "--local",
        "--clients",
        "claude,cursor,codex,zed",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    let value = Sandbox::json(&output);
    let data = &value["data"];
    assert_eq!(data["local"], true);
    assert_eq!(data["url"], "");
    assert_eq!(data["token_used"], false);
    assert_eq!(data["read_only"], true);
    assert_eq!(data["profile"], "memory");

    let results = data["results"].as_array().unwrap();
    assert_eq!(results.len(), 4, "{results:?}");
    for row in results {
        assert_eq!(row["ok"], true, "{}", row["detail"]);
        assert!(
            Path::new(row["path"].as_str().unwrap()).starts_with(sandbox.dir.path()),
            "config path escaped the sandbox: {}",
            row["path"]
        );
    }

    let vault = data["vault"].as_str().unwrap();
    for config in [
        sandbox.home.join(".claude.json"),
        sandbox.home.join(".cursor").join("mcp.json"),
    ] {
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        let entry = &root["mcpServers"]["memnoth"];
        assert!(
            entry["command"].as_str().unwrap().contains("memnoth"),
            "{entry}"
        );
        assert_eq!(entry["args"][0], "mcp");
        assert_eq!(entry["args"][1], "serve");
        assert_eq!(entry["args"][2], "--db");
        assert_eq!(entry["args"][3], vault);
        assert_eq!(entry["env"]["MEMNOTH_MCP_PROFILE"], "memory");
        assert_eq!(entry["env"]["MEMNOTH_MCP_READ_ONLY"], "1");
        assert!(entry.get("url").is_none(), "{entry}");
        assert!(entry.get("headers").is_none(), "{entry}");
    }

    let codex = std::fs::read_to_string(sandbox.home.join(".codex").join("config.toml")).unwrap();
    let codex: toml::Value = toml::from_str(&codex).unwrap();
    let entry = codex
        .get("mcp_servers")
        .and_then(|servers| servers.get("memnoth"))
        .unwrap();
    assert_eq!(
        entry.get("command").and_then(toml::Value::as_str).unwrap(),
        data["command"].as_str().unwrap()
    );
    assert!(entry.get("url").is_none(), "{entry:?}");
    assert_eq!(
        entry
            .get("env")
            .and_then(|env| env.get("MEMNOTH_MCP_READ_ONLY"))
            .and_then(toml::Value::as_str),
        Some("1")
    );

    let zed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sandbox.zed_settings()).unwrap()).unwrap();
    let entry = &zed["context_servers"]["memnoth"];
    assert_eq!(entry["args"][2], "--db");
    assert_eq!(entry["args"][3], vault);
    assert_eq!(entry["env"]["MEMNOTH_MCP_READ_ONLY"], "1");
}

/// `--allow-writes` is an actual `MEMNOTH_MCP_READ_ONLY=0` in the written
/// config, and the human-readable report must stop claiming read-only mode.
#[test]
fn allow_writes_changes_the_config_and_the_report() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[
        "mcp",
        "add",
        "--local",
        "--clients",
        "cursor",
        "--allow-writes",
    ]);
    let text = Sandbox::text(&output);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(
        text.contains("writes     allowed (--allow-writes)"),
        "{text}"
    );
    assert!(!text.contains("writes     read-only"), "{text}");

    let config = std::fs::read_to_string(sandbox.home.join(".cursor").join("mcp.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(
        config["mcpServers"]["memnoth"]["env"]["MEMNOTH_MCP_READ_ONLY"],
        "0"
    );
}

/// A dry run resolves paths but creates neither client configs, the persisted
/// CLI config, nor the team vault directory.
#[test]
fn dry_run_changes_nothing_including_team_directory() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[
        "mcp",
        "add",
        "--local",
        "--clients",
        "cursor,codex,zed",
        "--team",
        "acme",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    let data = &Sandbox::json(&output)["data"];
    assert_eq!(data["dry_run"], true);
    assert_eq!(
        data["vault"].as_str().unwrap(),
        sandbox
            .home
            .join(".memnoth")
            .join("teams")
            .join("acme")
            .join("vault.db")
            .to_string_lossy()
            .as_ref()
    );
    assert!(!sandbox.home.join(".memnoth").join("teams").exists());
    assert!(!sandbox.home.join(".cursor").exists());
    assert!(!sandbox.home.join(".codex").exists());
    assert!(!sandbox.zed_settings().exists());
    assert!(!sandbox.home.join(".memnoth").join("config.toml").exists());

    // The same command without --dry-run creates exactly the team directory.
    let output = sandbox.run(&[
        "mcp",
        "add",
        "--local",
        "--clients",
        "cursor",
        "--team",
        "acme",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    assert!(
        sandbox
            .home
            .join(".memnoth")
            .join("teams")
            .join("acme")
            .is_dir()
    );
}

/// `--clients` accepts exactly the advertised names; a typo fails instead of
/// silently registering nothing.
#[test]
fn unknown_client_name_is_refused() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["mcp", "add", "--local", "--clients", "zen", "--dry-run"]);
    assert_ne!(output.status.code(), Some(0));
    assert!(
        Sandbox::text(&output).contains("unknown client"),
        "{}",
        Sandbox::text(&output)
    );
}

/// When Claude Code owns its config, `mcp add` must ask its CLI to register the
/// stdio server with the read-only environment rather than guessing the shape.
#[test]
fn claude_registration_uses_the_host_cli_with_stdio_arguments() {
    let sandbox = Sandbox::new();
    let log = sandbox.home.join("claude-args.txt");
    install_fake_claude(&sandbox.bin, &log);

    let output = Command::new(BIN)
        .arg("--trust-folder")
        .arg("--root")
        .arg(&sandbox.root)
        .args(["mcp", "add", "--local", "--clients", "claude", "--json"])
        .env("MEMNOTH_HOME", sandbox.home.join(".memnoth"))
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("APPDATA", sandbox.home.join("AppData").join("Roaming"))
        .env("XDG_CONFIG_HOME", sandbox.home.join(".config"))
        .env("PATH", &sandbox.bin)
        .env("MEMNOTH_MCP_BIN", sandbox.bin.join("not-a-server"))
        .env("FAKE_CLAUDE_LOG", &log)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));

    let args = std::fs::read_to_string(&log).unwrap_or_else(|_| {
        panic!(
            "fake claude was not called; output: {}",
            Sandbox::text(&output)
        )
    });
    for expected in [
        "mcp add memnoth --scope user --transport stdio",
        "--env MEMNOTH_MCP_PROFILE=memory",
        "--env MEMNOTH_MCP_READ_ONLY=1",
        "-- ",
        "mcp serve --db ",
    ] {
        assert!(args.contains(expected), "missing {expected:?} in {args:?}");
    }
}

#[cfg(windows)]
fn install_fake_claude(bin: &Path, log: &Path) {
    let script = bin.join("claude.cmd");
    std::fs::write(
        &script,
        format!("@echo off\r\necho %* > \"{}\"\r\n", log.display()),
    )
    .unwrap();
}

#[cfg(not(windows))]
fn install_fake_claude(bin: &Path, log: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = bin.join("claude");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" > \"{}\"\n", log.display()),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
}

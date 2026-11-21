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

    fn write_zed_settings(&self, text: &str) {
        let path = self.zed_settings();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// A JSONC settings file with both comment styles, trailing commas, and a
/// server and a key that are not ours. Every one of those must survive `add`.
const ZED_JSONC: &str = r#"{
  // keep: the user's theme
  "theme": "One Dark",
  "context_servers": {
    /* another server the user owns */
    "other": { "command": "node", "args": ["other.js"], },
  },
}
"#;

/// Test-only JSONC reader: the production mask is exercised through the real
/// binary in the tests below; this just lets the assertions read the result.
fn parse_jsonc(text: &str) -> serde_json::Value {
    let chars: Vec<char> = text.chars().collect();
    let mut stripped = String::new();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < chars.len() {
        let ch = chars[index];
        if in_string {
            stripped.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            stripped.push(ch);
            index += 1;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                index += 1;
            }
            index = (index + 2).min(chars.len());
            stripped.push(' ');
            continue;
        }
        stripped.push(ch);
        index += 1;
    }
    let chars: Vec<char> = stripped.chars().collect();
    let mut clean = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut pending: Option<usize> = None;
    for ch in chars {
        if in_string {
            clean.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                pending = None;
                clean.push(ch);
            }
            ',' => {
                pending = Some(clean.len());
                clean.push(ch);
            }
            '}' | ']' => {
                if let Some(at) = pending.take() {
                    clean.replace_range(at..at + 1, " ");
                }
                clean.push(ch);
            }
            ch if ch.is_whitespace() => clean.push(ch),
            _ => {
                pending = None;
                clean.push(ch);
            }
        }
    }
    serde_json::from_str(&clean).expect("edited Zed settings must still parse as JSONC")
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

/// A commented Zed settings file is now written directly: the comments, the
/// trailing commas, the unrelated key and the other server all survive, and
/// the Command Palette snippet is no longer the only outcome.
#[test]
fn zed_jsonc_is_written_with_comments_and_unrelated_settings_preserved() {
    let sandbox = Sandbox::new();
    sandbox.write_zed_settings(ZED_JSONC);

    let output = sandbox.run(&["mcp", "add", "--local", "--clients", "zed", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    let data = &Sandbox::json(&output)["data"];
    let results = data["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["ok"], true, "{}", results[0]["detail"]);
    assert_eq!(results[0]["registered"], true, "{}", results[0]["detail"]);
    let snippets = data["snippets"].as_array().unwrap();
    assert!(
        !snippets.iter().any(|snippet| snippet
            .as_str()
            .is_some_and(|s| s.contains("Command Palette"))),
        "a written config must not fall back to the snippet: {snippets:?}"
    );

    let written = std::fs::read_to_string(sandbox.zed_settings()).unwrap();
    assert!(written.contains("// keep: the user's theme"), "{written}");
    assert!(
        written.contains("/* another server the user owns */"),
        "{written}"
    );
    let root = parse_jsonc(&written);
    assert_eq!(root["theme"], "One Dark");
    assert_eq!(root["context_servers"]["other"]["command"], "node");
    let entry = &root["context_servers"]["memnoth"];
    assert_eq!(entry["args"][0], "mcp");
    assert_eq!(entry["args"][1], "serve");
    assert_eq!(entry["env"]["MEMNOTH_MCP_READ_ONLY"], "1");
    assert!(entry.get("url").is_none(), "{entry}");
}

/// Re-running `add` updates the same entry in place instead of appending a
/// second one, and leaves an already-correct file byte-identical.
#[test]
fn zed_jsonc_re_add_is_idempotent() {
    let sandbox = Sandbox::new();
    sandbox.write_zed_settings(ZED_JSONC);

    let first = sandbox.run(&["mcp", "add", "--local", "--clients", "zed", "--json"]);
    assert_eq!(first.status.code(), Some(0), "{}", Sandbox::text(&first));
    let once = std::fs::read_to_string(sandbox.zed_settings()).unwrap();

    let second = sandbox.run(&["mcp", "add", "--local", "--clients", "zed", "--json"]);
    assert_eq!(second.status.code(), Some(0), "{}", Sandbox::text(&second));
    let twice = std::fs::read_to_string(sandbox.zed_settings()).unwrap();
    assert_eq!(once, twice, "a second add must not change the file");

    let root = parse_jsonc(&twice);
    let servers = root["context_servers"].as_object().unwrap();
    assert_eq!(servers.keys().filter(|key| *key == "memnoth").count(), 1);
    assert!(twice.contains("// keep: the user's theme"), "{twice}");
}

/// Fields inside an existing memnoth entry that we do not own survive the
/// update, and switching to local mode drops the stale HTTP fields.
#[test]
fn zed_jsonc_updates_an_existing_entry_without_touching_its_neighbours() {
    let sandbox = Sandbox::new();
    sandbox.write_zed_settings(
        "{\n  // top note\n  \"context_servers\": {\n    \"memnoth\": {\n      \"url\": \"https://old.example/mcp\",\n      \"custom_field\": \"keep me\",\n    },\n    \"other\": { \"command\": \"node\" },\n  },\n}\n",
    );

    let output = sandbox.run(&["mcp", "add", "--local", "--clients", "zed", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));

    let written = std::fs::read_to_string(sandbox.zed_settings()).unwrap();
    assert!(written.contains("// top note"), "{written}");
    assert!(written.contains("keep me"), "{written}");
    let root = parse_jsonc(&written);
    let entry = &root["context_servers"]["memnoth"];
    assert_eq!(entry["custom_field"], "keep me");
    assert!(!entry["command"].as_str().unwrap().is_empty());
    assert!(entry.get("url").is_none(), "{entry}");
    assert_eq!(root["context_servers"]["other"]["command"], "node");
}

/// A Zed settings file that is not valid even as JSONC is never rewritten:
/// the command fails and the file is left byte-for-byte untouched.
#[test]
fn zed_malformed_jsonc_fails_closed() {
    let sandbox = Sandbox::new();
    let original = "{\n  // broken\n  \"context_servers\": { \"memnoth\": {\n";
    sandbox.write_zed_settings(original);

    let output = sandbox.run(&["mcp", "add", "--local", "--clients", "zed", "--json"]);
    assert_ne!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    let text = Sandbox::text(&output);
    assert!(text.contains("refusing to rewrite"), "{text}");
    assert_eq!(
        std::fs::read_to_string(sandbox.zed_settings()).unwrap(),
        original
    );
}

/// Fail closed on a shape we cannot safely edit, too, not just broken syntax.
#[test]
fn zed_jsonc_with_a_non_object_context_servers_fails_closed() {
    let sandbox = Sandbox::new();
    let original = "{\n  // note\n  \"context_servers\": [],\n}\n";
    sandbox.write_zed_settings(original);

    let output = sandbox.run(&["mcp", "add", "--local", "--clients", "zed", "--json"]);
    assert_ne!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    let text = Sandbox::text(&output);
    assert!(text.contains("is not an object"), "{text}");
    assert_eq!(
        std::fs::read_to_string(sandbox.zed_settings()).unwrap(),
        original
    );
}

/// `--dry-run` validates the JSONC path but leaves the user's file untouched.
#[test]
fn zed_jsonc_dry_run_leaves_the_file_untouched() {
    let sandbox = Sandbox::new();
    sandbox.write_zed_settings(ZED_JSONC);

    let output = sandbox.run(&[
        "mcp",
        "add",
        "--local",
        "--clients",
        "zed",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    assert_eq!(
        std::fs::read_to_string(sandbox.zed_settings()).unwrap(),
        ZED_JSONC
    );
}

/// `//` and `/* */` inside a string value are data, not comments to strip.
#[test]
fn zed_jsonc_comment_markers_inside_strings_are_data() {
    let sandbox = Sandbox::new();
    let original = "{\n  \"note\": \"http://example.test//path /* still data */\",\n  \"context_servers\": {},\n}\n";
    sandbox.write_zed_settings(original);

    let output = sandbox.run(&["mcp", "add", "--local", "--clients", "zed", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", Sandbox::text(&output));
    let root = parse_jsonc(&std::fs::read_to_string(sandbox.zed_settings()).unwrap());
    assert_eq!(root["note"], "http://example.test//path /* still data */");
    assert!(root["context_servers"]["memnoth"].is_object());
}

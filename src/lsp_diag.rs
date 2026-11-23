//! Diagnostics after an edit: one bounded paragraph, or nothing.
//!
//! A language server starts only when someone opted in (`MEMNOTH_AUTO_LSP`) and
//! the language is on the allowlist, and it is killed before the tool returns:
//! a diagnostic is worth a couple of seconds, not a process left running behind
//! an edit the model already made.
//!
//! Honesty matters more than completeness here. An empty diagnostic list and a
//! server that never answered look identical to the client's poll, so a server
//! that cannot start, rejects the document, or publishes nothing is reported as
//! "not available" instead of being mistaken for a clean file.

use std::path::Path;
use std::time::Duration;

use memnoth_codegraph::lsp::{AutoStartPolicy, ServerSpec, server_for_extension};
use memnoth_lsp::{LspClient, LspClientHandle};

/// How long a server may take to publish before the answer says "not available".
/// Test code shortens this through [`after_write_with`] and a fake server.
const DIAGNOSTIC_WAIT: Duration = Duration::from_secs(2);

/// The hard ceiling on summary rows. The wait is configurable; this is not, so
/// a file with 200 warnings cannot crowd out the edit that caused them.
const MAX_DIAGNOSTICS: usize = 5;

/// The diagnostics paragraph for `path`, when the policy allows a server.
///
/// `None` means LSP is not enabled for this language — there is no server to be
/// unavailable. An enabled server that cannot start, rejects the document, or
/// publishes nothing yields an explicit "not available" line instead of the
/// silent empty that would make a dead server look like a clean file.
#[must_use]
pub fn after_write(root: &Path, path: &Path, text: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?;
    let spec = server_for_extension(extension)?;
    let policy = policy_from_env();
    if !policy.allows(spec.language) {
        return None;
    }
    after_write_with(root, path, text, spec, &policy, DIAGNOSTIC_WAIT)
}

/// The same probe with the wait bound injected, so a fake server can answer in
/// milliseconds. `spec` and `policy` are injected for the same reason: the
/// integration tests in `tests/lsp_diag_integration.rs` point them at a fake
/// server and an allowing policy instead of a real language server.
///
/// The edited `text` is sent verbatim in the `didOpen` notification — full
/// document sync — so a server diagnoses the file the tool just wrote, never an
/// empty or stale buffer.
#[doc(hidden)]
#[must_use]
pub fn after_write_with(
    root: &Path,
    path: &Path,
    text: &str,
    spec: &ServerSpec,
    policy: &AutoStartPolicy,
    wait: Duration,
) -> Option<String> {
    if !policy.allows(spec.language) {
        return None;
    }
    let client = match LspClient::start(spec, root, policy) {
        Ok(Some(client)) => client,
        Ok(None) => {
            return Some("diagnostics: not available (language server was not started)".into());
        }
        Err(err) => return Some(format!("diagnostics: not available ({err})")),
    };
    let uri = uri_for(path);
    let handle = LspClientHandle::new(client.clone());
    let answer = handle
        .did_open(&uri, spec.language, 1, text)
        .map_err(|err| format!("language server did not accept the document: {err}"))
        .and_then(|()| {
            let list = client.wait_for_diagnostics(&uri, wait);
            if list.is_empty() {
                Err("language server did not report diagnostics".to_string())
            } else {
                Ok(list)
            }
        });
    let _ = handle.did_close(&uri);
    client.kill();
    match answer {
        Ok(list) => summarize(path, &list),
        Err(reason) => Some(format!("diagnostics: not available ({reason})")),
    }
}

/// `MEMNOTH_AUTO_LSP=1` opts in; `MEMNOTH_LSP_ALLOW=rust,python` narrows the
/// languages. Absent either, the default policy refuses every start.
fn policy_from_env() -> AutoStartPolicy {
    let enabled = std::env::var("MEMNOTH_AUTO_LSP")
        .is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false" | "no" | "off"));
    if !enabled {
        return AutoStartPolicy::default();
    }
    let allowlist: Vec<String> = std::env::var("MEMNOTH_LSP_ALLOW")
        .unwrap_or_default()
        .split(',')
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    AutoStartPolicy {
        auto_lsp: true,
        allowlist,
    }
}

fn uri_for(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// At most five diagnostics: the model needs the shape of the breakage, not the
/// whole list, and a file with 200 warnings would otherwise crowd out the edit
/// it just made. Every row names the file, the line and the server's code, so a
/// following turn can fix the error without being told where it is.
fn summarize(path: &Path, list: &[serde_json::Value]) -> Option<String> {
    if list.is_empty() {
        return None;
    }
    let errors = list.iter().filter(|item| item["severity"] == 1).count();
    let warnings = list.iter().filter(|item| item["severity"] == 2).count();
    let mut out = format!("diagnostics after this edit: {errors} error(s), {warnings} warning(s)");
    for diagnostic in list.iter().take(MAX_DIAGNOSTICS) {
        let message = diagnostic["message"].as_str().unwrap_or("?");
        let line = diagnostic["range"]["start"]["line"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(1);
        let code = diagnostic_code(diagnostic);
        out.push_str(&format!(
            "\n  {}:{line} [{code}]  {message}",
            path.display()
        ));
    }
    if list.len() > MAX_DIAGNOSTICS {
        out.push_str(&format!("\n  … {} more", list.len() - MAX_DIAGNOSTICS));
    }
    Some(out)
}

/// LSP codes arrive as a string or as a number. Render what the server sent;
/// say `none` when it sent no code rather than inventing one.
fn diagnostic_code(diagnostic: &serde_json::Value) -> String {
    match &diagnostic["code"] {
        serde_json::Value::String(code) => code.clone(),
        serde_json::Value::Number(code) => code.to_string(),
        _ => "none".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_clean_file_returns_no_paragraph_at_all() {
        assert!(summarize(Path::new("a.rs"), &[]).is_none());
    }

    #[test]
    fn the_paragraph_counts_severities_and_stops_at_five_rows() {
        let list: Vec<serde_json::Value> = (0..8)
            .map(|index| {
                json!({
                    "severity": if index == 0 { 1 } else { 2 },
                    "code": format!("E{index:03}"),
                    "message": format!("problem {index}"),
                    "range": { "start": { "line": index, "character": 0 } }
                })
            })
            .collect();
        let text = summarize(Path::new("src/a.rs"), &list).unwrap();
        assert!(
            text.starts_with("diagnostics after this edit: 1 error(s), 7 warning(s)"),
            "{text}"
        );
        assert!(text.contains("src/a.rs:1 [E000]  problem 0"), "{text}");
        assert!(
            !text.contains("problem 5"),
            "only five rows are shown: {text}"
        );
        assert!(text.contains("… 3 more"), "{text}");
        let rows = text
            .lines()
            .filter(|line| line.contains("src/a.rs:"))
            .count();
        assert_eq!(rows, MAX_DIAGNOSTICS, "{text}");
    }

    #[test]
    fn a_numeric_code_and_a_missing_code_are_both_rendered_honestly() {
        let numeric = json!({ "code": 4242 });
        let missing = json!({});
        assert_eq!(diagnostic_code(&numeric), "4242");
        assert_eq!(diagnostic_code(&missing), "none");
        let text = summarize(
            Path::new("a.rs"),
            &[
                json!({
                    "code": 4242,
                    "message": "numeric",
                    "range": { "start": { "line": 0, "character": 0 } }
                }),
                json!({
                    "message": "no code",
                    "range": { "start": { "line": 2, "character": 0 } }
                }),
            ],
        )
        .unwrap();
        assert!(text.contains("a.rs:1 [4242]  numeric"), "{text}");
        assert!(text.contains("a.rs:3 [none]  no code"), "{text}");
    }

    #[test]
    fn a_windows_path_becomes_a_file_uri() {
        assert_eq!(uri_for(Path::new("C:/w/a.rs")), "file:///C:/w/a.rs");
        assert_eq!(uri_for(Path::new("/w/a.rs")), "file:///w/a.rs");
    }

    #[test]
    fn without_an_opt_in_no_language_server_may_start() {
        // The policy is read from the environment; this asserts the shape the
        // gate depends on rather than racing the process environment.
        let refused = AutoStartPolicy::default();
        assert!(!refused.auto_lsp);
        assert!(!refused.allows("rust"));
        let spec = ServerSpec {
            id: "fake",
            language: "rust",
            extensions: &["rs"],
            root_markers: &["Cargo.toml"],
            command: "no-such-server",
            args: &[],
        };
        assert!(
            after_write_with(
                Path::new("."),
                Path::new("a.rs"),
                "x",
                &spec,
                &refused,
                Duration::from_millis(1)
            )
            .is_none()
        );
    }
}

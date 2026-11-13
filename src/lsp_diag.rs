//! Diagnostics after an edit: one bounded paragraph, or nothing.
//!
//! A language server starts only when someone opted in (`MEMNOTH_AUTO_LSP`) and
//! the language is on the allowlist, and it is killed before the tool returns:
//! a diagnostic is worth a couple of seconds, not a process left running behind
//! an edit the model already made.

use std::path::Path;
use std::time::Duration;

use memnoth_codegraph::lsp::{AutoStartPolicy, ServerSpec, server_for_extension};
use memnoth_lsp::{LspClient, LspClientHandle};

/// How long a server may take to publish before the answer says nothing.
const DIAGNOSTIC_WAIT: Duration = Duration::from_secs(2);

/// How many diagnostics are worth the model's context.
const MAX_DIAGNOSTICS: usize = 5;

/// The diagnostics paragraph for `path`, when the policy allows a server.
#[must_use]
pub fn after_write(root: &Path, path: &Path, text: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?;
    let spec: &ServerSpec = server_for_extension(extension)?;
    let policy = policy_from_env();
    let client = LspClient::start(spec, root, &policy).ok()??;
    let uri = uri_for(path);
    let handle = LspClientHandle::new(client.clone());
    let list = match handle.did_open(&uri, spec.language, 1, text) {
        Ok(()) => client.wait_for_diagnostics(&uri, DIAGNOSTIC_WAIT),
        Err(_) => Vec::new(),
    };
    let _ = handle.did_close(&uri);
    client.kill();
    summarize(path, &list)
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
/// it just made.
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
        out.push_str(&format!("\n  {}:{line}  {message}", path.display()));
    }
    if list.len() > MAX_DIAGNOSTICS {
        out.push_str(&format!("\n  … {} more", list.len() - MAX_DIAGNOSTICS));
    }
    Some(out)
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
        assert!(text.contains("src/a.rs:1  problem 0"), "{text}");
        assert!(
            !text.contains("problem 5"),
            "only five rows are shown: {text}"
        );
        assert!(text.contains("… 3 more"), "{text}");
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
    }
}

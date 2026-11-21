//! End-to-end coverage of the evidence MCP surface through the shipped CLI.
//!
//! The unit tests in `memnoth-mcp` exercise the tool bodies directly. This file
//! drives the same dispatcher the way a harness does: `mn mcp call` spawns the
//! standalone server under a profile, so it also proves the profile gate and
//! the read-only promise. It deliberately uses the public CLI rather than
//! reaching into either crate, so no new dependency edge is needed here.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn mn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_memnoth"))
}

/// `mn mcp call` needs a server binary. Find the sibling built by the default
/// members; a checkout that has not built it yet gets a clear skip rather than
/// a misleading failure.
fn server_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MEMNOTH_MCP_BIN")
        && !path.trim().is_empty()
    {
        return Some(PathBuf::from(path));
    }
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_memnoth"));
    let dir = exe.parent()?;
    for name in ["memnoth-mcp", "memnoth-mcp.exe"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn temp_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "memnoth-evidence-mcp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create temporary vault root");
    root
}

fn call(server: &Path, root: &Path, profile: &str, args: &str) -> Output {
    let mut command = mn();
    command
        .env("MEMNOTH_MCP_BIN", server)
        .env("MEMNOTH_ROOT", root)
        .args(["--root"])
        .arg(root)
        .args(["mcp", "call", "evidence_receipt_verify", "--args"])
        .arg(args)
        .args(["--profile", profile])
        .output()
        .expect("run mn mcp call")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn receipt_verify_is_reachable_in_code_and_gated_out_of_memory() {
    let Some(server) = server_binary() else {
        eprintln!("skip: build crates/memnoth-mcp to exercise the MCP wire");
        return;
    };
    let root = temp_root();
    let receipt = r#"{
        "schema":"mn-receipt/1",
        "change":{"commit":"abc123","patch_sha256":"0000000000000000000000000000000000000000000000000000000000000000","files":[]},
        "authority":{"approvals":[]},
        "beliefs":[]
    }"#;
    let args = format!("{{\"commit\":\"abc123\",\"receipt\":{receipt}}}");

    // The default memory profile cannot reach it: receipts describe code
    // changes, and memory holds at its 22-tool ceiling.
    let refused = call(&server, &root, "memory", &args);
    assert!(
        !refused.status.success(),
        "memory profile must refuse evidence_receipt_verify"
    );
    assert!(
        combined(&refused).contains("not available in the 'memory' tool profile"),
        "the refusal must name the profile gate: {}",
        combined(&refused)
    );

    // The code profile reaches it. A caller-supplied document means this check
    // needs no repository, so the assertions hold on any machine.
    let ok = call(&server, &root, "code", &args);
    assert!(
        ok.status.success(),
        "code profile must run the read-only verifier: {}",
        combined(&ok)
    );
    let text = combined(&ok);
    for check in [
        "schema",
        "commit",
        "commit-exists",
        "patch",
        "files",
        "approvals-attributed",
        "beliefs",
    ] {
        assert!(
            text.contains(check),
            "every check must be reported: {check}"
        );
    }
    assert!(
        text.contains("NOT an authenticity attestation"),
        "the claims fence must travel with the answer"
    );
    assert!(
        !text.to_ascii_lowercase().contains("signed"),
        "the tool makes no signing claim"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn receipt_verify_stays_callable_when_the_server_is_read_only() {
    let Some(server) = server_binary() else {
        eprintln!("skip: build crates/memnoth-mcp to exercise the MCP wire");
        return;
    };
    let root = temp_root();
    let args = r#"{"commit":"abc123","receipt":{"schema":"mn-receipt/1","change":{"commit":"abc123","patch_sha256":"00","files":[]},"authority":{"approvals":[]},"beliefs":null,"beliefs_unavailable":"vault unavailable"}}"#;

    let mut command = mn();
    let output = command
        .env("MEMNOTH_MCP_BIN", server)
        .env("MEMNOTH_ROOT", &root)
        .env("MEMNOTH_MCP_READ_ONLY", "1")
        .args(["--root"])
        .arg(&root)
        .args(["mcp", "call", "evidence_receipt_verify", "--args"])
        .arg(args)
        .args(["--profile", "full"])
        .output()
        .expect("run read-only mcp call");

    assert!(
        output.status.success(),
        "a read-only tool must survive MEMNOTH_MCP_READ_ONLY=1: {}",
        combined(&output)
    );
    assert!(
        combined(&output).contains("not measured"),
        "an unmeasured beliefs field stays named rather than filled"
    );
    let _ = std::fs::remove_dir_all(&root);
}

//! Applying a [`BackendPlan`] on macOS: one `sandbox-exec` profile.
//!
//! Seatbelt is the same mechanism `sandbox-exec` has used since forever, and it
//! is deprecated in the sense that Apple does not document it — not in the sense
//! that it does nothing. `(deny default)` plus explicit allows *is* a kernel
//! boundary: the profile is compiled by the kernel's sandbox extension, and a
//! process outside its rules gets `EPERM`, not advice.
//!
//! ## What this backend does and does not do
//!
//! Enforced:
//!
//! - **default deny.** Anything the profile does not name is refused, so a
//!   command cannot reach a path, a socket or a device the plan did not allow;
//! - the filesystem is readable for the same reason it is on Linux — a compiler
//!   needs headers and toolchains — and writable only under the plan's
//!   `read_write` roots, which for every built-in profile includes the
//!   workspace;
//! - every deny path is re-denied *after* the allows, and Seatbelt's last
//!   matching rule wins, so a deny inside a writable root still hides;
//! - `restrict_network` becomes `(deny network*)`: no sockets, no DNS;
//! - the child cannot outlive the parent: Seatbelt has no job object, so this
//!   is the one property that comes from the caller (`Command` + `kill`), not
//!   from the profile, and it is stated rather than implied.
//!
//! **Not** enforced: syscall filtering (Seatbelt is not seccomp), and anything
//! an entitlement would grant — a helper that already holds a mach service can
//! use it if the profile allows `mach-lookup`, which it does, because a compiler
//! needs `dyld` services to start at all.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::backend::{BackendPlan, OsBackend};
use crate::confine::Confinement;

/// The `sandbox-exec` argv for one plan. Pure, so every platform can test it.
///
/// The profile is passed with `-p`, which keeps it out of the filesystem: a
/// temporary profile file would be a path an unconfined process could rewrite
/// between the write and the exec.
pub fn seatbelt_args(
    plan: &BackendPlan,
    workspace: &Path,
    argv: &[String],
) -> anyhow::Result<Vec<String>> {
    let profile = profile(plan, workspace)?;
    let mut args = vec!["-p".to_string(), profile, "--".to_string()];
    args.extend(argv.iter().cloned());
    Ok(args)
}

/// The SBPL text for one plan.
///
/// Order matters: `(deny default)` first, allows next, denies last. Seatbelt
/// evaluates every rule and the last one that matches decides, which is what
/// makes "allow the workspace, then hide `workspace/.env`" expressible.
pub fn profile(plan: &BackendPlan, workspace: &Path) -> anyhow::Result<String> {
    if !posix_absolute(workspace) {
        anyhow::bail!(
            "the workspace must be an absolute path to be named in a sandbox profile: {}",
            workspace.display()
        );
    }

    let mut text = String::from("(version 1)\n(deny default)\n");

    // Starting a process at all: exec, fork, and signals to our own children.
    text.push_str("(allow process-exec* process-fork signal)\n");
    // `sandbox-exec` itself needs to read its own bootstrapping services.
    text.push_str("(allow sysctl-read mach-lookup file-read-metadata)\n");
    // A toolchain reads far more than the workspace: SDK headers, cargo's
    // registry, the dynamic loader. Reads are allowed for the same reason the
    // Linux backend mounts `/` read-only — the boundary that matters here is
    // what can be *changed* and what can be *reached*.
    text.push_str("(allow file-read*)\n");

    for root in writable_roots(plan, workspace) {
        text.push_str(&format!("(allow file-write* (subpath {}))\n", quote(&root)));
    }

    if plan.restrict_network {
        text.push_str("(deny network*)\n");
    } else {
        text.push_str("(allow network*)\n");
    }

    // Deny last, so it wins over every allow above it.
    for path in deny_paths(plan) {
        if !posix_absolute(&path) {
            anyhow::bail!(
                "a deny path in profile '{}' is not absolute: {}",
                plan.profile,
                path.display()
            );
        }
        let quoted = quote(&path.display().to_string());
        text.push_str(&format!(
            "(deny file-read* file-write* (subpath {quoted}))\n"
        ));
    }

    Ok(text)
}

/// Whether a path is absolute in the sense a profile cares about.
///
/// `Path::is_absolute` answers a different question on Windows (`C:\\w` yes,
/// `/w` no), and a profile is generated for the machine it will run on, not for
/// the machine that compiled the test.
fn posix_absolute(path: &Path) -> bool {
    path.to_string_lossy().starts_with('/') || path.is_absolute()
}

/// Where writes are allowed: the plan's roots, and the workspace when the plan
/// forgot it. `cargo build` failing for want of a writable target directory is
/// not a boundary, it is a bug report.
fn writable_roots(plan: &BackendPlan, workspace: &Path) -> Vec<String> {
    let mut roots: Vec<String> = plan
        .read_write
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    let workspace = workspace.display().to_string();
    if !roots.iter().any(|root| root == &workspace) {
        roots.push(workspace);
    }
    roots
}

fn deny_paths(plan: &BackendPlan) -> Vec<std::path::PathBuf> {
    plan.deny_exact
        .iter()
        .cloned()
        .chain(plan.deny_expanded.iter().map(std::path::PathBuf::from))
        .collect()
}

/// An SBPL string literal.
///
/// A path with a quote or a backslash in it is a path the profile would
/// otherwise mis-parse, and a mis-parsed deny is a deny that does not apply.
fn quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Apply `plan` to `argv` with Seatbelt, or explain why it cannot be applied.
pub fn confine(plan: &BackendPlan, workspace: &Path, argv: &[String]) -> Confinement {
    if plan.profile == "off" {
        return Confinement::Unavailable {
            reason: "the sandbox policy is off, so nothing confines this command".to_string(),
        };
    }
    if plan.backend != OsBackend::MacOs {
        return Confinement::Unavailable {
            reason: format!(
                "Seatbelt applies the macOS plan; this machine is {}",
                plan.backend.as_str()
            ),
        };
    }
    if argv.is_empty() {
        return Confinement::Unavailable {
            reason: "no command to confine".to_string(),
        };
    }
    if let Err(reason) = probe() {
        return Confinement::Unavailable { reason };
    }
    match seatbelt_args(plan, workspace, argv) {
        Ok(args) => Confinement::Seatbelt {
            program: "sandbox-exec".to_string(),
            args,
        },
        Err(error) => Confinement::Unavailable {
            reason: format!("{error:#}"),
        },
    }
}

/// Whether `sandbox-exec` can actually start a confined process here, once.
///
/// Presence on `PATH` is not the same as usable: a profile that does not
/// compile, or a system where the sandbox extension has been withdrawn, fails
/// at exec time. Proving it with a real run is the difference between a report
/// and a wish.
fn probe() -> Result<(), String> {
    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE
        .get_or_init(|| {
            if !cfg!(target_os = "macos") {
                return Err(
                    "this build applies the macOS plan with sandbox-exec, and this machine is not \
                     macOS"
                        .to_string(),
                );
            }
            match Command::new("sandbox-exec")
                .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
            {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(format!(
                    "sandbox-exec cannot start a profile here ({})",
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
                Err(error) => Err(format!("sandbox-exec could not be executed: {error}")),
            }
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendPlan;
    use std::path::PathBuf;

    fn plan(workspace: &str) -> BackendPlan {
        BackendPlan {
            backend: OsBackend::MacOs,
            enforcement: crate::backend::EnforcementKind::Seatbelt,
            profile: "workspace".to_string(),
            read_only: vec![PathBuf::from("/")],
            read_write: vec![PathBuf::from(workspace)],
            deny_exact: vec![PathBuf::from("/Users/someone/.ssh")],
            deny_expanded: Vec::new(),
            deny_globs: Vec::new(),
            default_read: true,
            restrict_network: true,
        }
    }

    #[test]
    fn the_profile_denies_by_default_and_allows_last() {
        let profile = profile(
            &plan("/Users/someone/code"),
            Path::new("/Users/someone/code"),
        )
        .expect("an absolute workspace compiles");
        let deny_default = profile.find("(deny default)").expect("default deny");
        let write_allow = profile
            .find("(allow file-write* (subpath \"/Users/someone/code\"))")
            .expect("the workspace is writable");
        let deny_ssh = profile
            .find("(deny file-read* file-write* (subpath \"/Users/someone/.ssh\"))")
            .expect("the deny path is re-denied");
        assert!(deny_default < write_allow);
        // Last match wins in Seatbelt, so a deny has to come after the allow it
        // overrides.
        assert!(write_allow < deny_ssh);
    }

    #[test]
    fn the_network_is_denied_only_when_the_profile_says_so() {
        let mut open = plan("/w");
        open.restrict_network = false;
        let text = profile(&open, Path::new("/w")).unwrap();
        assert!(text.contains("(allow network*)"));
        assert!(!text.contains("(deny network*)"));

        let text = profile(&plan("/w"), Path::new("/w")).unwrap();
        assert!(text.contains("(deny network*)"));
    }

    #[test]
    fn the_workspace_is_writable_even_if_the_plan_forgot_it() {
        let mut forgotten = plan("/w");
        forgotten.read_write.clear();
        let text = profile(&forgotten, Path::new("/w")).unwrap();
        assert!(text.contains("(allow file-write* (subpath \"/w\"))"));
    }

    #[test]
    fn a_quote_in_a_path_cannot_break_the_profile() {
        let mut hostile = plan("/w");
        hostile.deny_exact = vec![PathBuf::from("/tmp/a\") (allow file-write*")];
        let text = profile(&hostile, Path::new("/w")).unwrap();
        // The injected text is inside the literal, not a rule of its own.
        assert!(!text.contains("(allow file-write*)\n"));
        assert!(text.contains("\\\""));
    }

    #[test]
    fn a_relative_workspace_is_refused_rather_than_guessed() {
        assert!(profile(&plan("w"), Path::new("w")).is_err());
    }

    #[test]
    fn the_argv_carries_the_profile_inline() {
        let args = seatbelt_args(
            &plan("/w"),
            Path::new("/w"),
            &["sh".to_string(), "-c".to_string(), "true".to_string()],
        )
        .unwrap();
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("(version 1)"));
        assert_eq!(&args[2..], &["--", "sh", "-c", "true"]);
    }

    #[test]
    fn the_off_profile_is_never_dressed_up_as_a_boundary() {
        let mut off = plan("/w");
        off.profile = "off".to_string();
        let confinement = confine(&off, Path::new("/w"), &["true".to_string()]);
        assert!(!confinement.is_enforced());
        assert!(confinement.describe().contains("policy is off"));
    }

    #[test]
    fn another_platform_gets_the_reason_not_a_seatbelt_shell() {
        let mut linux = plan("/w");
        linux.backend = OsBackend::Linux;
        let confinement = confine(&linux, Path::new("/w"), &["true".to_string()]);
        assert!(!confinement.is_enforced());
        assert!(confinement.describe().contains("this machine is"));
    }
}

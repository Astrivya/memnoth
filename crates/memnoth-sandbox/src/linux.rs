//! Applying a [`BackendPlan`] on Linux: one `bwrap` argv.
//!
//! The plan is data on every OS; this module is the single place it becomes a
//! process boundary. The split matters because a plan that nobody applies is
//! the exact failure this crate was warned about — "policy only" printed in the
//! same voice as "enforced".
//!
//! ## What this backend does and does not do
//!
//! Enforced, because `bwrap` and a user namespace enforce them:
//!
//! - the filesystem is read-only everywhere except the plan's `read_write`
//!   roots, which are bind-mounted back over the read-only view;
//! - every deny path is masked with an empty `tmpfs`, so a denied secret is not
//!   merely discouraged, it is not there;
//! - `restrict_network` becomes `--unshare-net`: no interfaces, no egress;
//! - the child dies with us (`--die-with-parent`) and starts no session, so a
//!   backgrounded grandchild cannot outlive the turn that spawned it.
//!
//! **Not** applied by this function: syscall filtering. There is no Landlock
//! layer here, and the seccomp filter is *built* by [`crate::seccomp`] and can be
//! installed by [`install_seccomp_in_child`] in a `pre_exec` hook — but installing
//! it needs a child that has forked and not yet exec'd, and this module returns
//! an argv instead of forking. Until a caller runs that hook, the filter is a
//! plan: `doctor` therefore describes the filesystem view as enforced and names
//! seccomp as a separate, uninstalled layer.
//!
//! ## Why the probe
//!
//! `bwrap` present on `PATH` is not the same as `bwrap` able to start: Ubuntu
//! 24.04 ships an AppArmor profile that refuses unprivileged user namespaces to
//! unconfined binaries. A `<binary> --version` check would report "enforced" on
//! a machine where every confined command fails, so the probe *starts a
//! namespace* and believes only the exit status.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::backend::{BackendPlan, OsBackend};

// The type is shared with the Seatbelt backend, and lived here when Linux was
// the only one. It is re-exported under the old path because four call sites
// name it that way and a rename would be a wider diff than the change.
pub use crate::confine::Confinement;

/// Apply `plan` to `argv`, or explain why it cannot be applied here.
///
/// `argv` is a program and its arguments — not a shell string. `!cmd` passes the
/// person's line to `sh -c`, which is a two-element argv; `session exec` passes
/// the argv it was given, unwrapped. Neither is re-quoted by this function, so
/// what runs is what the plan decided on.
pub fn confine(plan: &BackendPlan, workspace: &Path, argv: &[String]) -> Confinement {
    if plan.profile == "off" {
        return Confinement::Unavailable {
            reason: "the sandbox policy is off, so nothing confines this command".to_string(),
        };
    }
    if plan.backend != OsBackend::Linux {
        return Confinement::Unavailable {
            reason: format!(
                "this build applies the {} plan with bwrap on Linux only; \
                 {} has the policy engine, not a process boundary",
                plan.backend.as_str(),
                std::env::consts::OS
            ),
        };
    }
    if argv.is_empty() {
        return Confinement::Unavailable {
            reason: "no command to confine".to_string(),
        };
    }
    match probe() {
        Ok(()) => {}
        Err(reason) => return Confinement::Unavailable { reason },
    }
    match bwrap_args(plan, workspace, argv) {
        Ok(args) => Confinement::Bwrap {
            program: "bwrap".to_string(),
            args,
        },
        Err(error) => Confinement::Unavailable {
            reason: format!("{error:#}"),
        },
    }
}

/// Install the sandbox seccomp filter in the calling process.
///
/// Call this from a `pre_exec` hook: it must run in the child after `fork` and
/// before `execve`, which is the one point this crate cannot own because it does
/// not spawn the child. Installing it anywhere else filters the wrong process.
///
/// On a target with no syscall table the install is refused, and the caller must
/// treat that as "no syscall layer" rather than as a successful no-op.
pub fn install_seccomp_in_child() -> anyhow::Result<()> {
    crate::seccomp::install(&crate::seccomp::Filter::sandbox())
}

/// Whether `bwrap` can start a namespace here, for `doctor`.
pub fn probe_available() -> Result<(), String> {
    probe()
}

/// The `bwrap` argv for one plan. Pure, so every platform can test it.
///
/// Order is load-bearing and reads bottom-up: the read-only root first, an
/// empty `/tmp`, then the plan's writable roots bind-mounted back over it, then
/// the deny paths masked last so a deny inside the workspace still hides. A
/// deny entry that cannot be masked (missing, unrepresentable) is an error, not
/// a skipped line: a plan that promised to hide a file and did not is worse
/// than no plan.
pub fn bwrap_args(
    plan: &BackendPlan,
    workspace: &Path,
    argv: &[String],
) -> anyhow::Result<Vec<String>> {
    if !posix_absolute(workspace) {
        anyhow::bail!(
            "the workspace must be an absolute path to be bound into the sandbox: {}",
            workspace.display()
        );
    }
    let mut args: Vec<String> = vec![
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        // `/tmp` is shared by every process on the machine: an empty one inside
        // the namespace keeps a confined command from reading another
        // process's scratch files.
        "--tmpfs".into(),
        "/tmp".into(),
    ];

    // A non-empty `read_write` list is the profile's own statement of where
    // writes land; the workspace is one of them for every built-in profile, and
    // the chdir target has to be writable or `cargo build` fails for a reason
    // nobody can read.
    for root in plan.read_write.iter() {
        if !posix_absolute(root) {
            anyhow::bail!(
                "a writable root in profile '{}' is not absolute: {}",
                plan.profile,
                root.display()
            );
        }
        args.push("--bind".into());
        args.push(root.display().to_string());
        args.push(root.display().to_string());
    }
    if !plan.read_write.iter().any(|root| root == workspace) {
        args.push("--bind".into());
        args.push(workspace.display().to_string());
        args.push(workspace.display().to_string());
    }

    args.push("--chdir".into());
    args.push(workspace.display().to_string());

    if plan.restrict_network {
        args.push("--unshare-net".into());
    }

    for deny in plan.deny_exact.iter() {
        mask_deny(deny, &mut args)?;
    }
    for expanded in plan.deny_expanded.iter() {
        mask_deny(Path::new(expanded), &mut args)?;
    }

    args.push("--die-with-parent".into());
    args.push("--new-session".into());
    args.push("--".into());
    args.extend(argv.iter().cloned());
    Ok(args)
}

/// Mask one deny path with an empty tmpfs.
///
/// A path that does not exist is skipped rather than fatal: there is nothing at
/// it to read, a profile like `deny = ["~/.ssh"]` is normal on a machine that
/// has never connected to anything, and `bwrap` would refuse the whole argv for
/// a `--tmpfs` that has no mountpoint. The policy layer still answers `Deny`
/// for that path, so the skip costs no protection — it only stops a missing
/// file from turning into a command that cannot run.
///
/// A path that is not absolute *is* fatal: it cannot be masked, cannot be
/// checked by `decide`, and silently dropping it would leave a plan claiming
/// coverage it does not have.
fn mask_deny(path: &Path, args: &mut Vec<String>) -> anyhow::Result<()> {
    let display = path.display().to_string();
    anyhow::ensure!(
        posix_absolute(path),
        "a deny path in the plan is not absolute: {display}"
    );
    if !path.exists() {
        return Ok(());
    }
    args.push("--tmpfs".into());
    args.push(display);
    Ok(())
}

/// Whether a confined process can actually be started here, probed once.
fn probe() -> Result<(), String> {
    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE
        .get_or_init(|| {
            if which("bwrap").is_none() {
                return Err(
                    "bwrap is not installed (install bubblewrap, or run `mn doctor` to see the \
                     backend this machine has)"
                        .to_string(),
                );
            }
            // A real namespace, not `--version`: the failure this catches is a
            // policy that refuses namespace creation to this binary.
            match Command::new("bwrap")
                .args(["--ro-bind", "/", "/", "--dev", "/dev", "--", "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
            {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(format!(
                    "bwrap is installed but cannot start a namespace here ({})",
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
                Err(error) => Err(format!("bwrap could not be executed: {error}")),
            }
        })
        .clone()
}

/// Absolute in the sense `bwrap` means: rooted at `/`, which is the only kind of
/// path a Linux namespace can be given.
///
/// Deliberately not `Path::is_absolute`: compiling these tests on Windows would
/// otherwise reject `/repo` for lacking a drive letter, and the plan being
/// checked is a *Linux* plan wherever it is built.
fn posix_absolute(path: &Path) -> bool {
    path.to_string_lossy().starts_with('/')
}

/// `which`, without a dependency: the first `bwrap` on `PATH` that is a file.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(restrict_network: bool, deny: Vec<PathBuf>) -> BackendPlan {
        BackendPlan {
            backend: OsBackend::Linux,
            enforcement: crate::backend::EnforcementKind::PlanLandlockBwrap,
            profile: "workspace".to_string(),
            read_only: vec![],
            read_write: vec![PathBuf::from("/repo")],
            deny_exact: deny,
            deny_expanded: vec![],
            deny_globs: vec![],
            default_read: true,
            restrict_network,
            network: if restrict_network {
                crate::backend::NetworkPolicy::Off
            } else {
                crate::backend::NetworkPolicy::Open
            },
        }
    }

    fn argv(command: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), command.into()]
    }

    #[test]
    fn the_filesystem_is_read_only_with_the_workspace_bound_back() {
        let args = bwrap_args(&plan(false, vec![]), Path::new("/repo"), &argv("ls")).unwrap();
        let joined = args.join(" ");
        assert!(joined.starts_with("--ro-bind / /"), "{joined}");
        assert!(joined.contains("--bind /repo /repo"), "{joined}");
        assert!(joined.contains("--chdir /repo"), "{joined}");
        // The bind comes after the empty /tmp, so a workspace under /tmp is not
        // swallowed by it.
        let tmpfs = args.iter().position(|a| a == "/tmp").unwrap();
        let bind = args.windows(3).position(|w| w[0] == "--bind").unwrap();
        assert!(bind > tmpfs, "{joined}");
        // The command is last, after the separator.
        assert_eq!(args[args.len() - 3..], argv("ls")[..]);
        assert!(args.iter().any(|a| a == "--die-with-parent"));
    }

    #[test]
    fn network_is_shared_only_when_the_profile_allows_it() {
        let sealed = bwrap_args(&plan(true, vec![]), Path::new("/repo"), &argv("curl")).unwrap();
        assert!(sealed.iter().any(|a| a == "--unshare-net"));
        let open = bwrap_args(&plan(false, vec![]), Path::new("/repo"), &argv("curl")).unwrap();
        assert!(!open.iter().any(|a| a == "--unshare-net"));
    }

    #[cfg(unix)]
    #[test]
    fn a_deny_path_is_masked_with_an_empty_tmpfs_not_left_readable() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join(".env");
        std::fs::write(&secret, "TOKEN=1").unwrap();
        let args = bwrap_args(
            &plan(false, vec![secret.clone()]),
            Path::new("/repo"),
            &argv("cat"),
        )
        .unwrap();
        let masked = args
            .windows(2)
            .any(|w| w[0] == "--tmpfs" && w[1] == secret.display().to_string());
        assert!(masked, "{args:?}");
    }

    #[test]
    fn a_deny_path_that_does_not_exist_is_skipped_rather_than_fatal() {
        // `deny = ["~/.ssh"]` is a normal profile on a machine that never had
        // one; a missing mountpoint would make bwrap refuse every command.
        let args = bwrap_args(
            &plan(false, vec![PathBuf::from("/definitely/not/here")]),
            Path::new("/repo"),
            &argv("cat"),
        )
        .unwrap();
        assert!(
            !args
                .windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == "/definitely/not/here")
        );
    }

    #[test]
    fn a_deny_path_that_is_not_absolute_refuses_the_argv() {
        let error = bwrap_args(
            &plan(false, vec![PathBuf::from(".env")]),
            Path::new("/repo"),
            &argv("cat"),
        )
        .unwrap_err();
        assert!(error.to_string().contains(".env"), "{error:#}");
    }

    #[test]
    fn a_workspace_that_is_not_absolute_is_refused() {
        let error = bwrap_args(&plan(false, vec![]), Path::new("repo"), &argv("ls")).unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error:#}");
    }

    #[test]
    fn a_plan_that_is_off_confines_nothing_and_says_so() {
        let mut plan = plan(false, vec![]);
        plan.profile = "off".to_string();
        let confinement = confine(&plan, Path::new("/repo"), &argv("ls"));
        assert!(!confinement.is_enforced());
        assert!(
            confinement.describe().contains("not confined"),
            "{}",
            confinement.describe()
        );
    }

    #[test]
    fn a_mac_or_windows_plan_is_named_rather_than_claimed() {
        let mut plan = plan(false, vec![]);
        plan.backend = OsBackend::MacOs;
        let confinement = confine(&plan, Path::new("/repo"), &argv("ls"));
        assert!(!confinement.is_enforced());
        assert!(
            confinement.describe().contains("Linux only"),
            "{}",
            confinement.describe()
        );
    }
}

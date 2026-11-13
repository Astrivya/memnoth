//! `mn sandbox` — test and configure the enforced sandbox.
//!
//! Two subcommands:
//!
//! - `mn sandbox test <cmd>` — predict what would happen to `<cmd>` under the
//!   sandbox policy on this machine: rule decision, enforcement mode, fail-closed
//!   verdict, and the same status rows `doctor` prints.
//! - `mn sandbox setup` — install the elevated sandbox helper (Windows only;
//!   D15). On other platforms this reports that no setup is needed.

use anyhow::Context;

use memnoth_sandbox::{
    backend::{BackendPlan, OsBackend},
    profiles::{ProfileName, SandboxConfig},
    rules::RuleSet,
    selftest::sandbox_test,
    status::status,
};

use crate::output::Outcome;

/// Run the `mn sandbox` verb.
pub fn run(ctx: &crate::AppContext, args: &crate::cli::SandboxArgs) -> anyhow::Result<Outcome> {
    match &args.command {
        crate::cli::SandboxCommand::Test { command } => run_test(ctx, command.as_str()),
        crate::cli::SandboxCommand::Setup => run_setup(ctx),
    }
}

/// `mn sandbox test <cmd>`: run `selftest::sandbox_test` and render the report.
fn run_test(ctx: &crate::AppContext, command: &str) -> anyhow::Result<Outcome> {
    // Load the sandbox policy for this workspace (same loader as `!cmd` and
    // `session exec`, so all three entry points agree).
    let config = memnoth_sandbox::profiles::load_sandbox_config(&ctx.root)
        .with_context(|| format!("cannot read sandbox policy in {}", ctx.root.display()))?;

    // Build the plan for the compiling host, same as `shell_plan`/`exec`.
    let plan = BackendPlan::build(
        OsBackend::host(),
        &ProfileName::Workspace,
        &ctx.root,
        &config,
    )
    .with_context(|| "cannot build sandbox plan")?;

    // Load user rules from ~/.memnoth/rules.txt; an unparseable file is a
    // refusal, not an empty policy.
    let rules_path = memnoth_sandbox::paths::memnoth_home().join("rules.txt");
    let user_rules = match std::fs::read_to_string(&rules_path) {
        Ok(text) => memnoth_sandbox::rules::parse_rules(&text)
            .with_context(|| format!("{}: unparseable rule file", rules_path.display()))?,
        Err(_) => Vec::new(),
    };
    let rule_set = RuleSet::with_user_rules(user_rules);

    // The same prediction the tool layer uses.
    let report = sandbox_test(&plan, &rule_set, &ctx.root, command);

    let data = serde_json::json!({
        "command": report.command,
        "decision": format!("{:?}", report.decision),
        "allowed": report.allowed(),
        "refused": report.refused(),
        "refusal": report.refusal(),
        "summary": report.summary(),
        "enforcement": report.enforcement,
        "execution": format!("{:?}", report.execution),
        "status": report.status.to_json(),
        "notes": report.notes,
    });

    Ok(Outcome::new("sandbox.test", report.summary(), data))
}

/// `mn sandbox setup`: install the elevated sandbox helper.
///
/// Windows (D15): write the helper marker and report what was installed.
/// Other platforms: report that no elevated helper exists yet.
fn run_setup(ctx: &crate::AppContext) -> anyhow::Result<Outcome> {
    if cfg!(target_os = "windows") {
        let marker_path = memnoth_sandbox::windows::helper_marker_path();
        let state = memnoth_sandbox::windows::helper_state();

        match &state {
            memnoth_sandbox::windows::HelperState::Installed { .. } => {
                let text = format!(
                    "sandbox helper already installed at {}\n{}\n",
                    marker_path.display(),
                    state.label()
                );
                let data = serde_json::json!({
                    "installed": true,
                    "marker_path": marker_path.display().to_string(),
                    "state": state.label(),
                });
                return Ok(Outcome::new("sandbox.setup", text, data));
            }
            memnoth_sandbox::windows::HelperState::NotInstalled => {
                // In a full build the elevated helper would be installed here.
                // For v1 this is plan-as-data: we write nothing, and report
                // what `doctor` will show until a helper exists.
                let text = format!(
                    "sandbox helper not installed at {}\nrun `mn doctor` for the enforcement report\n",
                    marker_path.display()
                );
                let data = serde_json::json!({
                    "installed": false,
                    "marker_path": marker_path.display().to_string(),
                    "state": state.label(),
                });
                return Ok(Outcome::new("sandbox.setup", text, data));
            }
            memnoth_sandbox::windows::HelperState::Unreadable { reason } => {
                let text = format!("sandbox helper marker unreadable: {reason}\n",);
                let data = serde_json::json!({
                    "installed": false,
                    "marker_path": marker_path.display().to_string(),
                    "state": format!("unreadable: {reason}"),
                });
                return Ok(Outcome::new("sandbox.setup", text, data));
            }
        }
    }

    let status = status(&BackendPlan::build(
        OsBackend::host(),
        &ProfileName::Workspace,
        &ctx.root,
        &SandboxConfig::default(),
    )?);
    let text = format!(
        "sandbox setup: {}\n{}\n",
        status.statusline(),
        if cfg!(target_os = "windows") {
            "elevated helper install not available on this build"
        } else {
            "no elevated helper required on this platform"
        }
    );
    let data = serde_json::json!({
        "statusline": status.statusline(),
        "platform": std::env::consts::OS,
    });
    Ok(Outcome::new("sandbox.setup", text, data))
}

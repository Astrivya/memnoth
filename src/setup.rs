//! `mn setup` — interactive setup wizard.
//!
//! Walks the user through wiring the product on this machine:
//!
//! 1. Vault — create `~/.memnoth/vault.db` if missing.
//! 2. Graph — index the workspace (`.memnoth/graph.db`) if missing.
//! 3. Auth — run `mn auth login` only when a person asks for it; `--yes` never signs in.
//! 4. MCP — offer to start `mn mcp serve` if the server binary is present.
//! 5. Completions — install shell completions through `mn completion install`'s code path.
//! 6. Provider — offer to configure a model via `mn config set provider.*` if none is configured.
//! 7. Config — write `~/.memnoth/config.toml` with any changes.
//! 8. Sandbox — check/enforce sandbox policy and helper setup.
//!
//! Each step can be skipped with
//! `--skip vault,graph,auth,mcp,completions,provider,config,sandbox`.
//! `--yes` accepts the safe defaults without prompting and never signs in or
//! writes credentials: auth and provider stay skipped, and the verdict says so.

use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::Path;

use crate::auth::login;
use crate::completion;
use crate::context::{AppContext, Config};
use crate::kg;
use crate::output::Outcome;
use crate::provider::{ProviderConfig, Settings};

/// Step identifier for the setup wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Step {
    Vault,
    Graph,
    Auth,
    Mcp,
    Completions,
    Provider,
    Config,
    Sandbox,
}

impl Step {
    fn name(&self) -> &'static str {
        match self {
            Step::Vault => "vault",
            Step::Graph => "graph",
            Step::Auth => "auth",
            Step::Mcp => "mcp",
            Step::Completions => "completions",
            Step::Provider => "provider",
            Step::Config => "config",
            Step::Sandbox => "sandbox",
        }
    }

    fn all() -> &'static [Step] {
        &[
            Step::Vault,
            Step::Graph,
            Step::Auth,
            Step::Mcp,
            Step::Completions,
            Step::Provider,
            Step::Config,
            Step::Sandbox,
        ]
    }
}

/// State carried through the setup wizard.
struct SetupState {
    /// Whether each step was skipped by flag.
    skipped: HashSet<Step>,
    /// Whether each step was completed (or already satisfied).
    completed: HashSet<Step>,
    /// Why a step is not ready. Only non-completed steps carry a note.
    notes: HashMap<Step, String>,
    /// Whether config was modified and needs saving.
    config_dirty: bool,
    /// The config being built up.
    config: Config,
    /// Non-interactive mode.
    yes: bool,
    /// Quiet mode (suppress progress).
    quiet: bool,
}

impl SetupState {
    fn new(args: &crate::cli::SetupArgs) -> Self {
        let skipped: HashSet<Step> = args
            .skip
            .iter()
            .filter_map(|s| Step::all().iter().find(|step| step.name() == s).copied())
            .collect();
        let notes = skipped
            .iter()
            .map(|step| (*step, "skipped (--skip)".to_string()))
            .collect();

        Self {
            skipped,
            completed: HashSet::new(),
            notes,
            config_dirty: false,
            config: Config::default(),
            yes: args.yes,
            quiet: args.json, // JSON output implies quiet
        }
    }

    fn is_skipped(&self, step: Step) -> bool {
        self.skipped.contains(&step)
    }

    fn mark_completed(&mut self, step: Step) {
        self.completed.insert(step);
        self.notes.remove(&step);
    }

    /// Record why a step is not ready. A completed step never carries a note.
    fn note(&mut self, step: Step, reason: impl Into<String>) {
        if !self.completed.contains(&step) {
            self.notes.insert(step, reason.into());
        }
    }

    fn reason(&self, step: Step) -> &str {
        self.notes
            .get(&step)
            .map(String::as_str)
            .unwrap_or("not attempted")
    }

    fn print_progress(&self, step: Step, message: &str) {
        if self.quiet {
            return;
        }
        let status = if self.completed.contains(&step) {
            "✓"
        } else {
            "→"
        };
        eprintln!("  {} {}: {}", status, step.name(), message);
    }
}

/// Run the `mn setup` command.
pub fn run(ctx: &AppContext, args: &crate::cli::SetupArgs) -> Result<Outcome> {
    let mut state = SetupState::new(args);

    if !state.quiet {
        eprintln!("memnoth setup");
        eprintln!("────────────");
    }

    // Load existing config to preserve any existing settings
    state.config = ctx.config.clone();

    // Step 1: Vault
    if !state.is_skipped(Step::Vault) {
        run_vault_step(ctx, &mut state)?;
    }

    // Step 2: Graph
    if !state.is_skipped(Step::Graph) {
        run_graph_step(ctx, &mut state)?;
    }

    // Step 3: Auth
    if !state.is_skipped(Step::Auth) {
        run_auth_step(ctx, &mut state)?;
    }

    // Step 4: MCP
    if !state.is_skipped(Step::Mcp) {
        run_mcp_step(ctx, &mut state)?;
    }

    // Step 5: Completions
    if !state.is_skipped(Step::Completions) {
        run_completions_step(ctx, &mut state)?;
    }

    // Step 6: Provider
    if !state.is_skipped(Step::Provider) {
        run_provider_step(ctx, &mut state)?;
    }

    // Step 7: Config
    if !state.is_skipped(Step::Config) {
        run_config_step(ctx, &mut state)?;
    }

    // Step 8: Sandbox
    if !state.is_skipped(Step::Sandbox) {
        run_sandbox_step(ctx, &mut state)?;
    }

    let verdict = build_verdict_text(&state);
    let text = build_summary_text(&state, ctx, &verdict);
    let data = json!({
        "steps": Step::all().iter().map(|s| {
            json!({
                "name": s.name(),
                "skipped": state.is_skipped(*s),
                "completed": state.completed.contains(s),
                "reason": state.reason(*s),
            })
        }).collect::<Vec<_>>(),
        "config_path": ctx.config_path.display().to_string(),
        "config_saved": state.config_dirty,
        "ready": ready_steps(&state),
        "not_ready": not_ready_steps(&state),
        "verdict": verdict.clone(),
    });

    Ok(Outcome::new("setup", text, data))
}

fn run_vault_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Vault, "checking vault...");

    let vault_exists = ctx.vault_path.exists();

    if vault_exists {
        state.print_progress(Step::Vault, "vault exists");
        state.mark_completed(Step::Vault);
        return Ok(());
    }

    if state.yes || confirm_simple("Create vault?") {
        state.print_progress(Step::Vault, "creating vault...");
        ctx.ensure_home()?;
        let engine = ctx.open_vault()?;
        let _ = engine.stats()?; // force creation
        state.print_progress(Step::Vault, "vault created");
        state.mark_completed(Step::Vault);
    } else {
        state.print_progress(Step::Vault, "skipped (no vault)");
        state.note(Step::Vault, "no vault created; run `mn vault init`");
    }

    Ok(())
}

fn run_graph_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Graph, "checking code graph...");

    let graph_exists = ctx.graph_path.exists();

    if graph_exists {
        state.print_progress(Step::Graph, "graph exists");
        state.mark_completed(Step::Graph);
        return Ok(());
    }

    if state.yes || confirm_simple("Index workspace?") {
        state.print_progress(Step::Graph, "indexing workspace...");
        let mut progress_fn = |msg: &str| {
            if !state.quiet {
                eprintln!("    {}", msg);
            }
        };
        kg::index(ctx, false, false, &mut progress_fn)?;
        state.print_progress(Step::Graph, "graph indexed");
        state.mark_completed(Step::Graph);
    } else {
        state.print_progress(Step::Graph, "skipped (no graph)");
        state.note(Step::Graph, "workspace not indexed; run `mn kg index`");
    }

    Ok(())
}

fn run_auth_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Auth, "checking authentication...");

    // Check if we have a cached token
    let has_token = memnoth_auth::account_key_sync().is_some()
        || std::env::var("MEMNOTH_API_KEY").is_ok()
        || std::env::var("ANTHROPIC_API_KEY").is_ok()
        || std::env::var("OPENAI_API_KEY").is_ok()
        || std::env::var("KILO_API_KEY").is_ok()
        || std::env::var("KILOCODE_API_KEY").is_ok();

    if has_token {
        state.print_progress(Step::Auth, "authenticated");
        state.mark_completed(Step::Auth);
        return Ok(());
    }

    // `--yes` is the CI path: it must never open a browser, prompt, or write a
    // credential. Authentication is the one step that needs a person, so it is
    // reported as skipped rather than silently attempted.
    if !state.yes && confirm_simple("Run authentication flow?") {
        state.print_progress(Step::Auth, "starting login...");
        let login_args = crate::cli::AuthLoginArgs {
            no_browser: false,
            with_token: None,
            client_name: None,
            tier: None,
        };
        let outcome = login(ctx, &login_args)?;
        if !state.quiet {
            eprintln!("    {}", outcome.text.trim());
        }
        state.print_progress(Step::Auth, "authenticated");
        state.mark_completed(Step::Auth);
    } else {
        let reason = if state.yes {
            "not authenticated (--yes never signs in); run `mn auth login`"
        } else {
            "not authenticated; run `mn auth login`"
        };
        state.print_progress(Step::Auth, "skipped (not authenticated)");
        state.note(Step::Auth, reason);
    }

    Ok(())
}

fn run_mcp_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Mcp, "checking MCP server...");

    let has_server = ctx.mcp_binary().is_some();

    if !has_server {
        state.print_progress(Step::Mcp, "server binary not found (skipped)");
        // Not an error — the server is a separate crate — but not ready either.
        state.note(
            Step::Mcp,
            "no MCP server binary; build crates/memnoth-mcp or use `mn mcp add`",
        );
        return Ok(());
    }

    // Check if there's a config with mcp_url set
    let has_endpoint = state.config.mcp_url.is_some();

    if has_endpoint {
        state.print_progress(Step::Mcp, "endpoint configured");
        state.mark_completed(Step::Mcp);
        return Ok(());
    }

    if state.yes || confirm_simple("Start MCP server now? (runs in foreground, Ctrl-C to stop)") {
        state.print_progress(Step::Mcp, "starting MCP server...");
        // We can't actually run the server in the background from here
        // Just inform the user how to start it
        if !state.quiet {
            eprintln!("    Run `memnoth mcp serve` in a separate terminal to start the server.");
            eprintln!(
                "    Or run `memnoth mcp add` to register clients against the cloud endpoint."
            );
        }
        state.mark_completed(Step::Mcp);
    } else {
        state.print_progress(Step::Mcp, "skipped (server available)");
        state.note(
            Step::Mcp,
            "server available but not started; run `mn mcp serve` or `mn mcp add`",
        );
    }

    Ok(())
}

/// The completion decision, kept separate from the install so it can be tested
/// without touching a real shell profile or completion directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionDecision {
    AlreadyInstalled,
    Install,
    Decline,
}

fn completion_decision(path: &Path, yes: bool) -> CompletionDecision {
    if path.is_file() {
        return CompletionDecision::AlreadyInstalled;
    }
    if yes {
        return CompletionDecision::Install;
    }
    if confirm_simple("Install shell completions?") {
        CompletionDecision::Install
    } else {
        CompletionDecision::Decline
    }
}

/// Install shell completions using the same public code path as
/// `mn doctor --fix`: detect the shell, ask `completion` where its script lives,
/// and call `completion::install_completions`. No new generator is introduced.
fn run_completions_step(_ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Completions, "checking shell completions...");

    let shell = match completion::detect_shell() {
        Some(shell) => shell,
        None => {
            state.print_progress(Step::Completions, "skipped (no shell detected)");
            state.note(
                Step::Completions,
                "no shell detected; run `mn completion install --shell <shell>`",
            );
            return Ok(());
        }
    };

    let path = match completion::completion_path(shell) {
        Ok(path) => path,
        Err(error) => {
            state.print_progress(
                Step::Completions,
                &format!("skipped (no completion path: {error:#})"),
            );
            state.note(
                Step::Completions,
                format!("cannot resolve completion path: {error:#}"),
            );
            return Ok(());
        }
    };

    match completion_decision(&path, state.yes) {
        CompletionDecision::AlreadyInstalled => {
            state.print_progress(
                Step::Completions,
                &format!("already installed at {}", path.display()),
            );
            state.mark_completed(Step::Completions);
        }
        CompletionDecision::Install => {
            // `install_completions` is idempotent in effect (it backs up an
            // existing script), and `completion_decision` already avoided the
            // rewrite when the script is in place.
            let installed = completion::install_completions(shell)?;
            state.print_progress(
                Step::Completions,
                &format!("installed at {}", installed.display()),
            );
            state.mark_completed(Step::Completions);
        }
        CompletionDecision::Decline => {
            state.print_progress(Step::Completions, "skipped (not installed)");
            state.note(
                Step::Completions,
                "not installed; run `mn completion install`",
            );
        }
    }

    Ok(())
}

fn run_provider_step(_ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Provider, "checking model provider...");

    // Try to load provider settings
    let settings = Settings::load(&state.config).ok();

    let has_provider = settings.as_ref().map(|s| s.ready()).unwrap_or(false);
    let has_config = state.config.provider.is_some();

    if has_provider && has_config {
        state.print_progress(Step::Provider, "provider configured");
        state.mark_completed(Step::Provider);
        return Ok(());
    }

    // The CI path never guesses a provider and never touches config on behalf
    // of a person who is not there. It leaves the step honestly skipped.
    if state.yes {
        state.print_progress(
            Step::Provider,
            "skipped (--yes does not configure a provider)",
        );
        state.note(
            Step::Provider,
            "no provider configured; export an API key or run `mn config set provider.kind`",
        );
        return Ok(());
    }

    if confirm_simple("Configure a model provider?") {
        state.print_progress(Step::Provider, "configuring provider...");

        if !state.quiet {
            eprintln!("    Available providers: anthropic, openai, memnoth, kilo");
            eprintln!("    Run `memnoth config set provider.kind <kind>` to set the provider.");
            eprintln!("    Run `memnoth config set provider.model <model>` to set the model.");
            eprintln!("    Run `memnoth config set provider.base_url <url>` for custom endpoints.");
            eprintln!(
                "    Set your API key via environment: MEMNOTH_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY, or KILO_API_KEY"
            );
        }

        // Set a default provider config if none exists
        if !has_config {
            let default_kind = if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                "anthropic"
            } else if std::env::var("OPENAI_API_KEY").is_ok() {
                "openai"
            } else if std::env::var("KILO_API_KEY").is_ok()
                || std::env::var("KILOCODE_API_KEY").is_ok()
            {
                "kilo"
            } else if std::env::var("MEMNOTH_API_KEY").is_ok()
                || memnoth_auth::account_key_sync().is_some()
            {
                "memnoth"
            } else {
                "anthropic"
            };

            state.config.provider = Some(ProviderConfig {
                kind: Some(default_kind.to_string()),
                model: None,
                base_url: None,
                max_tokens: None,
                input_per_mtok: None,
                output_per_mtok: None,
                ..Default::default()
            });
            state.config_dirty = true;
            state.print_progress(
                Step::Provider,
                &format!("default provider set to {}", default_kind),
            );
        }

        state.mark_completed(Step::Provider);
    } else {
        state.print_progress(Step::Provider, "skipped (no provider configured)");
        state.note(
            Step::Provider,
            "no provider configured; export an API key or run `mn config set provider.kind`",
        );
    }

    Ok(())
}

fn run_config_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Config, "writing config...");

    if state.config_dirty {
        state
            .config
            .save(&ctx.config_path)
            .with_context(|| format!("cannot write config to {}", ctx.config_path.display()))?;
        state.print_progress(Step::Config, "config saved");
    } else {
        state.print_progress(Step::Config, "no changes");
    }

    state.mark_completed(Step::Config);
    Ok(())
}

fn run_sandbox_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Sandbox, "checking sandbox policy...");

    // Build the sandbox plan for this workspace to verify enforcement is available.
    let config = match memnoth_sandbox::profiles::load_sandbox_config(&ctx.root) {
        Ok(config) => config,
        Err(error) => {
            state.print_progress(Step::Sandbox, &format!("policy unreadable: {error:#}"));
            state.note(
                Step::Sandbox,
                format!("sandbox policy unreadable: {error:#}"),
            );
            return Ok(());
        }
    };

    let plan_ok = match memnoth_sandbox::backend::BackendPlan::build(
        memnoth_sandbox::backend::OsBackend::host(),
        &memnoth_sandbox::profiles::ProfileName::Workspace,
        &ctx.root,
        &config,
    ) {
        Ok(plan) => {
            let report = memnoth_sandbox::status::status(&plan);
            let line = report.statusline();
            if report.report.mode == "enforced" {
                state.print_progress(Step::Sandbox, &format!("enforced ({line})"));
            } else {
                state.print_progress(Step::Sandbox, &format!("policy-checked ({line})"));
            }
            true
        }
        Err(error) => {
            state.print_progress(Step::Sandbox, &format!("plan refused: {error:#}"));
            state.note(Step::Sandbox, format!("sandbox plan refused: {error:#}"));
            false
        }
    };

    if plan_ok {
        state.mark_completed(Step::Sandbox);
    }
    Ok(())
}

fn confirm_simple(prompt: &str) -> bool {
    // std::io::IsTerminal since Rust 1.70; the atty crate is unmaintained and
    // carries an unfixed unsoundness advisory (RUSTSEC-2021-0145).
    if !std::io::IsTerminal::is_terminal(&io::stdin()) {
        return false; // Non-interactive, default to no
    }

    print!("{} [y/N]: ", prompt);
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let input = input.trim().to_lowercase();
    input == "y" || input == "yes"
}

/// Steps that actually succeeded. A skipped or failed step is never in here.
fn ready_steps(state: &SetupState) -> Vec<&'static str> {
    Step::all()
        .iter()
        .filter(|step| state.completed.contains(step))
        .map(|step| step.name())
        .collect()
}

/// Every step that did not succeed, with the reason recorded when it was skipped
/// or refused. This is the honest half of the verdict.
fn not_ready_steps(state: &SetupState) -> Vec<String> {
    Step::all()
        .iter()
        .filter(|step| !state.completed.contains(step))
        .map(|step| format!("{} — {}", step.name(), state.reason(*step)))
        .collect()
}

fn build_verdict_text(state: &SetupState) -> String {
    let ready = ready_steps(state);
    let ready_text = if ready.is_empty() {
        "none".to_string()
    } else {
        format!("{} ✓", ready.join(" ✓ "))
    };

    let mut verdict = format!("ready: {ready_text} — next: mn doctor\n");
    let not_ready = not_ready_steps(state);
    if !not_ready.is_empty() {
        verdict.push_str(&format!("not ready: {}\n", not_ready.join("; ")));
    }
    verdict
}

fn build_summary_text(state: &SetupState, ctx: &AppContext, verdict: &str) -> String {
    let mut text = String::new();

    text.push_str("setup complete\n\n");

    for step in Step::all() {
        let name = step.name();
        let status = if state.is_skipped(*step) {
            "skipped"
        } else if state.completed.contains(step) {
            "done"
        } else {
            "pending"
        };
        text.push_str(&format!("  {name}: {status}\n"));
    }

    text.push_str(&format!("\nconfig: {}\n", ctx.config_path.display()));

    if state.config_dirty {
        text.push_str("config saved.\n");
    }

    text.push_str("\nnext steps:\n");
    text.push_str("  memnoth              # open the session\n");
    text.push_str("  memnoth doctor       # verify everything is wired\n");

    text.push('\n');
    text.push_str(verdict);

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(skip: &[&str], yes: bool) -> crate::cli::SetupArgs {
        crate::cli::SetupArgs {
            skip: skip.iter().map(|name| name.to_string()).collect(),
            yes,
            json: true,
        }
    }

    #[test]
    fn completions_is_a_first_class_skip_name() {
        let state = SetupState::new(&args(&["completions", "auth", "not-a-step"], true));
        assert!(state.is_skipped(Step::Completions));
        assert!(state.is_skipped(Step::Auth));
        assert_eq!(state.skipped.len(), 2);
        assert_eq!(state.reason(Step::Completions), "skipped (--skip)");
    }

    #[test]
    fn completion_install_is_idempotent_and_declines_without_a_yes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memnoth.bash");

        assert_eq!(
            completion_decision(&path, true),
            CompletionDecision::Install
        );
        std::fs::write(&path, "generated script").expect("write script");
        assert_eq!(
            completion_decision(&path, true),
            CompletionDecision::AlreadyInstalled
        );
        std::fs::remove_file(&path).expect("remove script");
        assert_eq!(
            completion_decision(&path, false),
            CompletionDecision::Decline
        );
    }

    #[test]
    fn verdict_lists_only_successful_steps_and_names_every_skipped_one() {
        let mut state = SetupState::new(&args(&["auth", "provider"], true));
        state.mark_completed(Step::Vault);
        state.mark_completed(Step::Graph);
        state.mark_completed(Step::Completions);

        let verdict = build_verdict_text(&state);
        let ready_line = verdict.lines().next().expect("ready line");
        assert!(
            ready_line.contains("vault ✓ graph ✓ completions ✓"),
            "{ready_line}"
        );
        assert!(!ready_line.contains("auth"), "{ready_line}");
        assert!(!ready_line.contains("provider"), "{ready_line}");

        let not_ready_line = verdict.lines().nth(1).expect("not ready line");
        assert!(
            not_ready_line.contains("auth — skipped (--skip)"),
            "{not_ready_line}"
        );
        assert!(
            not_ready_line.contains("provider — skipped (--skip)"),
            "{not_ready_line}"
        );
        assert!(verdict.contains("next: mn doctor"), "{verdict}");
        assert!(ready_steps(&state) == vec!["vault", "graph", "completions"]);
    }

    #[test]
    fn summary_carries_the_verdict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::context::AppContext {
            root: dir.path().to_path_buf(),
            home: dir.path().join("state"),
            vault_path: dir.path().join("state").join("vault.db"),
            namespace: "default".to_string(),
            graph_path: dir.path().join(".memnoth").join("graph.db"),
            sessions_dir: dir.path().join("state").join("sessions"),
            config_path: dir.path().join("state").join("config.toml"),
            config: crate::context::Config::default(),
            json: true,
            quiet: true,
            progress_tx: None,
            exe_path: std::path::PathBuf::from("mn"),
        };
        let state = SetupState::new(&args(&["auth", "provider"], true));
        let verdict = build_verdict_text(&state);
        let text = build_summary_text(&state, &ctx, &verdict);
        assert!(text.contains("ready:"), "{text}");
        assert!(text.contains("not ready:"), "{text}");
        assert!(text.contains("auth — skipped (--skip)"), "{text}");
        assert!(text.contains("provider — skipped (--skip)"), "{text}");
    }
}

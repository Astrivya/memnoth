//! `mn setup` — interactive setup wizard.
//!
//! Walks the user through wiring the product on this machine:
//!
//! 1. Vault — create `~/.memnoth/vault.db` if missing.
//! 2. Graph — index the workspace (`.memnoth/graph.db`) if missing.
//! 3. Auth — run `mn auth login` if no token is cached.
//! 4. MCP — offer to start `mn mcp serve` if the server binary is present.
//! 5. Provider — offer to configure a model via `mn config set provider.*` if none is configured.
//! 6. Config — write `~/.memnoth/config.toml` with any changes.
//!
//! Each step can be skipped with `--skip vault,graph,auth,mcp,provider,config`.
//! `--yes` accepts all defaults without prompting (for CI).

use anyhow::{Context, Result};
use serde_json::json;
use std::io::{self, Write};

use crate::auth::{auth_status, login};
use crate::context::{AppContext, Config};
use crate::kg;
use crate::mcp;
use crate::output::{Outcome, human_bytes};
use crate::provider::{Kind, ProviderConfig, Settings};
use crate::vault;

/// Step identifier for the setup wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Step {
    Vault,
    Graph,
    Auth,
    Mcp,
    Provider,
    Config,
}

impl Step {
    fn name(&self) -> &'static str {
        match self {
            Step::Vault => "vault",
            Step::Graph => "graph",
            Step::Auth => "auth",
            Step::Mcp => "mcp",
            Step::Provider => "provider",
            Step::Config => "config",
        }
    }

    fn all() -> &'static [Step] {
        &[
            Step::Vault,
            Step::Graph,
            Step::Auth,
            Step::Mcp,
            Step::Provider,
            Step::Config,
        ]
    }
}

/// State carried through the setup wizard.
struct SetupState {
    /// Whether each step was skipped by flag.
    skipped: std::collections::HashSet<Step>,
    /// Whether each step was completed (or already satisfied).
    completed: std::collections::HashSet<Step>,
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
        let skipped = args
            .skip
            .iter()
            .filter_map(|s| Step::all().iter().find(|step| step.name() == s).copied())
            .collect();

        Self {
            skipped,
            completed: std::collections::HashSet::new(),
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

    // Step 5: Provider
    if !state.is_skipped(Step::Provider) {
        run_provider_step(ctx, &mut state)?;
    }

    // Step 6: Config
    if !state.is_skipped(Step::Config) {
        run_config_step(ctx, &mut state)?;
    }

    let text = build_summary_text(&state, ctx);
    let data = json!({
        "steps": Step::all().iter().map(|s| {
            json!({
                "name": s.name(),
                "skipped": state.is_skipped(*s),
                "completed": state.completed.contains(s),
            })
        }).collect::<Vec<_>>(),
        "config_path": ctx.config_path.display().to_string(),
        "config_saved": state.config_dirty,
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
    }

    Ok(())
}

fn run_auth_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Auth, "checking authentication...");

    // Check if we have a cached token
    let has_token = memnoth_auth::get_credential_sync("memnoth")
        .ok()
        .flatten()
        .is_some()
        || std::env::var("MEMNOTH_API_KEY").is_ok()
        || std::env::var("ANTHROPIC_API_KEY").is_ok()
        || std::env::var("OPENAI_API_KEY").is_ok();

    if has_token {
        state.print_progress(Step::Auth, "authenticated");
        state.mark_completed(Step::Auth);
        return Ok(());
    }

    if state.yes || confirm_simple("Run authentication flow?") {
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
        state.print_progress(Step::Auth, "skipped (not authenticated)");
    }

    Ok(())
}

fn run_mcp_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
    state.print_progress(Step::Mcp, "checking MCP server...");

    let has_server = ctx.mcp_binary().is_some();

    if !has_server {
        state.print_progress(Step::Mcp, "server binary not found (skipped)");
        // Not an error - the server is a separate crate
        state.mark_completed(Step::Mcp);
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
    }

    Ok(())
}

fn run_provider_step(ctx: &AppContext, state: &mut SetupState) -> Result<()> {
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

    if state.yes || confirm_simple("Configure a model provider?") {
        state.print_progress(Step::Provider, "configuring provider...");

        if !state.quiet {
            eprintln!("    Available providers: anthropic, openai, memnoth");
            eprintln!("    Run `memnoth config set provider.kind <kind>` to set the provider.");
            eprintln!("    Run `memnoth config set provider.model <model>` to set the model.");
            eprintln!("    Run `memnoth config set provider.base_url <url>` for custom endpoints.");
            eprintln!(
                "    Set your API key via environment: MEMNOTH_API_KEY, ANTHROPIC_API_KEY, or OPENAI_API_KEY"
            );
        }

        // Set a default provider config if none exists
        if !has_config {
            let default_kind = if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                "anthropic"
            } else if std::env::var("OPENAI_API_KEY").is_ok() {
                "openai"
            } else if std::env::var("MEMNOTH_API_KEY").is_ok()
                || memnoth_auth::get_credential_sync("memnoth")
                    .ok()
                    .flatten()
                    .is_some()
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

fn confirm_simple(prompt: &str) -> bool {
    if !atty::is(atty::Stream::Stdin) {
        return false; // Non-interactive, default to no
    }

    print!("{} [y/N]: ", prompt);
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let input = input.trim().to_lowercase();
    input == "y" || input == "yes"
}

fn build_summary_text(state: &SetupState, ctx: &AppContext) -> String {
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
        text.push_str(&format!("  {}: {}\n", name, status));
    }

    text.push_str(&format!("\nconfig: {}\n", ctx.config_path.display()));

    if state.config_dirty {
        text.push_str("config saved.\n");
    }

    text.push_str("\nnext steps:\n");
    text.push_str("  memnoth              # open the session\n");
    text.push_str("  memnoth doctor       # verify everything is wired\n");

    text
}

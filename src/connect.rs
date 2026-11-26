//! `mn connect` — configure model provider with API key.
//!
//! Provides interactive and non-interactive workflows for adding, listing,
//! removing, and testing model provider configurations. API keys are stored
//! securely using memnoth-auth's credential store (OS keychain with file fallback).

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;

use crate::context::{AppContext, Config};
use crate::mcp::clients;
use crate::output::Outcome;
use crate::provider::Kind;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use memnoth_auth::MultiCredentialStore;
use memnoth_ui::{confirm::ConfirmDialog, spinner::Spinner};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Known provider configurations with defaults.
///
/// One list for the whole binary: `mn connect` and the `/connect` overlay both
/// read it, so a preset added here is offered in both places.
pub(crate) fn known_providers() -> Vec<ProviderInfo> {
    vec![
        ProviderInfo {
            name: "anthropic",
            display_name: "Anthropic (Claude)",
            kind: Kind::Anthropic,
            default_base_url: "https://api.anthropic.com",
            default_model: "claude-3-5-sonnet-20241022",
            env_var: "ANTHROPIC_API_KEY",
        },
        ProviderInfo {
            name: "openai",
            display_name: "OpenAI",
            kind: Kind::OpenAi,
            default_base_url: "https://api.openai.com/v1",
            default_model: "gpt-4o",
            env_var: "OPENAI_API_KEY",
        },
        ProviderInfo {
            name: "kilo",
            display_name: "Kilo Gateway",
            kind: Kind::Kilo,
            default_base_url: "https://ai-gateway.kilo.ai/api/v1",
            default_model: "anthropic/claude-sonnet-4.5",
            env_var: "KILO_API_KEY",
        },
        ProviderInfo {
            name: "openrouter",
            display_name: "OpenRouter",
            kind: Kind::OpenAi,
            default_base_url: "https://openrouter.ai/api/v1",
            default_model: "anthropic/claude-3.5-sonnet",
            env_var: "OPENROUTER_API_KEY",
        },
        ProviderInfo {
            name: "custom",
            display_name: "Custom OpenAI-compatible",
            kind: Kind::OpenAi,
            default_base_url: "",
            default_model: "",
            env_var: "",
        },
        ProviderInfo {
            name: "memnoth",
            display_name: "Memnoth (managed)",
            kind: Kind::Memnoth,
            default_base_url: "https://memnoth.astrivya.ai/api/v1",
            default_model: "memnoth-default",
            env_var: "MEMNOTH_API_KEY",
        },
    ]
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderInfo {
    pub(crate) name: &'static str,
    pub(crate) display_name: &'static str,
    pub(crate) kind: Kind,
    pub(crate) default_base_url: &'static str,
    pub(crate) default_model: &'static str,
    pub(crate) env_var: &'static str,
}

/// Credential key prefix for provider API keys
const PROVIDER_KEY_PREFIX: &str = "provider_api_key_";

/// Get the credential store
fn get_store() -> Result<MultiCredentialStore> {
    let rt = Runtime::new()?;
    rt.block_on(async { MultiCredentialStore::default().await })
}

/// Store a provider's API key securely
pub(crate) fn store_provider_key(name: &str, key: &str) -> Result<()> {
    let store = get_store()?;
    let key_name = format!("{}{}", PROVIDER_KEY_PREFIX, name);
    let rt = Runtime::new()?;
    rt.block_on(async { store.set(&key_name, key.to_string()).await })
        .context("Failed to store API key")
}

/// Get a provider's API key
pub(crate) fn get_provider_key(name: &str) -> Result<Option<String>> {
    let store = get_store()?;
    let key_name = format!("{}{}", PROVIDER_KEY_PREFIX, name);
    let rt = Runtime::new()?;
    rt.block_on(async { store.get(&key_name).await })
        .context("Failed to retrieve API key")
}

/// Remove a provider's API key
pub(crate) fn remove_provider_key(name: &str) -> Result<()> {
    let store = get_store()?;
    let key_name = format!("{}{}", PROVIDER_KEY_PREFIX, name);
    let rt = Runtime::new()?;
    rt.block_on(async { store.delete(&key_name).await })
        .context("Failed to delete API key")
}

/// List all stored provider keys
fn list_provider_keys() -> Result<Vec<String>> {
    let store = get_store()?;
    let rt = Runtime::new()?;
    let all_keys = rt.block_on(async { store.list_keys().await })?;
    Ok(all_keys
        .into_iter()
        .filter(|k| k.starts_with(PROVIDER_KEY_PREFIX))
        .map(|k| k[PROVIDER_KEY_PREFIX.len()..].to_string())
        .collect())
}

/// Run `mn connect add` - add or update a provider configuration
pub fn connect_add(ctx: &AppContext, args: &crate::cli::ConnectAddArgs) -> Result<Outcome> {
    if args.non_interactive {
        return connect_add_non_interactive(ctx, args);
    }
    connect_add_interactive(ctx, args)
}

/// Non-interactive provider addition
fn connect_add_non_interactive(
    ctx: &AppContext,
    args: &crate::cli::ConnectAddArgs,
) -> Result<Outcome> {
    let provider_name = args
        .provider
        .as_ref()
        .context("--provider is required in non-interactive mode")?;
    let api_key = args
        .api_key
        .as_ref()
        .context("--api-key is required in non-interactive mode")?;

    let providers = known_providers();
    let provider_info = providers
        .iter()
        .find(|p| p.name == provider_name.as_str())
        .context(format!("Unknown provider: {}", provider_name))?;

    // Store API key
    store_provider_key(provider_name, api_key)?;

    // Update config.toml
    let config_path = &ctx.config_path;
    let mut config: Config = if config_path.exists() {
        let content = std::fs::read_to_string(config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    let mut provider_config = config.provider.unwrap_or_default();
    provider_config.kind = Some(provider_info.kind.name().to_string());
    provider_config.name = Some(provider_info.name.to_string());
    provider_config.model = args
        .model
        .clone()
        .or_else(|| Some(provider_info.default_model.to_string()));
    provider_config.base_url = args
        .base_url
        .clone()
        .or_else(|| Some(provider_info.default_base_url.to_string()));

    // The report describes what was written, so it is read before the value
    // moves into the config.
    let model = provider_config.model.clone();
    let base_url = provider_config.base_url.clone();

    config.provider = Some(provider_config);
    config.save(config_path)?;

    let text = format!(
        "Provider '{}' configured successfully.\nConfig updated: {}",
        provider_name,
        config_path.display()
    );

    let data = json!({
        "provider": provider_name,
        "kind": provider_info.kind.name(),
        "model": model,
        "base_url": base_url,
        "config_path": config_path.to_string_lossy(),
    });

    Ok(Outcome::new("connect.add", text, data))
}

/// Interactive provider addition with TUI
fn connect_add_interactive(ctx: &AppContext, args: &crate::cli::ConnectAddArgs) -> Result<Outcome> {
    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let providers = known_providers();

    // Step 1: Select provider
    let selected_idx = select_provider_tui(&mut terminal, &providers, args.provider.as_deref())?;
    let provider_info = &providers[selected_idx];

    // Step 2: Get API key (masked input)
    let api_key = get_api_key_tui(&mut terminal, provider_info)?;

    // Step 3: Test connection
    test_connection_tui(
        &mut terminal,
        provider_info,
        &api_key,
        args.base_url.as_deref(),
    )?;

    // Step 4: Confirm and save
    let confirmed = confirm_save_tui(&mut terminal, provider_info)?;
    if !confirmed {
        // Cleanup terminal
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        anyhow::bail!("Cancelled by user");
    }

    // Store API key
    store_provider_key(provider_info.name, &api_key)?;

    // Update config.toml
    let config_path = &ctx.config_path;
    let mut config: Config = if config_path.exists() {
        let content = std::fs::read_to_string(config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    let mut provider_config = config.provider.unwrap_or_default();
    provider_config.kind = Some(provider_info.kind.name().to_string());
    provider_config.name = Some(provider_info.name.to_string());
    provider_config.model = args
        .model
        .clone()
        .or_else(|| Some(provider_info.default_model.to_string()));
    provider_config.base_url = args
        .base_url
        .clone()
        .or_else(|| Some(provider_info.default_base_url.to_string()));

    let model = provider_config.model.clone();
    let base_url = provider_config.base_url.clone();

    config.provider = Some(provider_config);
    config.save(config_path)?;

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let text = format!(
        "Provider '{}' configured successfully.\nConfig updated: {}",
        provider_info.name,
        config_path.display()
    );

    let data = json!({
        "provider": provider_info.name,
        "kind": provider_info.kind.name(),
        "model": model,
        "base_url": base_url,
        "config_path": config_path.to_string_lossy(),
    });

    Ok(Outcome::new("connect.add", text, data))
}

/// TUI provider selection
fn select_provider_tui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    providers: &[ProviderInfo],
    preselected: Option<&str>,
) -> Result<usize> {
    let mut selected = providers
        .iter()
        .position(|p| Some(p.name) == preselected)
        .unwrap_or(0);

    loop {
        terminal.draw(|f| {
            let area = f.area();
            let title = "Select Provider";
            let items: Vec<ratatui::text::Line> = providers
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let style = if i == selected {
                        ratatui::style::Style::default()
                            .fg(ratatui::style::Color::Yellow)
                            .add_modifier(ratatui::style::Modifier::BOLD)
                    } else {
                        ratatui::style::Style::default()
                    };
                    let prefix = if i == selected { "▸ " } else { "  " };
                    ratatui::text::Line::from(vec![
                        ratatui::text::Span::styled(format!("{}{}", prefix, p.display_name), style),
                        ratatui::text::Span::styled(
                            format!(" ({})", p.name),
                            ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
                        ),
                    ])
                })
                .collect();

            let list = ratatui::widgets::List::new(items)
                .block(
                    ratatui::widgets::Block::default()
                        .title(title)
                        .borders(ratatui::widgets::Borders::ALL),
                )
                .highlight_style(ratatui::style::Style::default());

            f.render_widget(list, area);
        })?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    if selected < providers.len() - 1 {
                        selected += 1;
                    }
                }
                KeyCode::Enter => return Ok(selected),
                KeyCode::Esc => anyhow::bail!("Cancelled"),
                _ => {}
            }
        }
    }
}

/// TUI masked API key input
fn get_api_key_tui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    provider_info: &ProviderInfo,
) -> Result<String> {
    let mut input = String::new();
    let env_var = provider_info.env_var;

    // Check if key exists in environment
    if !env_var.is_empty()
        && let Ok(env_key) = std::env::var(env_var)
    {
        input = env_key;
    }

    loop {
        terminal.draw(|f| {
            let area = f.area();
            let title = format!("Enter API Key for {}", provider_info.display_name);

            let masked = if input.is_empty() {
                "".to_string()
            } else {
                "•".repeat(input.len().min(50))
            };

            let hint = if !env_var.is_empty() && std::env::var(env_var).is_ok() {
                format!(" (detected from ${})", env_var)
            } else {
                "".to_string()
            };

            let text = vec![
                ratatui::text::Line::from(vec![
                    ratatui::text::Span::styled(
                        "API Key: ",
                        ratatui::style::Style::default().fg(ratatui::style::Color::Cyan),
                    ),
                    ratatui::text::Span::raw(&masked),
                    ratatui::text::Span::styled(
                        hint,
                        ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
                    ),
                ]),
                ratatui::text::Line::from(""),
                ratatui::text::Line::from(vec![ratatui::text::Span::styled(
                    "Press Enter when done, Esc to cancel",
                    ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
                )]),
            ];

            let paragraph = ratatui::widgets::Paragraph::new(text).block(
                ratatui::widgets::Block::default()
                    .title(title)
                    .borders(ratatui::widgets::Borders::ALL),
            );

            f.render_widget(paragraph, area);
        })?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char(c) => input.push(c),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    if !input.is_empty() {
                        return Ok(input);
                    }
                }
                KeyCode::Esc => anyhow::bail!("Cancelled"),
                _ => {}
            }
        }
    }
}

/// Test connection with spinner
fn test_connection_tui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    provider_info: &ProviderInfo,
    api_key: &str,
    base_url_override: Option<&str>,
) -> Result<()> {
    let base_url = base_url_override.unwrap_or(provider_info.default_base_url);
    let model = provider_info.default_model;
    let kind = provider_info.kind;

    let mut spinner = Spinner::new().with_message("Testing connection...");
    let start = std::time::Instant::now();

    loop {
        terminal.draw(|f| {
            let area = f.area();
            let _ = start.elapsed();
            spinner.tick();
            spinner.render(f, area);
        })?;

        // Run test in background check
        if event::poll(std::time::Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && key.code == KeyCode::Esc
        {
            anyhow::bail!("Cancelled");
        }

        // Actually test the connection (run once)
        static mut TESTED: bool = false;
        let mut tested = false;
        unsafe {
            if !TESTED {
                TESTED = true;
                tested = true;
            }
        }

        if tested {
            let result = test_provider_connection(kind, base_url, model, api_key);
            match result {
                Ok(_) => {
                    // Show success
                    terminal.draw(|f| {
                        let area = f.area();
                        let text = vec![
                            ratatui::text::Line::from(vec![ratatui::text::Span::styled(
                                "✓ Connection successful!",
                                ratatui::style::Style::default()
                                    .fg(ratatui::style::Color::Green)
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            )]),
                            ratatui::text::Line::from(""),
                            ratatui::text::Line::from(vec![ratatui::text::Span::styled(
                                "Press any key to continue...",
                                ratatui::style::Style::default()
                                    .fg(ratatui::style::Color::DarkGray),
                            )]),
                        ];
                        let paragraph = ratatui::widgets::Paragraph::new(text).block(
                            ratatui::widgets::Block::default()
                                .title("Test Result")
                                .borders(ratatui::widgets::Borders::ALL),
                        );
                        f.render_widget(paragraph, area);
                    })?;

                    // Wait for key press
                    loop {
                        if let Event::Key(key) = event::read()?
                            && key.kind == KeyEventKind::Press
                        {
                            break;
                        }
                    }
                    return Ok(());
                }
                Err(e) => {
                    // Show error
                    terminal.draw(|f| {
                        let area = f.area();
                        let text = vec![
                            ratatui::text::Line::from(vec![ratatui::text::Span::styled(
                                "✗ Connection failed!",
                                ratatui::style::Style::default()
                                    .fg(ratatui::style::Color::Red)
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            )]),
                            ratatui::text::Line::from(""),
                            ratatui::text::Line::from(vec![ratatui::text::Span::raw(
                                e.to_string(),
                            )]),
                            ratatui::text::Line::from(""),
                            ratatui::text::Line::from(vec![ratatui::text::Span::styled(
                                "Press Esc to cancel, Enter to continue anyway...",
                                ratatui::style::Style::default()
                                    .fg(ratatui::style::Color::DarkGray),
                            )]),
                        ];
                        let paragraph = ratatui::widgets::Paragraph::new(text).block(
                            ratatui::widgets::Block::default()
                                .title("Test Result")
                                .borders(ratatui::widgets::Borders::ALL),
                        );
                        f.render_widget(paragraph, area);
                    })?;

                    // Wait for key press
                    loop {
                        if let Event::Key(key) = event::read()?
                            && key.kind == KeyEventKind::Press
                        {
                            match key.code {
                                KeyCode::Esc => anyhow::bail!("Cancelled"),
                                KeyCode::Enter => return Ok(()),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Confirm save dialog
fn confirm_save_tui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    provider_info: &ProviderInfo,
) -> Result<bool> {
    let mut dialog = ConfirmDialog::with_message(
        "Save Configuration",
        format!(
            "Save provider '{}' configuration?",
            provider_info.display_name
        ),
    );
    dialog.show();
    dialog.select_yes();

    loop {
        terminal.draw(|f| {
            let area = f.area();
            dialog.render(f, area);
        })?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(response) = dialog.handle_key(key)
        {
            return Ok(response == memnoth_ui::confirm::ConfirmResponse::Yes);
        }
    }
}

/// What a connectivity probe observed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Probe {
    /// Time to the reply, in milliseconds.
    pub(crate) latency_ms: u128,
    /// Models the provider advertises, when its catalogue endpoint answers.
    pub(crate) model_count: usize,
}

/// Send one minimal completion, the way a turn would.
///
/// This is the only connectivity test in the binary: the provider's own wire
/// shape, its own model, its own auth header. A gateway that answers 401 here
/// answers 401 on a real turn, which is the point — the old TUI test asked the
/// Memnoth catalogue endpoint about every provider and therefore always failed.
pub(crate) fn probe_provider(
    kind: Kind,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<Probe> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("memnoth/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let (url, body) = match kind {
        Kind::Anthropic => {
            let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
            let body = json!({
                "model": model,
                "max_tokens": 10,
                "messages": [{"role": "user", "content": "Hi"}]
            });
            (url, body)
        }
        Kind::OpenAi | Kind::Memnoth | Kind::Kilo => {
            let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
            let body = json!({
                "model": model,
                "max_tokens": 10,
                "messages": [{"role": "user", "content": "Hi"}]
            });
            (url, body)
        }
    };

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);

    match kind {
        Kind::Anthropic => {
            req = req
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        }
        Kind::OpenAi | Kind::Memnoth | Kind::Kilo => {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        }
    }

    let started = std::time::Instant::now();
    let response = req.send()?;
    let latency_ms = started.elapsed().as_millis();

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().unwrap_or_default();
        let detail = crate::provider::scrub_secrets(&truncate(&text, 300), Some(api_key));
        anyhow::bail!("API error {}: {}", status, detail);
    }

    // The count is a nice-to-have: a provider that serves completions but not a
    // model list is connected, not broken.
    let model_count = crate::provider::fetch_provider_models(kind, base_url, Some(api_key))
        .map(|models| models.len())
        .unwrap_or(0);

    Ok(Probe {
        latency_ms,
        model_count,
    })
}

/// Test provider connection by making a simple API call
fn test_provider_connection(kind: Kind, base_url: &str, model: &str, api_key: &str) -> Result<()> {
    probe_provider(kind, base_url, model, api_key).map(|_| ())
}

/// Run `mn connect list` - list configured providers
pub fn connect_list(ctx: &AppContext, _args: &crate::cli::ConnectListArgs) -> Result<Outcome> {
    let stored_keys = list_provider_keys()?;
    let config: Config = Config::load(&ctx.config_path)?;
    let provider_config = config.provider;

    let mut rows = Vec::new();

    for name in &stored_keys {
        let providers = known_providers();
        let info = providers.iter().find(|p| p.name == name);

        // `name` is the identity: two OpenAI-compatible gateways share a
        // `kind`, so kind alone would mark both rows active.
        let is_active = provider_config
            .as_ref()
            .map(|configured| {
                configured.name.as_deref() == Some(name.as_str())
                    || (configured.name.is_none()
                        && configured.kind.as_deref() == info.map(|info| info.kind.name()))
            })
            .unwrap_or(false);

        // Only the active row can claim the config's model and endpoint; the
        // other rows show what their preset would be, not what this one is.
        let active = provider_config.as_ref().filter(|_| is_active);
        let model = active
            .and_then(|p| p.model.clone())
            .or_else(|| info.map(|i| i.default_model.to_string()))
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| "—".to_string());
        let base_url = active
            .and_then(|p| p.base_url.clone())
            .or_else(|| info.map(|i| i.default_base_url.to_string()))
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| "—".to_string());

        rows.push(json!({
            "name": name,
            "display_name": info.map(|i| i.display_name).unwrap_or(name),
            "kind": info.map(|i| i.kind.name()).unwrap_or("unknown"),
            "model": model,
            "base_url": base_url,
            "is_active": is_active,
            "has_key": true,
        }));
    }

    if rows.is_empty() {
        let text = "No providers configured. Run `mn connect add` to add one.".to_string();
        let data = json!({ "providers": [] });
        return Ok(Outcome::new("connect.list", text, data));
    }

    // Render table
    let mut table_text = String::new();
    table_text.push_str(&format!(
        "{:<20} {:<12} {:<30} {:<40} {}\n",
        "NAME", "KIND", "MODEL", "BASE URL", "STATUS"
    ));
    table_text.push_str(&"-".repeat(120));
    table_text.push('\n');

    for row in &rows {
        let name = row["name"].as_str().unwrap_or("");
        let kind = row["kind"].as_str().unwrap_or("");
        let model = row["model"].as_str().unwrap_or("");
        let base_url = row["base_url"].as_str().unwrap_or("");
        let is_active = row["is_active"].as_bool().unwrap_or(false);

        let status = if is_active { "● active" } else { "○" };
        table_text.push_str(&format!(
            "{:<20} {:<12} {:<30} {:<40} {}\n",
            truncate(name, 19),
            truncate(kind, 11),
            truncate(model, 29),
            truncate(base_url, 39),
            status
        ));
    }

    let data = json!({ "providers": rows });
    Ok(Outcome::new("connect.list", table_text, data))
}

/// Run `mn connect remove <name>` - remove a provider configuration
pub fn connect_remove(ctx: &AppContext, args: &crate::cli::ConnectRemoveArgs) -> Result<Outcome> {
    let name = &args.name;

    // Check if provider exists
    let stored_keys = list_provider_keys()?;
    if !stored_keys.contains(name) {
        anyhow::bail!(
            "Provider '{}' not found. Run `mn connect list` to see configured providers.",
            name
        );
    }

    // Remove API key
    remove_provider_key(name)?;

    // If this was the active provider, clear config
    let config_path = &ctx.config_path;
    let mut config: Config = if config_path.exists() {
        let content = std::fs::read_to_string(config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    if let Some(ref provider) = config.provider
        && (provider.name.as_deref() == Some(name.as_str())
            || provider.kind.as_deref() == Some(name.as_str()))
    {
        config.provider = None;
        config.save(config_path)?;
    }

    let text = format!("Provider '{}' removed.", name);
    let data = json!({ "provider": name, "removed": true });
    Ok(Outcome::new("connect.remove", text, data))
}

/// Run `mn connect test` - test connection to a provider
pub fn connect_test(ctx: &AppContext, args: &crate::cli::ConnectTestArgs) -> Result<Outcome> {
    let config: Config = Config::load(&ctx.config_path)?;
    let provider_config = config
        .provider
        .context("No provider configured. Run `mn connect add` first.")?;

    let provider_name = args
        .provider
        .as_ref()
        .or(provider_config.name.as_ref())
        .or(provider_config.kind.as_ref())
        .context("No provider specified and no default configured")?;

    // Get API key
    let api_key = get_provider_key(provider_name)?.context(format!(
        "No API key stored for provider '{}'. Run `mn connect add` first.",
        provider_name
    ))?;

    let providers = known_providers();
    let provider_info = providers
        .iter()
        .find(|p| p.name == provider_name.as_str())
        .context(format!("Unknown provider: {}", provider_name))?;

    // Overrides from the config describe the active provider; testing a
    // different one must use that preset's own endpoint and model.
    let testing_active = provider_config
        .name
        .as_deref()
        .map(|name| name == provider_name.as_str())
        .unwrap_or_else(|| provider_config.kind.as_deref() == Some(provider_name.as_str()));

    let base_url = provider_config
        .base_url
        .as_deref()
        .filter(|_| testing_active)
        .unwrap_or(provider_info.default_base_url);
    let model = provider_config
        .model
        .as_deref()
        .filter(|_| testing_active)
        .unwrap_or(provider_info.default_model);
    let kind = provider_info.kind;

    // Test connection
    let probe = probe_provider(kind, base_url, model, &api_key)?;

    let text = format!(
        "Connection to '{}' successful — {} ms, {} models advertised",
        provider_info.display_name, probe.latency_ms, probe.model_count
    );
    let data = json!({
        "provider": provider_name,
        "kind": kind.name(),
        "model": model,
        "base_url": base_url,
        "success": true,
        "latency_ms": probe.latency_ms,
        "model_count": probe.model_count,
    });

    Ok(Outcome::new("connect.test", text, data))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ── `mn connect --watch` — opt-in agent-host watcher ────────────────────────
//
// Deliberately additive to the provider configuration above. This half never
// reads, writes, or prints a provider key or a cloud token: every registration
// it makes is a local stdio entry with `allow_writes = false` and `token = None`.
//
// Safety rules, in the order they are applied:
//   1. opt-in — nothing here runs unless `mn connect` is invoked with no
//      provider subcommand, and the polling loop only with `--watch`;
//   2. visible — every action is printed to stderr and appended to
//      `~/.memnoth/connect.log` before the next host is considered;
//   3. reversible — the entry is an ordinary `mcp add --local` registration,
//      removable with the existing `mn mcp remove` path;
//   4. no silent writes — only hosts with an existing config file or config
//      *directory* are candidates, and configs we cannot parse are skipped;
//   5. no token — `mcp::add(local = true)` refuses a URL or token outright.

/// Set by SIGINT/SIGTERM (or a Windows console control event). The handler
/// only touches an atomic, so it is safe to run at signal time; the loop
/// checks it between hosts and between sleep slices, never mid-write.
static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn note_stop(_signal: i32) {
    STOP.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
unsafe extern "C" {
    fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize;
}

/// Install the Unix stop handlers. `SIG_ERR` is `(void (*)(int))-1`, i.e. a
/// pointer-sized all-ones value; a failure is reported instead of leaving a
/// watcher that ignores Ctrl-C.
#[cfg(unix)]
fn install_stop_handlers() -> Result<()> {
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    unsafe {
        if signal(SIGINT, note_stop) == usize::MAX {
            anyhow::bail!("cannot install a SIGINT handler");
        }
        if signal(SIGTERM, note_stop) == usize::MAX {
            anyhow::bail!("cannot install a SIGTERM handler");
        }
    }
    Ok(())
}

#[cfg(windows)]
extern "system" fn note_console_ctrl(kind: u32) -> i32 {
    // CTRL_C_EVENT, CTRL_BREAK_EVENT and CTRL_CLOSE_EVENT. Windows has no
    // SIGTERM; a console close is its nearest equivalent.
    if matches!(kind, 0..=2) {
        STOP.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

#[cfg(windows)]
fn install_stop_handlers() -> Result<()> {
    unsafe {
        if SetConsoleCtrlHandler(Some(note_console_ctrl), 1) == 0 {
            anyhow::bail!("cannot install a console control handler");
        }
    }
    Ok(())
}

/// Sleep in short slices so a stop request is observed without waiting out a
/// full 300-second interval.
fn sleep_until_stop(total: Duration) {
    const SLICE: Duration = Duration::from_millis(250);
    let mut left = total;
    while left > Duration::ZERO && !STOP.load(Ordering::SeqCst) {
        let step = if left < SLICE { left } else { SLICE };
        std::thread::sleep(step);
        left -= step;
    }
}

/// One redacted, single-line JSON record. `scrub_secrets` is the crate's
/// generic token redactor; newlines are flattened so one action is one line
/// in the audit log no matter what a host CLI printed.
fn connect_log_line(event: &str, client: &str, detail: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);
    let clean = |text: &str| crate::provider::scrub_secrets(text, None).replace(['\r', '\n'], " ");
    json!({
        "ts": crate::output::rfc3339(now),
        "event": clean(event),
        "client": clean(client),
        "detail": clean(detail),
    })
    .to_string()
}

/// Append one audit line to `~/.memnoth/connect.log`. A log that cannot be
/// written is an error: a watcher that registrations without a record is the
// one thing this design must not allow.
fn append_connect_log(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("cannot write {}", path.display()))
}

/// A host is a candidate only when its own config file or config *directory*
/// exists. `clients::detect` alone also matches a binary on PATH and, for
/// Claude, a config path whose parent is the home directory — neither proves
/// the user configured that host. Refusing those avoids creating a config for
/// a program the user has never set up.
fn host_has_config_dir(kind: clients::ClientKind, home: &Path) -> bool {
    let path = kind.config_path(home);
    if path.exists() {
        return true;
    }
    match path.parent() {
        Some(parent) if parent != home => parent.is_dir(),
        _ => false,
    }
}

/// `clients::detect` is still the single source of "known hosts"; the filter
/// above only narrows what this opt-in daemon is allowed to write.
fn detect_configured_hosts(home: &Path) -> Vec<clients::ClientKind> {
    clients::detect(home)
        .into_iter()
        .filter(|kind| host_has_config_dir(*kind, home))
        .collect()
}

/// What one candidate host needs. `Skip` is used for a config we must not
/// touch — an unparsable file, or JSONC whose comments our writer would lose.
#[derive(Debug, PartialEq, Eq)]
enum HostAction {
    Already,
    Register,
    Skip(String),
}

fn plan_host(kind: clients::ClientKind, home: &Path) -> (String, String, HostAction) {
    let path = kind.config_path(home).display().to_string();
    let status = clients::status_for_workspace(kind, home, None, "");
    let action = if !status.ok {
        HostAction::Skip(format!("cannot read config: {}", status.detail))
    } else if status.detail.contains("JSON with comments") {
        HostAction::Skip("config is JSONC; register it manually to preserve comments".into())
    } else if status.registered {
        HostAction::Already
    } else {
        HostAction::Register
    };
    (kind.name().to_string(), path, action)
}

/// The accumulated result of every pass in this process.
#[derive(Debug, Default)]
struct PassReport {
    detected: Vec<String>,
    registered: Vec<String>,
    would_register: Vec<String>,
    already: Vec<String>,
    skipped: Vec<Value>,
    failed: Vec<Value>,
}

impl PassReport {
    fn merge(&mut self, other: PassReport) {
        self.detected.extend(other.detected);
        self.registered.extend(other.registered);
        self.would_register.extend(other.would_register);
        self.already.extend(other.already);
        self.skipped.extend(other.skipped);
        self.failed.extend(other.failed);
    }
}

/// One detection pass. `seen` makes each host an attempt at most once per
/// process: a refused config is never retried in a loop, and a host the user
/// later unregisters is not silently added back.
///
/// `ctx` is only needed for a real registration; `--dry-run` passes `None`,
/// which keeps a dry run entirely free of writes.
fn connect_pass(
    ctx: Option<&AppContext>,
    home: &Path,
    log_path: &Path,
    dry_run: bool,
    seen: &mut std::collections::BTreeSet<&'static str>,
) -> Result<PassReport> {
    let mut report = PassReport::default();
    let hosts = detect_configured_hosts(home);
    report.detected = hosts.iter().map(|kind| kind.name().to_string()).collect();

    for kind in hosts {
        let name = kind.name();
        if seen.contains(name) {
            continue;
        }
        let (client, path, action) = plan_host(kind, home);
        match action {
            HostAction::Already => {
                append_connect_log(
                    log_path,
                    &connect_log_line("skip", &client, "already registered"),
                )?;
                report.already.push(client.clone());
                seen.insert(name);
            }
            HostAction::Skip(reason) => {
                append_connect_log(log_path, &connect_log_line("skip", &client, &reason))?;
                report
                    .skipped
                    .push(json!({ "client": client, "reason": reason }));
                seen.insert(name);
            }
            HostAction::Register if dry_run => {
                let detail = format!("would register local read-only entry at {path}");
                eprintln!("[connect] {detail} (no token)");
                append_connect_log(log_path, &connect_log_line("dry-run", &client, &detail))?;
                report.would_register.push(client.clone());
                seen.insert(name);
            }
            HostAction::Register => {
                let ctx = ctx.context("an AppContext is required to register a host")?;
                eprintln!("[connect] registering {client} at {path} (local, read-only, no token)");
                let names = [name.to_string()];
                let added = crate::mcp::add(
                    ctx,
                    crate::mcp::AddOptions {
                        client_names: &names,
                        profile: None,
                        url: None,
                        token: None,
                        db: None,
                        team: None,
                        local: true,
                        allow_writes: false,
                        dry_run: false,
                    },
                );
                match added {
                    Ok(add_report) => {
                        let detail = add_report
                            .results
                            .first()
                            .map(|row| row.detail.clone())
                            .unwrap_or_else(|| "registered".to_string());
                        append_connect_log(
                            log_path,
                            &connect_log_line("register", &client, &detail),
                        )?;
                        report.registered.push(client.clone());
                    }
                    Err(error) => {
                        let detail = format!("registration failed: {error:#}");
                        append_connect_log(log_path, &connect_log_line("error", &client, &detail))?;
                        eprintln!(
                            "[connect] {client}: registration failed; see {}",
                            log_path.display()
                        );
                        let clean = crate::provider::scrub_secrets(&detail, None);
                        report
                            .failed
                            .push(json!({ "client": client, "reason": clean }));
                    }
                }
                seen.insert(name);
            }
        }
    }
    Ok(report)
}

/// Run `mn connect` with no provider subcommand.
///
/// Default is a single pass. `--watch` starts a foreground loop (never a
/// detached daemon), `--once` forces one pass, and `--dry-run` never writes a
/// host config. Ctrl-C or SIGTERM sets [`STOP`] and the loop logs a clean stop.
pub fn run(ctx: &AppContext, args: &crate::cli::ConnectArgs) -> Result<Outcome> {
    let home = clients::home_dir()
        .context("cannot find a home directory; connect needs one to inspect host configs")?;
    let interval = args.interval.max(1);
    let log_path = ctx.home.join("connect.log");
    let mut seen = std::collections::BTreeSet::new();

    append_connect_log(
        &log_path,
        &connect_log_line(
            "start",
            "",
            &format!(
                "watch={} once={} interval_s={} dry_run={}",
                args.watch, args.once, interval, args.dry_run
            ),
        ),
    )?;
    eprintln!("[connect] audit log: {}", log_path.display());

    let mut total = PassReport::default();
    if !args.watch || args.once {
        let pass = connect_pass(Some(ctx), &home, &log_path, args.dry_run, &mut seen)?;
        total.merge(pass);
    } else {
        install_stop_handlers()?;
        while !STOP.load(Ordering::SeqCst) {
            let pass = connect_pass(Some(ctx), &home, &log_path, args.dry_run, &mut seen)?;
            total.merge(pass);
            if STOP.load(Ordering::SeqCst) {
                break;
            }
            sleep_until_stop(Duration::from_secs(interval));
        }
    }

    append_connect_log(&log_path, &connect_log_line("stop", "", "clean stop"))?;
    eprintln!("[connect] stopped; audit log: {}", log_path.display());

    let summary = format!(
        "connect: detected {} host(s), registered {}, would register {}, already {}, skipped {}, failed {}; log {}",
        total.detected.len(),
        total.registered.len(),
        total.would_register.len(),
        total.already.len(),
        total.skipped.len(),
        total.failed.len(),
        log_path.display()
    );
    let data = json!({
        "watch": args.watch && !args.once,
        "once": !args.watch || args.once,
        "interval_s": interval,
        "dry_run": args.dry_run,
        "detected": total.detected,
        "registered": total.registered,
        "would_register": total.would_register,
        "already_registered": total.already,
        "skipped": total.skipped,
        "failed": total.failed,
        "log": log_path.display().to_string(),
    });
    Ok(Outcome::new("connect.watch", summary, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_providers() {
        let providers = known_providers();
        assert_eq!(providers.len(), 6);
        assert!(providers.iter().any(|p| p.name == "anthropic"));
        assert!(providers.iter().any(|p| p.name == "openai"));
        assert!(providers.iter().any(|p| p.name == "kilo"));
        assert!(providers.iter().any(|p| p.name == "openrouter"));
        assert!(providers.iter().any(|p| p.name == "custom"));
        assert!(providers.iter().any(|p| p.name == "memnoth"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn connect_log_line_redacts_tokens_and_stays_on_one_line() {
        let line = connect_log_line(
            "register",
            "cursor",
            "Bearer sk-abcdefghijklmnop wrote /tmp/x\nsecond line",
        );
        assert_eq!(line.lines().count(), 1, "{line}");
        assert!(!line.contains("sk-abcdefghijklmnop"), "{line}");
        assert!(line.contains("Bearer ***"), "{line}");
        let value: Value = serde_json::from_str(&line).expect("one JSON object per action");
        assert_eq!(value["event"], "register");
        assert_eq!(value["client"], "cursor");
        assert!(value["ts"].as_str().is_some(), "{value}");
    }

    #[test]
    fn a_bare_home_does_not_make_claude_a_candidate() {
        let dir = tempfile::tempdir().unwrap();
        // Claude's config file sits directly in home; the home directory
        // existing is not evidence that Claude has ever been configured.
        assert!(!host_has_config_dir(
            clients::ClientKind::Claude,
            dir.path()
        ));
        std::fs::write(dir.path().join(".claude.json"), "{}").unwrap();
        assert!(host_has_config_dir(clients::ClientKind::Claude, dir.path()));
    }

    #[test]
    fn a_cursor_config_directory_makes_cursor_a_candidate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cursor")).unwrap();
        assert!(host_has_config_dir(clients::ClientKind::Cursor, dir.path()));
        assert!(detect_configured_hosts(dir.path()).contains(&clients::ClientKind::Cursor));
    }

    #[test]
    fn dry_run_reports_a_host_without_touching_its_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join(".cursor").join("mcp.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "{\"mcpServers\":{}}\n").unwrap();
        let before = std::fs::read(&config).unwrap();
        let log = home.join("connect.log");
        let mut seen = std::collections::BTreeSet::new();

        let report = connect_pass(None, home, &log, true, &mut seen).unwrap();
        assert_eq!(report.would_register, vec!["cursor".to_string()]);
        assert!(report.registered.is_empty());
        assert_eq!(
            std::fs::read(&config).unwrap(),
            before,
            "dry run wrote a config"
        );

        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("\"dry-run\""), "{text}");
        assert!(text.contains("cursor"), "{text}");
    }

    #[test]
    fn an_existing_memnoth_entry_is_already_registered() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join(".cursor").join("mcp.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            "{\"mcpServers\":{\"memnoth\":{\"command\":\"mn\"}}}\n",
        )
        .unwrap();
        let log = home.join("connect.log");
        let mut seen = std::collections::BTreeSet::new();

        let report = connect_pass(None, home, &log, true, &mut seen).unwrap();
        assert_eq!(report.already, vec!["cursor".to_string()]);
        assert!(report.would_register.is_empty());
    }

    #[test]
    fn a_host_is_attempted_at_most_once_per_process() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        let log = home.join("connect.log");
        let mut seen = std::collections::BTreeSet::new();

        let first = connect_pass(None, home, &log, true, &mut seen).unwrap();
        let second = connect_pass(None, home, &log, true, &mut seen).unwrap();
        assert_eq!(first.would_register, vec!["cursor".to_string()]);
        assert!(second.would_register.is_empty());
        assert_eq!(second.detected, vec!["cursor".to_string()]);
    }
}

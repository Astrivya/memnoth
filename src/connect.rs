//! `mn connect` — configure model provider with API key.
//!
//! Provides interactive and non-interactive workflows for adding, listing,
//! removing, and testing model provider configurations. API keys are stored
//! securely using memnoth-auth's credential store (OS keychain with file fallback).

use anyhow::{Context, Result};
use serde_json::json;
use std::io::{self, Write};
use tokio::runtime::Runtime;

use crate::context::{AppContext, Config};
use crate::output::Outcome;
use crate::provider::{Kind, ProviderConfig};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use memnoth_auth::{CredentialStore, MultiCredentialStore};
use memnoth_cloud_client::CloudClient;
use memnoth_ui::{
    confirm::ConfirmDialog,
    spinner::Spinner,
    table::{Column, Row, Table},
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Known provider configurations with defaults
fn known_providers() -> Vec<ProviderInfo> {
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
    ]
}

#[derive(Debug, Clone)]
struct ProviderInfo {
    name: &'static str,
    display_name: &'static str,
    kind: Kind,
    default_base_url: &'static str,
    default_model: &'static str,
    env_var: &'static str,
}

/// Credential key prefix for provider API keys
const PROVIDER_KEY_PREFIX: &str = "provider_api_key_";

/// Get the credential store
fn get_store() -> Result<MultiCredentialStore> {
    let rt = Runtime::new()?;
    rt.block_on(async { MultiCredentialStore::default().await })
}

/// Store a provider's API key securely
fn store_provider_key(name: &str, key: &str) -> Result<()> {
    let store = get_store()?;
    let key_name = format!("{}{}", PROVIDER_KEY_PREFIX, name);
    let rt = Runtime::new()?;
    rt.block_on(async { store.set(&key_name, key.to_string()).await })
        .context("Failed to store API key")
}

/// Get a provider's API key
fn get_provider_key(name: &str) -> Result<Option<String>> {
    let store = get_store()?;
    let key_name = format!("{}{}", PROVIDER_KEY_PREFIX, name);
    let rt = Runtime::new()?;
    rt.block_on(async { store.get(&key_name).await })
        .context("Failed to retrieve API key")
}

/// Remove a provider's API key
fn remove_provider_key(name: &str) -> Result<()> {
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
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    let mut provider_config = config.provider.unwrap_or_default();
    provider_config.kind = Some(provider_info.kind.name().to_string());
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
    config.save(&config_path)?;

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
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    let mut provider_config = config.provider.unwrap_or_default();
    provider_config.kind = Some(provider_info.kind.name().to_string());
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
    config.save(&config_path)?;

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

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Up => {
                        if selected > 0 {
                            selected -= 1;
                        }
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
}

/// TUI masked API key input
fn get_api_key_tui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    provider_info: &ProviderInfo,
) -> Result<String> {
    let mut input = String::new();
    let env_var = provider_info.env_var;

    // Check if key exists in environment
    if !env_var.is_empty() {
        if let Ok(env_key) = std::env::var(env_var) {
            input = env_key;
        }
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

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
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
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc {
                    anyhow::bail!("Cancelled");
                }
            }
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
                        if let Event::Key(key) = event::read()? {
                            if key.kind == KeyEventKind::Press {
                                break;
                            }
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
                        if let Event::Key(key) = event::read()? {
                            if key.kind == KeyEventKind::Press {
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

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                if let Some(response) = dialog.handle_key(key) {
                    return Ok(response == memnoth_ui::confirm::ConfirmResponse::Yes);
                }
            }
        }
    }
}

/// Test provider connection by making a simple API call
fn test_provider_connection(kind: Kind, base_url: &str, model: &str, api_key: &str) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
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
        Kind::OpenAi | Kind::Memnoth => {
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
        Kind::OpenAi | Kind::Memnoth => {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        }
    }

    let response = req.send()?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, text);
    }

    Ok(())
}

/// Run `mn connect list` - list configured providers
pub fn connect_list(ctx: &AppContext, args: &crate::cli::ConnectListArgs) -> Result<Outcome> {
    let stored_keys = list_provider_keys()?;
    let config: Config = Config::load(&ctx.config_path)?;
    let provider_config = config.provider;

    let mut rows = Vec::new();

    for name in &stored_keys {
        let providers = known_providers();
        let info = providers.iter().find(|p| p.name == name);

        let is_active = provider_config
            .as_ref()
            .and_then(|p| p.kind.as_ref())
            .map(|k| info.map(|i| i.kind.name() == k.as_str()).unwrap_or(false))
            .unwrap_or(false);

        let model = provider_config
            .as_ref()
            .and_then(|p| p.model.clone())
            .unwrap_or_else(|| "—".to_string());
        let base_url = provider_config
            .as_ref()
            .and_then(|p| p.base_url.clone())
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
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content).context("Failed to parse config.toml")?
    } else {
        Config::default()
    };

    if let Some(ref provider) = config.provider {
        if provider.kind.as_deref() == Some(name.as_str()) {
            config.provider = None;
            config.save(&config_path)?;
        }
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

    let base_url = args
        .provider
        .as_ref()
        .and_then(|_| provider_config.base_url.as_deref())
        .unwrap_or(provider_info.default_base_url);
    let model = provider_config
        .model
        .as_deref()
        .unwrap_or(provider_info.default_model);
    let kind = provider_info.kind;

    // Test connection
    test_provider_connection(kind, base_url, model, &api_key)?;

    let text = format!("Connection to '{}' successful!", provider_info.display_name);
    let data = json!({
        "provider": provider_name,
        "kind": kind.name(),
        "model": model,
        "base_url": base_url,
        "success": true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_providers() {
        let providers = known_providers();
        assert_eq!(providers.len(), 4);
        assert!(providers.iter().any(|p| p.name == "anthropic"));
        assert!(providers.iter().any(|p| p.name == "openai"));
        assert!(providers.iter().any(|p| p.name == "openrouter"));
        assert!(providers.iter().any(|p| p.name == "custom"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
    }
}

//! Connect overlay: configure model provider connections with beautiful TUI UI.

use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::provider::{Kind, Settings};
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Default)]
pub struct ConnectOverlay {
    visible: bool,
    providers: Vec<ProviderCard>,
    selected: usize,
    list_state: ListState,
    editing: bool,
    edit_field: EditField,
    edit_buffer: String,
    edit_cursor: usize,
    testing: Option<usize>,
    test_results: Vec<TestResult>,
    info_popup: Option<InfoPopup>,
}

#[derive(Debug, Clone)]
struct ProviderCard {
    kind: Kind,
    name: String,
    display_name: String,
    icon: &'static str,
    status: ProviderStatus,
    last_tested: Option<String>,
    model_count: usize,
    base_url: Option<String>,
    model: Option<String>,
    has_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderStatus {
    Connected,
    Disconnected,
    Testing,
    Error(String),
}

#[derive(Debug, Clone)]
struct TestResult {
    provider: Kind,
    success: bool,
    message: String,
    timestamp: String,
}

#[derive(Debug, Clone)]
struct InfoPopup {
    provider: Kind,
    details: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    Model,
    BaseUrl,
    ApiKey,
    Kind,
}

impl ConnectOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    fn load_providers(&mut self, app: &mut App) {
        // Load current provider settings
        let settings = Settings::load(&app.config).ok();

        // Define all supported providers
        let all_providers = vec![
            (Kind::Anthropic, "Anthropic", "🤖", "https://api.anthropic.com"),
            (Kind::OpenAI, "OpenAI", "🧠", "https://api.openai.com"),
            (Kind::Memnoth, "Memnoth", "✨", "https://api.memnoth.astrivya.ai"),
        ];

        self.providers = all_providers
            .into_iter()
            .map(|(kind, display_name, icon, default_url)| {
                let has_key = self.check_api_key(&kind, &app.config);
                let base_url = settings.as_ref().and_then(|s| {
                    if s.kind == kind {
                        s.base_url.clone()
                    } else {
                        None
                    }
                }).or_else(|| Some(default_url.to_string()));

                let model = settings.as_ref().and_then(|s| {
                    if s.kind == kind {
                        Some(s.model.clone())
                    } else {
                        None
                    }
                }).unwrap_or_else(|| self.default_model_for(&kind));

                let status = if has_key && base_url.is_some() {
                    ProviderStatus::Connected
                } else {
                    ProviderStatus::Disconnected
                };

                ProviderCard {
                    kind,
                    name: format!("{:?}", kind).to_lowercase(),
                    display_name: display_name.to_string(),
                    icon,
                    status,
                    last_tested: None,
                    model_count: 0,
                    base_url,
                    model: Some(model),
                    has_key,
                }
            })
            .collect();

        self.selected = 0;
        self.list_state.select(Some(0));
    }

    fn check_api_key(&self, kind: &Kind, config: &crate::Config) -> bool {
        match kind {
            Kind::Anthropic => {
                std::env::var("ANTHROPIC_API_KEY").is_ok()
                    || config.provider.as_ref().and_then(|p| p.api_key_env.as_ref())
                        .map(|env| std::env::var(env).is_ok())
                        .unwrap_or(false)
            }
            Kind::OpenAI => {
                std::env::var("OPENAI_API_KEY").is_ok()
                    || config.provider.as_ref().and_then(|p| p.api_key_env.as_ref())
                        .map(|env| std::env::var(env).is_ok())
                        .unwrap_or(false)
            }
            Kind::Memnoth => {
                // Check memnoth-auth credential store
                memnoth_auth::get_credential_sync("memnoth").is_ok()
                    || std::env::var("MEMNOTH_API_KEY").is_ok()
            }
        }
    }

    fn default_model_for(&self, kind: &Kind) -> String {
        match kind {
            Kind::Anthropic => "claude-3-5-sonnet-20241022".to_string(),
            Kind::OpenAI => "gpt-4o".to_string(),
            Kind::Memnoth => "memnoth-default".to_string(),
        }
    }

    fn start_edit(&mut self) {
        if self.selected < self.providers.len() {
            self.editing = true;
            self.edit_field = EditField::Model;
            self.edit_buffer = self.providers[self.selected].model.clone().unwrap_or_default();
            self.edit_cursor = self.edit_buffer.len();
        }
    }

    fn apply_edit(&mut self, app: &mut App) {
        if self.selected < self.providers.len() {
            let provider = &mut self.providers[self.selected];
            match self.edit_field {
                EditField::Model => {
                    provider.model = Some(self.edit_buffer.clone());
                    self.save_provider_config(app);
                }
                EditField::BaseUrl => {
                    provider.base_url = Some(self.edit_buffer.clone());
                    self.save_provider_config(app);
                }
                EditField::ApiKey => {
                    // API key handling would go here - stored via memnoth-auth
                    self.save_provider_config(app);
                }
                EditField::Kind => {}
            }
            self.edit_buffer.clear();
            self.edit_cursor = 0;
        }
    }

    fn save_provider_config(&self, app: &mut App) {
        if self.selected < self.providers.len() {
            let provider = &self.providers[self.selected];
            app.config.provider = Some(crate::provider::ProviderConfig {
                kind: Some(format!("{:?}", provider.kind).to_lowercase()),
                model: provider.model.clone(),
                base_url: provider.base_url.clone(),
                ..Default::default()
            });
            let _ = app.config.save(&app.config_path);
        }
    }

    fn test_connection(&mut self, app: &mut App) {
        if self.selected >= self.providers.len() {
            return;
        }

        self.testing = Some(self.selected);
        self.providers[self.selected].status = ProviderStatus::Testing;

        let provider = self.providers[self.selected].clone();
        let base_url = provider.base_url.clone().unwrap_or_else(|| provider.kind.default_base_url().to_string());
        let api_key = self.get_api_key(&provider.kind, &app.config);

        // Spawn async test
        let kind = provider.kind;
        let selected = self.selected;
        let tx = app.provider_test_tx.clone();

        std::thread::spawn(move || {
            let result = crate::provider::fetch_memnoth_models(&base_url, api_key.as_deref());
            let timestamp = chrono::Local::now().format("%H:%M:%S").to_string();

            let (success, message, model_count) = match result {
                Ok(models) => (true, format!("Connected - {} models available", models.len()), models.len()),
                Err(e) => (false, format!("Failed: {}", e), 0),
            };

            let _ = tx.send(crate::tui::app::ProviderTestResult {
                index: selected,
                success,
                message,
                model_count,
                timestamp,
            });
        });
    }

    fn get_api_key(&self, kind: &Kind, config: &crate::Config) -> Option<String> {
        match kind {
            Kind::Anthropic => {
                std::env::var("ANTHROPIC_API_KEY").ok()
                    .or_else(|| config.provider.as_ref().and_then(|p| p.api_key_env.as_ref())
                        .and_then(|env| std::env::var(env).ok()))
            }
            Kind::OpenAI => {
                std::env::var("OPENAI_API_KEY").ok()
                    .or_else(|| config.provider.as_ref().and_then(|p| p.api_key_env.as_ref())
                        .and_then(|env| std::env::var(env).ok()))
            }
            Kind::Memnoth => {
                memnoth_auth::get_credential_sync("memnoth").ok()
                    .or_else(|| std::env::var("MEMNOTH_API_KEY").ok())
            }
        }
    }

    fn handle_test_result(&mut self, index: usize, success: bool, message: String, model_count: usize, timestamp: String) {
        if index < self.providers.len() {
            self.providers[index].status = if success {
                ProviderStatus::Connected
            } else {
                ProviderStatus::Error(message.clone())
            };
            self.providers[index].last_tested = Some(timestamp);
            self.providers[index].model_count = model_count;
            self.test_results.push(TestResult {
                provider: self.providers[index].kind,
                success,
                message,
                timestamp,
            });
            self.testing = None;
        }
    }

    fn show_info(&mut self) {
        if self.selected < self.providers.len() {
            let provider = &self.providers[self.selected];
            let mut details = vec![
                format!("Provider: {}", provider.display_name),
                format!("Kind: {:?}", provider.kind),
                format!("Status: {:?}", provider.status),
                format!("Base URL: {}", provider.base_url.as_deref().unwrap_or("not set")),
                format!("Model: {}", provider.model.as_deref().unwrap_or("not set")),
                format!("API Key: {}", if provider.has_key { "✓ Configured" } else { "✗ Not configured" }),
                format!("Models: {}", provider.model_count),
                format!("Last Tested: {}", provider.last_tested.as_deref().unwrap_or("never")),
            ];

            if let Some(last_result) = self.test_results.iter()
                .rev()
                .find(|r| r.provider == provider.kind)
            {
                details.push(format!("Last Test: {} - {}", last_result.timestamp, last_result.message));
            }

            self.info_popup = Some(InfoPopup {
                provider: provider.kind,
                details,
            });
        }
    }

    fn render_provider_card(&self, provider: &ProviderCard, selected: bool, area: Rect, frame: &mut Frame) {
        let style = if selected {
            Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let status_style = match provider.status {
            ProviderStatus::Connected => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ProviderStatus::Disconnected => Style::default().fg(Color::Yellow),
            ProviderStatus::Testing => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ProviderStatus::Error(_) => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        };

        let status_text = match &provider.status {
            ProviderStatus::Connected => "● Connected",
            ProviderStatus::Disconnected => "○ Disconnected",
            ProviderStatus::Testing => "◐ Testing...",
            ProviderStatus::Error(msg) => &format!("✗ {}", msg),
        };

        let lines = vec![
            Line::from(vec![
                Span::styled(format!("{}  {}", provider.icon, provider.display_name), style),
            ]),
            Line::from(vec![
                Span::styled(status_text, status_style),
            ]),
            Line::from(vec![
                Span::styled(format!("  Model: {}", provider.model.as_deref().unwrap_or("not set")), style.fg(Color::Gray)),
            ]),
            Line::from(vec![
                Span::styled(format!("  Endpoint: {}", provider.base_url.as_deref().unwrap_or("not set")), style.fg(Color::Gray)),
            ]),
            Line::from(vec![
                Span::styled(format!("  Models: {}  •  Last tested: {}", provider.model_count, provider.last_tested.as_deref().unwrap_or("never")), style.fg(Color::DarkGray)),
            ]),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if selected { Style::default().fg(Color::Yellow) } else { Style::default().fg(Color::DarkGray) })
            .title(if selected { " ▼ " } else { "   " });

        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: true });

        frame.render_widget(paragraph, area);
    }

    fn render_grid(&mut self, frame: &mut Frame, area: Rect) {
        // Calculate grid layout: 2 columns, dynamic rows
        let card_width = 50;
        let card_height = 7;
        let cols = (area.width as usize).saturating_sub(4) / (card_width + 2);
        let cols = cols.max(1).min(3);

        let rows = (self.providers.len() + cols - 1) / cols;

        let constraints: Vec<Constraint> = (0..rows)
            .map(|_| Constraint::Length(card_height as u16))
            .collect();

        let row_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        for row in 0..rows {
            let row_area = row_chunks[row];
            let col_constraints: Vec<Constraint> = (0..cols)
                .map(|_| Constraint::Length(card_width as u16))
                .collect();

            let col_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(col_constraints)
                .margin(1)
                .split(row_area);

            for col in 0..cols {
                let index = row * cols + col;
                if index < self.providers.len() && col < col_chunks.len() {
                    let selected = index == self.selected;
                    self.render_provider_card(&self.providers[index], selected, col_chunks[col], frame);
                }
            }
        }
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.providers.iter().enumerate().map(|(i, p)| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let status_style = match p.status {
                ProviderStatus::Connected => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ProviderStatus::Disconnected => Style::default().fg(Color::Yellow),
                ProviderStatus::Testing => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ProviderStatus::Error(_) => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            };

            let status_text = match &p.status {
                ProviderStatus::Connected => "●",
                ProviderStatus::Disconnected => "○",
                ProviderStatus::Testing => "◐",
                ProviderStatus::Error(_) => "✗",
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!("{}  {}", p.icon, p.display_name), style),
                Span::raw("  "),
                Span::styled(status_text, status_style),
                Span::styled(format!("  {}", p.model.as_deref().unwrap_or("not set")), style.fg(Color::Gray)),
                Span::styled(format!("  ({} models)", p.model_count), style.fg(Color::DarkGray)),
            ]))
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Providers "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD))
            .highlight_symbol("► ");

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_info_popup(&self, frame: &mut Frame, area: Rect) {
        if let Some(popup) = &self.info_popup {
            let popup_area = centered_rect(60, 70, area);
            frame.render_widget(Clear, popup_area);

            let lines: Vec<Line> = popup.details.iter()
                .map(|d| Line::from(Span::raw(d.clone())))
                .collect();

            let paragraph = Paragraph::new(lines)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} Provider Details ", popup.provider))
                    .border_style(Style::default().fg(Color::Cyan)))
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::White));

            frame.render_widget(paragraph, popup_area);
        }
    }

    fn render_editing(&self, frame: &mut Frame, area: Rect) {
        if self.editing {
            let popup_area = centered_rect(50, 20, area);
            frame.render_widget(Clear, popup_area);

            let field_name = match self.edit_field {
                EditField::Model => "Model ID",
                EditField::BaseUrl => "Base URL",
                EditField::ApiKey => "API Key",
                EditField::Kind => "Provider Kind",
            };

            let paragraph = Paragraph::new(self.edit_buffer.as_str())
                .block(Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Editing {} (Enter to save, Esc to cancel) ", field_name))
                    .border_style(Style::default().fg(Color::Yellow)))
                .style(Style::default().fg(Color::White));

            frame.render_widget(paragraph, popup_area);
        }
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

impl Overlay for ConnectOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Connect
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        // Providers will be loaded on first key press
    }

    fn hide(&mut self) {
        self.visible = false;
        self.editing = false;
        self.info_popup = None;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        // Lazy load providers on first interaction
        if self.providers.is_empty() {
            self.load_providers(app);
        }

        // Handle info popup
        if self.info_popup.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('q') => {
                    self.info_popup = None;
                }
                _ => {}
            }
            return OverlayAction::None;
        }

        // Handle editing
        if self.editing {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc => {
                    self.editing = false;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                }
                KeyCode::Enter => {
                    self.apply_edit(app);
                    self.editing = false;
                }
                KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                    self.edit_buffer.insert(self.edit_cursor, ch);
                    self.edit_cursor += 1;
                }
                KeyCode::Backspace => {
                    if self.edit_cursor > 0 {
                        self.edit_cursor -= 1;
                        self.edit_buffer.remove(self.edit_cursor);
                    }
                }
                KeyCode::Delete => {
                    if self.edit_cursor < self.edit_buffer.len() {
                        self.edit_buffer.remove(self.edit_cursor);
                    }
                }
                KeyCode::Left => {
                    if self.edit_cursor > 0 { self.edit_cursor -= 1; }
                }
                KeyCode::Right => {
                    if self.edit_cursor < self.edit_buffer.len() { self.edit_cursor += 1; }
                }
                KeyCode::Home => { self.edit_cursor = 0; }
                KeyCode::End => { self.edit_cursor = self.edit_buffer.len(); }
                _ => {}
            }
            return OverlayAction::None;
        }

        // Check for test results
        if let Some(event) = app.try_recv_provider_test_result() {
            self.handle_test_result(event.index, event.success, event.message, event.model_count, event.timestamp);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return OverlayAction::Close,
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Down => {
                if self.selected + 1 < self.providers.len() {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Left => {
                // Navigate grid left
                if self.selected % 2 > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Right => {
                // Navigate grid right
                if self.selected % 2 == 0 && self.selected + 1 < self.providers.len() {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(4);
                self.list_state.select(Some(self.selected));
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + 4).min(self.providers.len().saturating_sub(1));
                self.list_state.select(Some(self.selected));
            }
            KeyCode::Home => {
                self.selected = 0;
                self.list_state.select(Some(0));
            }
            KeyCode::End => {
                self.selected = self.providers.len().saturating_sub(1);
                self.list_state.select(Some(self.selected));
            }
            KeyCode::Enter => {
                self.start_edit();
            }
            KeyCode::Char('d') => {
                // Disconnect: clear the provider config
                if self.selected < self.providers.len() {
                    self.providers[self.selected].status = ProviderStatus::Disconnected;
                    self.providers[self.selected].has_key = false;
                    self.providers[self.selected].model = None;
                    self.providers[self.selected].base_url = None;
                    self.providers[self.selected].model_count = 0;
                    self.providers[self.selected].last_tested = None;
                    app.push(crate::tui::app::EntryRole::Info, format!("Disconnected {}", self.providers[self.selected].display_name));
                }
            }
            KeyCode::Char('t') => {
                self.test_connection(app);
            }
            KeyCode::Char('i') => {
                self.show_info();
            }
            KeyCode::Char('r') => {
                // Refresh - reload providers
                self.load_providers(app);
                app.push(crate::tui::app::EntryRole::Info, "Provider list refreshed".to_string());
            }
            KeyCode::Char('1') => {
                if self.selected < self.providers.len() {
                    self.edit_field = EditField::Model;
                    self.start_edit();
                }
            }
            KeyCode::Char('2') => {
                if self.selected < self.providers.len() {
                    self.edit_field = EditField::BaseUrl;
                    self.edit_buffer = self.providers[self.selected].base_url.clone().unwrap_or_default();
                    self.edit_cursor = self.edit_buffer.len();
                    self.editing = true;
                }
            }
            KeyCode::Char('3') => {
                if self.selected < self.providers.len() {
                    self.edit_field = EditField::ApiKey;
                    self.edit_buffer.clear();
                    self.edit_cursor = 0;
                    self.editing = true;
                }
            }
            KeyCode::Tab => {
                // Switch between grid and list view (could add view mode state)
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        if !self.visible {
            return;
        }

        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Title
                Constraint::Min(0),     // Content
                Constraint::Length(2),  // Help footer
            ])
            .split(area);

        // Title
        let title = Paragraph::new("Model Provider Connections")
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, chunks[0]);

        // Content - use grid layout for wider screens, list for narrow
        if area.width >= 110 {
            self.render_grid(frame, chunks[1]);
        } else {
            self.render_list(frame, chunks[1]);
        }

        // Help footer
        let help_text = if self.editing {
            "enter: save · esc: cancel · ←/→: move cursor"
        } else if self.info_popup.is_some() {
            "esc/i/q: close info"
        } else {
            "↑↓←→: navigate · enter: edit model · 1: model · 2: url · 3: key · d: disconnect · t: test · i: info · r: refresh · esc: close"
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);

        // Render popups on top
        self.render_editing(frame, area);
        self.render_info_popup(frame, area);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/connect", "Open provider connections overlay"),
            ("↑/↓/←/→", "Navigate provider cards"),
            ("Enter", "Edit selected provider model"),
            ("1/2/3", "Edit model / base URL / API key"),
            ("D", "Disconnect provider"),
            ("T", "Test provider connection"),
            ("I", "Show provider details"),
            ("R", "Refresh provider list"),
            ("Esc", "Close overlay"),
        ]
    }
}
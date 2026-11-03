//! Model overlay: view and configure model settings, providers, parameters.

use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Default)]
pub struct ModelOverlay {
    visible: bool,
    tabs: Vec<ModelTab>,
    active_tab: usize,
    providers: Vec<ProviderEntry>,
    models: Vec<ModelEntry>,
    settings: ModelSettings,
    selected: usize,
    list_state: ListState,
    editing: bool,
    edit_field: EditField,
    edit_buffer: String,
    edit_cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelTab {
    Providers,
    Models,
    Settings,
    Status,
}

#[derive(Debug, Clone)]
struct ProviderEntry {
    name: String,
    enabled: bool,
    models: usize,
    default: bool,
    status: ProviderStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderStatus {
    Connected,
    Disconnected,
    Error,
    Unknown,
}

#[derive(Debug, Clone)]
struct ModelEntry {
    name: String,
    provider: String,
    context_window: usize,
    max_output: usize,
    supports_tools: bool,
    supports_vision: bool,
    pricing: Option<ModelPricing>,
    is_default: bool,
}

#[derive(Debug, Clone)]
struct ModelPricing {
    input_per_m: f64,
    output_per_m: f64,
}

#[derive(Debug, Clone, Default)]
struct ModelSettings {
    temperature: f32,
    top_p: f32,
    max_tokens: Option<usize>,
    system_prompt: String,
    timeout_seconds: u64,
    retry_attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    Temperature,
    TopP,
    MaxTokens,
    SystemPrompt,
    Timeout,
    RetryAttempts,
}

impl ModelOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            tabs: vec![ModelTab::Providers, ModelTab::Models, ModelTab::Settings, ModelTab::Status],
            list_state: state,
            settings: ModelSettings {
                temperature: 0.7,
                top_p: 0.95,
                max_tokens: Some(4096),
                system_prompt: "You are a helpful coding assistant.".to_string(),
                timeout_seconds: 120,
                retry_attempts: 3,
            },
            ..Default::default()
        }
    }

    fn load_mock_data(&mut self) {
        self.providers = vec![
            ProviderEntry {
                name: "openai".to_string(),
                enabled: true,
                models: 6,
                default: true,
                status: ProviderStatus::Connected,
            },
            ProviderEntry {
                name: "anthropic".to_string(),
                enabled: true,
                models: 4,
                default: false,
                status: ProviderStatus::Connected,
            },
            ProviderEntry {
                name: "ollama".to_string(),
                enabled: false,
                models: 0,
                default: false,
                status: ProviderStatus::Disconnected,
            },
            ProviderEntry {
                name: "groq".to_string(),
                enabled: true,
                models: 3,
                default: false,
                status: ProviderStatus::Connected,
            },
        ];

        self.models = vec![
            ModelEntry {
                name: "gpt-4o".to_string(),
                provider: "openai".to_string(),
                context_window: 128000,
                max_output: 4096,
                supports_tools: true,
                supports_vision: true,
                pricing: Some(ModelPricing { input_per_m: 5.0, output_per_m: 15.0 }),
                is_default: true,
            },
            ModelEntry {
                name: "gpt-4o-mini".to_string(),
                provider: "openai".to_string(),
                context_window: 128000,
                max_output: 4096,
                supports_tools: true,
                supports_vision: true,
                pricing: Some(ModelPricing { input_per_m: 0.15, output_per_m: 0.6 }),
                is_default: false,
            },
            ModelEntry {
                name: "claude-3-5-sonnet".to_string(),
                provider: "anthropic".to_string(),
                context_window: 200000,
                max_output: 8192,
                supports_tools: true,
                supports_vision: true,
                pricing: Some(ModelPricing { input_per_m: 3.0, output_per_m: 15.0 }),
                is_default: false,
            },
            ModelEntry {
                name: "claude-3-haiku".to_string(),
                provider: "anthropic".to_string(),
                context_window: 200000,
                max_output: 4096,
                supports_tools: true,
                supports_vision: true,
                pricing: Some(ModelPricing { input_per_m: 0.25, output_per_m: 1.25 }),
                is_default: false,
            },
            ModelEntry {
                name: "llama-3.1-70b".to_string(),
                provider: "groq".to_string(),
                context_window: 131072,
                max_output: 8192,
                supports_tools: true,
                supports_vision: false,
                pricing: None,
                is_default: false,
            },
        ];
    }

    fn render_providers(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.providers.iter().enumerate().map(|(i, p)| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let status_char = match p.status {
                ProviderStatus::Connected => "●",
                ProviderStatus::Disconnected => "○",
                ProviderStatus::Error => "✗",
                ProviderStatus::Unknown => "?",
            };
            let status_color = match p.status {
                ProviderStatus::Connected => Color::Green,
                ProviderStatus::Disconnected => Color::DarkGray,
                ProviderStatus::Error => Color::Red,
                ProviderStatus::Unknown => Color::Yellow,
            };

            let enabled_mark = if p.enabled { "✓" } else { "✗" };
            let default_mark = if p.default { " ★" } else { "" };

            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", enabled_mark), Style::default().fg(if p.enabled { Color::Green } else { Color::Red })),
                    Span::styled(&p.name, style),
                    Span::styled(default_mark, style.fg(Color::Yellow)),
                    Span::styled(format!("  ({} models)", p.models), style.fg(Color::DarkGray)),
                ]),
                Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(status_char, Style::default().fg(status_color)),
                    Span::styled(format!("  {:?}", p.status), style.fg(Color::Gray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Providers "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_models(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.models.iter().enumerate().map(|(i, m)| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let default_mark = if m.is_default { " ★" } else { "" };
            let tools_mark = if m.supports_tools { " 🔧" } else { "" };
            let vision_mark = if m.supports_vision { " 👁" } else { "" };

            let pricing_str = m.pricing.map_or("free".to_string(), |p| {
                format!("${:.2}/${:.2} per M", p.input_per_m, p.output_per_m)
            });

            let lines = vec![
                Line::from(vec![
                    Span::styled(&m.name, style),
                    Span::styled(default_mark, style.fg(Color::Yellow)),
                    Span::styled(format!("  [{}]", m.provider), style.fg(Color::Cyan)),
                    Span::styled(tools_mark, style.fg(Color::Green)),
                    Span::styled(vision_mark, style.fg(Color::Blue)),
                ]),
                Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(format!("Context: {}k  Output: {}k  {}", 
                        m.context_window / 1000, m.max_output / 1000, pricing_str), style.fg(Color::Gray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Models "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_settings(&mut self, frame: &mut Frame, area: Rect) {
        let settings = vec![
            ("Temperature", self.settings.temperature.to_string(), "0.0-2.0"),
            ("Top-p", self.settings.top_p.to_string(), "0.0-1.0"),
            ("Max Tokens", self.settings.max_tokens.map_or("unlimited".to_string(), |v| v.to_string()), "optional"),
            ("System Prompt", if self.settings.system_prompt.len() > 50 { format!("{}...", &self.settings.system_prompt[..47]) } else { self.settings.system_prompt.clone() }, "string"),
            ("Timeout (s)", self.settings.timeout_seconds.to_string(), "seconds"),
            ("Retry Attempts", self.settings.retry_attempts.to_string(), "count"),
        ];

        let items: Vec<ListItem> = settings.iter().enumerate().map(|(i, (name, value, hint))| {
            let selected = i == self.selected;
            let is_editing = self.editing && self.selected == i;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let display_value = if is_editing {
                format!("{}_", self.edit_buffer)
            } else {
                value.clone()
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{}: ", name), style.fg(Color::Cyan)),
                    Span::styled(display_value, style),
                    Span::styled(format!("  ({})", hint), style.fg(Color::DarkGray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Settings "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_status(&mut self, frame: &mut Frame, area: Rect) {
        let status_text = vec![
            Line::from(vec![
                Span::styled("Model Configuration Status", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Default Provider: ", Style::default().fg(Color::Cyan)),
                Span::styled("openai (connected)", Style::default().fg(Color::Green)),
            ]),
            Line::from(vec![
                Span::styled("Default Model: ", Style::default().fg(Color::Cyan)),
                Span::styled("gpt-4o", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Temperature: ", Style::default().fg(Color::Cyan)),
                Span::styled(self.settings.temperature.to_string(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("Top-p: ", Style::default().fg(Color::Cyan)),
                Span::styled(self.settings.top_p.to_string(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("Max Tokens: ", Style::default().fg(Color::Cyan)),
                Span::styled(self.settings.max_tokens.map_or("unlimited".to_string(), |v| v.to_string()), Style::default().fg(Color::White)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Available Providers:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]),
            Line::from("  ✓ openai - Connected (6 models)"),
            Line::from("  ✓ anthropic - Connected (4 models)"),
            Line::from("  ✗ ollama - Disconnected"),
            Line::from("  ✓ groq - Connected (3 models)"),
            Line::from(""),
            Line::from(vec![
                Span::styled("Press 'r' to refresh, Tab for settings", Style::default().fg(Color::DarkGray)),
            ]),
        ];

        let status = Paragraph::new(status_text)
            .style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title(" Status "))
            .wrap(Wrap { trim: true });
        frame.render_widget(status, area);
    }

    fn start_edit(&mut self) {
        match self.active_tab {
            ModelTab::Settings => {
                self.editing = true;
                self.edit_cursor = 0;
                match self.selected {
                    0 => {
                        self.edit_field = EditField::Temperature;
                        self.edit_buffer = self.settings.temperature.to_string();
                    }
                    1 => {
                        self.edit_field = EditField::TopP;
                        self.edit_buffer = self.settings.top_p.to_string();
                    }
                    2 => {
                        self.edit_field = EditField::MaxTokens;
                        self.edit_buffer = self.settings.max_tokens.map_or("".to_string(), |v| v.to_string());
                    }
                    3 => {
                        self.edit_field = EditField::SystemPrompt;
                        self.edit_buffer = self.settings.system_prompt.clone();
                    }
                    4 => {
                        self.edit_field = EditField::Timeout;
                        self.edit_buffer = self.settings.timeout_seconds.to_string();
                    }
                    5 => {
                        self.edit_field = EditField::RetryAttempts;
                        self.edit_buffer = self.settings.retry_attempts.to_string();
                    }
                    _ => self.editing = false,
                }
            }
            _ => {}
        }
    }

    fn apply_edit(&mut self) {
        match self.edit_field {
            EditField::Temperature => {
                if let Ok(v) = self.edit_buffer.parse::<f32>() {
                    self.settings.temperature = v.clamp(0.0, 2.0);
                }
            }
            EditField::TopP => {
                if let Ok(v) = self.edit_buffer.parse::<f32>() {
                    self.settings.top_p = v.clamp(0.0, 1.0);
                }
            }
            EditField::MaxTokens => {
                if self.edit_buffer.is_empty() {
                    self.settings.max_tokens = None;
                } else if let Ok(v) = self.edit_buffer.parse::<usize>() {
                    self.settings.max_tokens = Some(v);
                }
            }
            EditField::SystemPrompt => {
                self.settings.system_prompt = self.edit_buffer.clone();
            }
            EditField::Timeout => {
                if let Ok(v) = self.edit_buffer.parse::<u64>() {
                    self.settings.timeout_seconds = v;
                }
            }
            EditField::RetryAttempts => {
                if let Ok(v) = self.edit_buffer.parse::<u32>() {
                    self.settings.retry_attempts = v;
                }
            }
        }
    }

    fn toggle_setting(&mut self) {
        match self.active_tab {
            ModelTab::Providers => {
                if let Some(p) = self.providers.get_mut(self.selected) {
                    p.enabled = !p.enabled;
                }
            }
            _ => {}
        }
    }
}

impl Overlay for ModelOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Model
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.load_mock_data();
        self.active_tab = 0;
        self.selected = 0;
        self.list_state.select(Some(0));
        self.editing = false;
    }

    fn hide(&mut self) {
        self.visible = false;
        self.editing = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Handle editing mode
        if self.editing {
            match key.code {
                KeyCode::Esc => {
                    self.editing = false;
                    self.edit_buffer.clear();
                }
                KeyCode::Enter => {
                    self.apply_edit();
                    self.editing = false;
                    self.edit_buffer.clear();
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

        match key.code {
            KeyCode::Esc => return OverlayAction::Close,
            KeyCode::Char('q') => return OverlayAction::Close,
            KeyCode::Tab => {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
                self.selected = 0;
                self.list_state.select(Some(0));
            }
            KeyCode::BackTab => {
                self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                self.selected = 0;
                self.list_state.select(Some(0));
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Down => {
                let len = match self.active_tab {
                    ModelTab::Providers => self.providers.len(),
                    ModelTab::Models => self.models.len(),
                    ModelTab::Settings => 6,
                    ModelTab::Status => 0,
                };
                if self.selected + 1 < len {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(10);
                self.list_state.select(Some(self.selected));
            }
            KeyCode::PageDown => {
                let len = match self.active_tab {
                    ModelTab::Providers => self.providers.len(),
                    ModelTab::Models => self.models.len(),
                    ModelTab::Settings => 6,
                    ModelTab::Status => 0,
                };
                self.selected = (self.selected + 10).min(len.saturating_sub(1));
                self.list_state.select(Some(self.selected));
            }
            KeyCode::Home => {
                self.selected = 0;
                self.list_state.select(Some(0));
            }
            KeyCode::End => {
                let len = match self.active_tab {
                    ModelTab::Providers => self.providers.len(),
                    ModelTab::Models => self.models.len(),
                    ModelTab::Settings => 6,
                    ModelTab::Status => 0,
                };
                self.selected = len.saturating_sub(1);
                self.list_state.select(Some(self.selected));
            }
            KeyCode::Enter => {
                self.start_edit();
            }
            KeyCode::Char(' ') => {
                // Toggle boolean settings
                self.toggle_setting();
            }
            KeyCode::Char('t') => {
                // Test connection
                if self.active_tab == ModelTab::Providers {
                    if let Some(p) = self.providers.get(self.selected) {
                        app.push(crate::tui::app::EntryRole::Info, format!("Testing {}...", p.name));
                    }
                }
            }
            KeyCode::Char('d') => {
                // Set as default
                match self.active_tab {
                    ModelTab::Providers => if let Some(p) = self.providers.get(self.selected) {
                        app.push(crate::tui::app::EntryRole::Info, format!("Set {} as default provider", p.name));
                    },
                    ModelTab::Models => if let Some(m) = self.models.get(self.selected) {
                        app.push(crate::tui::app::EntryRole::Info, format!("Set {} as default model", m.name));
                    },
                    _ => {}
                }
            }
            KeyCode::Char('c') if ctrl => {
                // Copy
                match self.active_tab {
                    ModelTab::Providers => if let Some(p) = self.providers.get(self.selected) {
                        app.push(crate::tui::app::EntryRole::Info, format!("Copied provider: {}", p.name));
                    },
                    ModelTab::Models => if let Some(m) = self.models.get(self.selected) {
                        app.push(crate::tui::app::EntryRole::Info, format!("Copied model: {}", m.name));
                    },
                    _ => {}
                }
            }
            KeyCode::Char('r') => {
                // Refresh
                self.load_mock_data();
                app.push(crate::tui::app::EntryRole::Info, "Model data refreshed".to_string());
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Tabs
                Constraint::Min(0),     // Content
                Constraint::Length(2),  // Help footer
            ])
            .split(area);

        // Tabs
        let tab_titles: Vec<&str> = self.tabs.iter().map(|t| match t {
            ModelTab::Providers => "Providers",
            ModelTab::Models => "Models",
            ModelTab::Settings => "Settings",
            ModelTab::Status => "Status",
        }).collect();
        let tabs = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::ALL).title(" Model "))
            .select(self.active_tab)
            .style(Style::default().fg(Color::Gray))
            .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        frame.render_widget(tabs, chunks[0]);

        // Content
        match self.active_tab {
            ModelTab::Providers => self.render_providers(frame, chunks[1]),
            ModelTab::Models => self.render_models(frame, chunks[1]),
            ModelTab::Settings => self.render_settings(frame, chunks[1]),
            ModelTab::Status => self.render_status(frame, chunks[1]),
        }

        // Help footer
        let help_text = if self.editing {
            "enter: save · esc: cancel"
        } else {
            match self.active_tab {
                ModelTab::Providers => "↑↓: navigate · enter: edit · space: toggle · d: default · t: test · c: copy · r: refresh · tab: next · esc: close",
                ModelTab::Models => "↑↓: navigate · d: default · c: copy · tab: next · esc: close",
                ModelTab::Settings => "↑↓: navigate · enter: edit · space: toggle · tab: next · esc: close",
                ModelTab::Status => "tab: next · esc: close",
            }
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/model", "Open model overlay"),
            ("Tab", "Switch tabs (Providers/Models/Settings/Status)"),
            ("↑/↓", "Navigate"),
            ("Enter", "Edit setting"),
            ("Space", "Toggle enabled/default"),
            ("D", "Set as default"),
            ("T", "Test provider connection"),
            ("Ctrl+C", "Copy entry"),
            ("R", "Refresh"),
            ("Esc", "Close overlay"),
        ]
    }
}
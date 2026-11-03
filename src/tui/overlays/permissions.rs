//! Permissions overlay: view and manage tool permissions, sandbox policies.

use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

#[derive(Debug, Default)]
pub struct PermissionsOverlay {
    visible: bool,
    tabs: Vec<PermissionTab>,
    active_tab: usize,
    tools: Vec<ToolPermission>,
    policies: Vec<SandboxPolicy>,
    requests: Vec<PermissionRequest>,
    selected: usize,
    list_state: ListState,
    detail_view: bool,
    detail_content: String,
    mode: PermissionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PermissionMode {
    #[default]
    Browse,
    ConfirmAllow,
    ConfirmDeny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionTab {
    Tools,
    Policies,
    Requests,
    Audit,
}

#[derive(Debug, Clone)]
struct ToolPermission {
    name: String,
    description: String,
    allowed: PermissionState,
    risk_level: RiskLevel,
    last_used: Option<String>,
    use_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionState {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone)]
struct SandboxPolicy {
    name: String,
    description: String,
    enabled: bool,
    rules: Vec<String>,
}

#[derive(Debug, Clone)]
struct PermissionRequest {
    id: String,
    tool: String,
    arguments: String,
    timestamp: String,
    status: RequestStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

impl PermissionsOverlay {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            tabs: vec![PermissionTab::Tools, PermissionTab::Policies, PermissionTab::Requests, PermissionTab::Audit],
            list_state: state,
            ..Default::default()
        }
    }

    /// The tab the cursor is on. `active_tab` stays an index because `Tabs`
    /// takes one; this is where that index becomes a variant.
    fn tab(&self) -> PermissionTab {
        self.tabs
            .get(self.active_tab)
            .copied()
            .unwrap_or(PermissionTab::Tools)
    }

    fn load_mock_data(&mut self) {
        self.tools = vec![
            ToolPermission {
                name: "vault_read".to_string(),
                description: "Read facts and decisions from the vault".to_string(),
                allowed: PermissionState::Allow,
                risk_level: RiskLevel::Low,
                last_used: Some("2m ago".to_string()),
                use_count: 142,
            },
            ToolPermission {
                name: "vault_write".to_string(),
                description: "Write facts, decisions, and corrections to the vault".to_string(),
                allowed: PermissionState::Ask,
                risk_level: RiskLevel::Medium,
                last_used: Some("15m ago".to_string()),
                use_count: 23,
            },
            ToolPermission {
                name: "kg_search".to_string(),
                description: "Search the code graph for symbols and definitions".to_string(),
                allowed: PermissionState::Allow,
                risk_level: RiskLevel::Low,
                last_used: Some("5m ago".to_string()),
                use_count: 89,
            },
            ToolPermission {
                name: "kg_impact".to_string(),
                description: "Analyze impact of changes (refs, tests, dependents)".to_string(),
                allowed: PermissionState::Allow,
                risk_level: RiskLevel::Low,
                last_used: Some("1h ago".to_string()),
                use_count: 12,
            },
            ToolPermission {
                name: "file_read".to_string(),
                description: "Read files from the workspace".to_string(),
                allowed: PermissionState::Allow,
                risk_level: RiskLevel::Low,
                last_used: Some("just now".to_string()),
                use_count: 567,
            },
            ToolPermission {
                name: "file_write".to_string(),
                description: "Write files to the workspace".to_string(),
                allowed: PermissionState::Ask,
                risk_level: RiskLevel::High,
                last_used: Some("30m ago".to_string()),
                use_count: 34,
            },
            ToolPermission {
                name: "shell_exec".to_string(),
                description: "Execute shell commands".to_string(),
                allowed: PermissionState::Deny,
                risk_level: RiskLevel::Critical,
                last_used: None,
                use_count: 0,
            },
            ToolPermission {
                name: "network_request".to_string(),
                description: "Make HTTP requests to external services".to_string(),
                allowed: PermissionState::Ask,
                risk_level: RiskLevel::High,
                last_used: Some("2h ago".to_string()),
                use_count: 5,
            },
        ];

        self.policies = vec![
            SandboxPolicy {
                name: "default".to_string(),
                description: "Default sandbox policy for model turns".to_string(),
                enabled: true,
                rules: vec![
                    "Allow: vault_read, kg_search, kg_impact, file_read".to_string(),
                    "Ask: vault_write, file_write, network_request".to_string(),
                    "Deny: shell_exec".to_string(),
                ],
            },
            SandboxPolicy {
                name: "readonly".to_string(),
                description: "Read-only policy for safe exploration".to_string(),
                enabled: false,
                rules: vec![
                    "Allow: vault_read, kg_search, kg_impact, file_read".to_string(),
                    "Deny: vault_write, file_write, shell_exec, network_request".to_string(),
                ],
            },
            SandboxPolicy {
                name: "full-access".to_string(),
                description: "Full access policy for trusted sessions".to_string(),
                enabled: false,
                rules: vec![
                    "Allow: all tools".to_string(),
                ],
            },
        ];

        self.requests = vec![
            PermissionRequest {
                id: "req-001".to_string(),
                tool: "file_write".to_string(),
                arguments: "path: src/main.rs, content: ... (120 chars)".to_string(),
                timestamp: "14:32:15".to_string(),
                status: RequestStatus::Approved,
            },
            PermissionRequest {
                id: "req-002".to_string(),
                tool: "vault_write".to_string(),
                arguments: "name: decision, kind: decision, fact: Use event sourcing".to_string(),
                timestamp: "14:29:42".to_string(),
                status: RequestStatus::Approved,
            },
            PermissionRequest {
                id: "req-003".to_string(),
                tool: "shell_exec".to_string(),
                arguments: "command: cargo test".to_string(),
                timestamp: "14:15:00".to_string(),
                status: RequestStatus::Denied,
            },
            PermissionRequest {
                id: "req-004".to_string(),
                tool: "network_request".to_string(),
                arguments: "url: https://api.github.com/repos/...".to_string(),
                timestamp: "12:45:30".to_string(),
                status: RequestStatus::Expired,
            },
        ];
    }

    fn render_tools(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.tools.iter().enumerate().map(|(i, t)| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let perm_char = match t.allowed {
                PermissionState::Allow => "✓",
                PermissionState::Deny => "✗",
                PermissionState::Ask => "?",
            };
            let perm_color = match t.allowed {
                PermissionState::Allow => Color::Green,
                PermissionState::Deny => Color::Red,
                PermissionState::Ask => Color::Yellow,
            };

            let risk_char = match t.risk_level {
                RiskLevel::Low => "L",
                RiskLevel::Medium => "M",
                RiskLevel::High => "H",
                RiskLevel::Critical => "C",
            };
            let risk_color = match t.risk_level {
                RiskLevel::Low => Color::Green,
                RiskLevel::Medium => Color::Yellow,
                RiskLevel::High => Color::Red,
                RiskLevel::Critical => Color::Magenta,
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", perm_char), Style::default().fg(perm_color)),
                    Span::styled(&t.name, style),
                    Span::styled(format!("  [{}]", risk_char), Style::default().fg(risk_color).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  ({} uses)", t.use_count), style.fg(Color::DarkGray)),
                ]),
                Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(&t.description, style.fg(Color::Gray)),
                    Span::styled(t.last_used.as_deref().map(|s| format!("  ({})", s)).unwrap_or("  (never)".to_string()), style.fg(Color::DarkGray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Tool Permissions "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_policies(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.policies.iter().enumerate().map(|(i, p)| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let enabled_char = if p.enabled { "●" } else { "○" };
            let enabled_color = if p.enabled { Color::Green } else { Color::DarkGray };

            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", enabled_char), Style::default().fg(enabled_color)),
                    Span::styled(&p.name, style),
                    Span::styled(if p.enabled { " (active)" } else { "" }, style.fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(&p.description, style.fg(Color::Gray)),
                    Span::styled(format!("  {} rules", p.rules.len()), style.fg(Color::DarkGray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Sandbox Policies "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_requests(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self.requests.iter().enumerate().map(|(i, r)| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let status_char = match r.status {
                RequestStatus::Pending => "⏳",
                RequestStatus::Approved => "✓",
                RequestStatus::Denied => "✗",
                RequestStatus::Expired => "⌛",
            };
            let status_color = match r.status {
                RequestStatus::Pending => Color::Yellow,
                RequestStatus::Approved => Color::Green,
                RequestStatus::Denied => Color::Red,
                RequestStatus::Expired => Color::DarkGray,
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", status_char), Style::default().fg(status_color)),
                    Span::styled(&r.tool, style),
                    Span::styled(format!("  {}", r.id), style.fg(Color::DarkGray)),
                    Span::styled(format!("  {}", r.timestamp), style.fg(Color::DarkGray)),
                ]),
                Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(&r.arguments, style.fg(Color::Gray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Permission Requests "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_audit(&mut self, frame: &mut Frame, area: Rect) {
        let audit_entries = vec![
            ("14:32:15", "file_write", "ALLOWED", "src/main.rs"),
            ("14:29:42", "vault_write", "ALLOWED", "architecture decision"),
            ("14:15:00", "shell_exec", "DENIED", "cargo test"),
            ("12:45:30", "network_request", "EXPIRED", "api.github.com"),
            ("12:30:00", "file_read", "ALLOWED", "Cargo.toml"),
            ("12:25:00", "kg_search", "ALLOWED", "event sourcing"),
            ("12:20:00", "vault_read", "ALLOWED", "decisions"),
        ];

        let items: Vec<ListItem> = audit_entries.iter().enumerate().map(|(i, (time, tool, result, target))| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let result_color = match *result {
                "ALLOWED" => Color::Green,
                "DENIED" => Color::Red,
                "EXPIRED" => Color::DarkGray,
                _ => Color::White,
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(*time, Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("  {}", tool), style.fg(Color::Cyan)),
                    Span::styled(format!("  {}", result), Style::default().fg(result_color).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {}", target), style.fg(Color::Gray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Audit Log "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }
}

impl Overlay for PermissionsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Permissions
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
        self.detail_view = false;
        self.mode = PermissionMode::Browse;
    }

    fn hide(&mut self) {
        self.visible = false;
        self.detail_view = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Handle confirmation modes
        if matches!(self.mode, PermissionMode::ConfirmAllow | PermissionMode::ConfirmDeny) {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    if let Some(req) = self.requests.get(self.selected) {
                        let action = if self.mode == PermissionMode::ConfirmAllow { "Approved" } else { "Denied" };
                        app.push(crate::tui::app::EntryRole::Info, format!("{} request for {}", action, req.tool));
                    }
                    self.mode = PermissionMode::Browse;
                    self.detail_view = false;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.mode = PermissionMode::Browse;
                    self.detail_view = false;
                }
                _ => {}
            }
            return OverlayAction::None;
        }

        match key.code {
            KeyCode::Esc => {
                if self.detail_view {
                    self.detail_view = false;
                    self.detail_content.clear();
                } else {
                    return OverlayAction::Close;
                }
            }
            KeyCode::Tab => {
                if !self.detail_view {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    self.selected = 0;
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::BackTab => {
                if !self.detail_view {
                    self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                    self.selected = 0;
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::Up => {
                if !self.detail_view && self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Down => {
                if !self.detail_view {
                    let len = match self.tab() {
                        PermissionTab::Tools => self.tools.len(),
                        PermissionTab::Policies => self.policies.len(),
                        PermissionTab::Requests => self.requests.len(),
                        PermissionTab::Audit => 10,
                    };
                    if self.selected + 1 < len {
                        self.selected += 1;
                        self.list_state.select(Some(self.selected));
                    }
                }
            }
            KeyCode::PageUp => {
                if !self.detail_view {
                    self.selected = self.selected.saturating_sub(10);
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::PageDown => {
                if !self.detail_view {
                    let len = match self.tab() {
                        PermissionTab::Tools => self.tools.len(),
                        PermissionTab::Policies => self.policies.len(),
                        PermissionTab::Requests => self.requests.len(),
                        PermissionTab::Audit => 10,
                    };
                    self.selected = (self.selected + 10).min(len.saturating_sub(1));
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Home => {
                if !self.detail_view {
                    self.selected = 0;
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::End => {
                if !self.detail_view {
                    let len = match self.tab() {
                        PermissionTab::Tools => self.tools.len(),
                        PermissionTab::Policies => self.policies.len(),
                        PermissionTab::Requests => self.requests.len(),
                        PermissionTab::Audit => 10,
                    };
                    self.selected = len.saturating_sub(1);
                    self.list_state.select(Some(self.selected));
                }
            }
            KeyCode::Enter => {
                if !self.detail_view {
                    self.detail_view = true;
                    match self.tab() {
                        PermissionTab::Tools => {
                            if let Some(t) = self.tools.get(self.selected) {
                                self.detail_content = format!(
                                    "Tool: {}\nDescription: {}\nPermission: {:?}\nRisk: {:?}\nLast used: {}\nUse count: {}",
                                    t.name, t.description, t.allowed, t.risk_level,
                                    t.last_used.as_deref().unwrap_or("never"),
                                    t.use_count
                                );
                            }
                        }
                        PermissionTab::Policies => {
                            if let Some(p) = self.policies.get(self.selected) {
                                self.detail_content = format!(
                                    "Policy: {}\nDescription: {}\nEnabled: {}\n\nRules:\n{}",
                                    p.name, p.description, p.enabled,
                                    p.rules.join("\n")
                                );
                            }
                        }
                        PermissionTab::Requests => {
                            if let Some(r) = self.requests.get(self.selected) {
                                self.detail_content = format!(
                                    "Request: {}\nTool: {}\nArguments: {}\nTime: {}\nStatus: {:?}",
                                    r.id, r.tool, r.arguments, r.timestamp, r.status
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('a') => {
                // Allow / Approve
                if !self.detail_view {
                    match self.tab() {
                        PermissionTab::Tools => {
                            if let Some(t) = self.tools.get_mut(self.selected) {
                                t.allowed = PermissionState::Allow;
                                app.push(crate::tui::app::EntryRole::Info, format!("Allowed: {}", t.name));
                            }
                        }
                        PermissionTab::Requests => {
                            if let Some(r) = self.requests.get(self.selected) {
                                if r.status == RequestStatus::Pending {
                                    self.mode = PermissionMode::ConfirmAllow;
                                    self.detail_view = true;
                                    self.detail_content = format!("Allow request {} for {}?\n\ny: confirm · n: cancel", r.id, r.tool);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('d') => {
                // Deny
                if !self.detail_view {
                    match self.tab() {
                        PermissionTab::Tools => {
                            if let Some(t) = self.tools.get_mut(self.selected) {
                                t.allowed = PermissionState::Deny;
                                app.push(crate::tui::app::EntryRole::Info, format!("Denied: {}", t.name));
                            }
                        }
                        PermissionTab::Requests => {
                            if let Some(r) = self.requests.get(self.selected) {
                                if r.status == RequestStatus::Pending {
                                    self.mode = PermissionMode::ConfirmDeny;
                                    self.detail_view = true;
                                    self.detail_content = format!("Deny request {} for {}?\n\ny: confirm · n: cancel", r.id, r.tool);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('k') => {
                // Ask
                if !self.detail_view && self.tab() == PermissionTab::Tools {
                    if let Some(t) = self.tools.get_mut(self.selected) {
                        t.allowed = PermissionState::Ask;
                        app.push(crate::tui::app::EntryRole::Info, format!("Set to ask: {}", t.name));
                    }
                }
            }
            KeyCode::Char('c') if ctrl => {
                // Copy
                if !self.detail_view {
                    match self.tab() {
                        PermissionTab::Tools => if let Some(t) = self.tools.get(self.selected) {
                            app.push(crate::tui::app::EntryRole::Info, format!("Copied tool: {}", t.name));
                        },
                        PermissionTab::Policies => if let Some(p) = self.policies.get(self.selected) {
                            app.push(crate::tui::app::EntryRole::Info, format!("Copied policy: {}", p.name));
                        },
                        PermissionTab::Requests => if let Some(r) = self.requests.get(self.selected) {
                            app.push(crate::tui::app::EntryRole::Info, format!("Copied request: {}", r.id));
                        },
                        _ => {}
                    }
                }
            }
            KeyCode::Char('e') => {
                // Enable/disable policy
                if !self.detail_view && self.tab() == PermissionTab::Policies {
                    if let Some(p) = self.policies.get_mut(self.selected) {
                        p.enabled = !p.enabled;
                        app.push(crate::tui::app::EntryRole::Info, format!("Policy {}: {}", p.name, if p.enabled { "enabled" } else { "disabled" }));
                    }
                }
            }
            KeyCode::Char('r') => {
                // Refresh
                self.load_mock_data();
                app.push(crate::tui::app::EntryRole::Info, "Permissions refreshed".to_string());
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
            PermissionTab::Tools => "Tools",
            PermissionTab::Policies => "Policies",
            PermissionTab::Requests => "Requests",
            PermissionTab::Audit => "Audit",
        }).collect();
        let tabs = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::ALL).title(" Permissions "))
            .select(self.active_tab)
            .style(Style::default().fg(Color::Gray))
            .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        frame.render_widget(tabs, chunks[0]);

        // Content
        if self.detail_view {
            let detail = Paragraph::new(self.detail_content.clone())
                .style(Style::default().fg(Color::White))
                .block(Block::default().borders(Borders::ALL).title(" Detail "))
                .wrap(Wrap { trim: true });
            frame.render_widget(detail, chunks[1]);
        } else {
            match self.tab() {
                PermissionTab::Tools => self.render_tools(frame, chunks[1]),
                PermissionTab::Policies => self.render_policies(frame, chunks[1]),
                PermissionTab::Requests => self.render_requests(frame, chunks[1]),
                PermissionTab::Audit => self.render_audit(frame, chunks[1]),
            }
        }

        // Help footer
        let help_text = if matches!(self.mode, PermissionMode::ConfirmAllow | PermissionMode::ConfirmDeny) {
            "y: confirm · n/esc: cancel"
        } else if self.detail_view {
            "esc: back · c: copy"
        } else {
            match self.tab() {
                PermissionTab::Tools => "↑↓: navigate · a: allow · d: deny · k: ask · c: copy · tab: next · esc: close",
                PermissionTab::Policies => "↑↓: navigate · e: toggle · c: copy · tab: next · esc: close",
                PermissionTab::Requests => "↑↓: navigate · a: approve · d: deny · c: copy · tab: next · esc: close",
                PermissionTab::Audit => "tab: next · esc: close",
            }
        };
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, chunks[2]);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/permissions", "Open permissions overlay"),
            ("Tab", "Switch tabs (Tools/Policies/Requests/Audit)"),
            ("↑/↓", "Navigate"),
            ("Enter", "View detail"),
            ("A", "Allow / Approve"),
            ("D", "Deny"),
            ("K", "Set to Ask"),
            ("E", "Enable/disable policy"),
            ("Ctrl+C", "Copy entry"),
            ("R", "Refresh"),
            ("Esc", "Close overlay"),
        ]
    }
}
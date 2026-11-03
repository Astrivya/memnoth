//! Tasks pane: background jobs and async operations tracking.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::tui::app::App;

/// Background task status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// A background task
#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: TaskStatus,
    pub progress: f64, // 0.0 to 1.0
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub result: Option<String>,
}

/// Tasks pane showing background jobs
#[derive(Debug, Default)]
pub struct TasksPane {
    pub visible: bool,
    pub tabs: Vec<TasksTab>,
    pub active_tab: usize,
    pub list_state: ListState,
    pub tasks: Vec<BackgroundTask>,
    pub selected: usize,
    pub detail_view: bool,
    pub detail_content: String,
    pub auto_refresh: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TasksTab {
    All,
    Running,
    Completed,
    Failed,
}

impl TasksPane {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            visible: true,
            tabs: vec![
                TasksTab::All,
                TasksTab::Running,
                TasksTab::Completed,
                TasksTab::Failed,
            ],
            active_tab: 0,
            list_state: state,
            tasks: Vec::new(),
            selected: 0,
            detail_view: false,
            detail_content: String::new(),
            auto_refresh: true,
        }
    }

    /// Add a new background task
    pub fn add_task(&mut self, task: BackgroundTask) {
        self.tasks.insert(0, task); // Newest first
        if self.selected >= self.tasks.len() {
            self.selected = self.tasks.len().saturating_sub(1);
            self.list_state.select(if self.tasks.is_empty() { None } else { Some(self.selected) });
        }
    }

    /// Update task status
    pub fn update_task(&mut self, id: &str, status: TaskStatus, progress: Option<f64>, result: Option<String>, error: Option<String>) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = status;
            if let Some(p) = progress {
                task.progress = p;
            }
            task.result = result;
            task.error = error;
            if matches!(status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled) {
                task.completed_at = Some(chrono::Local::now().format("%H:%M:%S").to_string());
            }
        }
    }

    /// Handle key events for the tasks pane
    pub fn handle_key(&mut self, key: KeyEvent, _app: &mut App) -> bool {
        if !self.visible {
            return false;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Tab => {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
                self.selected = 0;
                self.update_list_state();
                true
            }
            KeyCode::BackTab => {
                self.active_tab = if self.active_tab == 0 {
                    self.tabs.len() - 1
                } else {
                    self.active_tab - 1
                };
                self.selected = 0;
                self.update_list_state();
                true
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.list_state.select(Some(self.selected));
                }
                true
            }
            KeyCode::Down => {
                let filtered = self.filtered_tasks();
                if self.selected < filtered.len().saturating_sub(1) {
                    self.selected += 1;
                    self.list_state.select(Some(self.selected));
                }
                true
            }
            KeyCode::Char('r') if ctrl => {
                // Retry failed task
                if let Some(task) = self.filtered_tasks().get(self.selected) {
                    if task.status == TaskStatus::Failed {
                        // In real implementation, would restart the task
                    }
                }
                true
            }
            KeyCode::Char('x') if ctrl => {
                // Cancel running task
                if let Some(task) = self.filtered_tasks().get(self.selected) {
                    if task.status == TaskStatus::Running || task.status == TaskStatus::Pending {
                        // In real implementation, would cancel the task
                    }
                }
                true
            }
            KeyCode::Char('c') if ctrl => {
                // Clear completed tasks
                self.tasks.retain(|t| t.status == TaskStatus::Running || t.status == TaskStatus::Pending);
                self.selected = 0;
                self.update_list_state();
                true
            }
            KeyCode::Char('a') => {
                self.auto_refresh = !self.auto_refresh;
                true
            }
            KeyCode::Enter => {
                self.detail_view = !self.detail_view;
                if self.detail_view {
                    self.update_detail_content();
                }
                true
            }
            KeyCode::Esc => {
                if self.detail_view {
                    self.detail_view = false;
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn filtered_tasks(&self) -> Vec<&BackgroundTask> {
        self.tasks.iter().filter(|t| {
            match self.active_tab {
                TasksTab::All => true,
                TasksTab::Running => matches!(t.status, TaskStatus::Running | TaskStatus::Pending),
                TasksTab::Completed => matches!(t.status, TaskStatus::Completed),
                TasksTab::Failed => matches!(t.status, TaskStatus::Failed | TaskStatus::Cancelled),
            }
        }).collect()
    }

    fn update_list_state(&mut self) {
        let filtered = self.filtered_tasks();
        if filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(self.selected.min(filtered.len() - 1)));
        }
    }

    fn update_detail_content(&mut self) {
        if let Some(task) = self.filtered_tasks().get(self.selected) {
            self.detail_content = format!(
                "ID: {}\nName: {}\nStatus: {:?}\nProgress: {:.0}%\nStarted: {}\nCompleted: {}\n\nDescription:\n{}\n\nResult:\n{}\n\nError:\n{}",
                task.id,
                task.name,
                task.status,
                task.progress * 100.0,
                task.started_at,
                task.completed_at.as_deref().unwrap_or("—"),
                task.description,
                task.result.as_deref().unwrap_or("—"),
                task.error.as_deref().unwrap_or("—"),
            );
        } else {
            self.detail_content = String::new();
        }
    }

    /// Render the tasks pane
    pub fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Tabs
                Constraint::Min(0),     // Task list
                Constraint::Length(3),  // Footer
            ])
            .split(area);

        // Render tabs
        self.render_tabs(frame, chunks[0]);

        // Render task list
        self.render_task_list(frame, chunks[1]);

        // Render footer
        self.render_footer(frame, chunks[2]);
    }

    fn render_tabs(&self, frame: &mut Frame, area: Rect) {
        let titles = vec!["All", "Running", "Completed", "Failed"];
        let counts = [
            self.tasks.len(),
            self.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Running | TaskStatus::Pending)).count(),
            self.tasks.iter().filter(|t| t.status == TaskStatus::Completed).count(),
            self.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::Cancelled)).count(),
        ];

        let tabs = titles.iter().enumerate().map(|(i, t)| {
            let style = if i == self.active_tab {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Span::styled(format!(" {} ({}) ", t, counts[i]), style)
        }).collect::<Vec<_>>();

        let tabs_widget = Paragraph::new(Line::from(tabs))
            .block(Block::default().borders(Borders::BOTTOM))
            .style(Style::default().fg(Color::White));
        frame.render_widget(tabs_widget, area);
    }

    fn render_task_list(&mut self, frame: &mut Frame, area: Rect) {
        let filtered = self.filtered_tasks();
        
        if filtered.is_empty() {
            let empty = Paragraph::new("No tasks")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title(" Tasks "))
                .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(empty, area);
            return;
        }

        let items: Vec<ListItem> = filtered.iter().enumerate().map(|(i, task)| {
            let selected = i == self.selected;
            let style = if selected {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let (status_char, status_color) = match task.status {
                TaskStatus::Pending => ("⏳", Color::Yellow),
                TaskStatus::Running => ("⟳", Color::Cyan),
                TaskStatus::Completed => ("✓", Color::Green),
                TaskStatus::Failed => ("✗", Color::Red),
                TaskStatus::Cancelled => ("○", Color::DarkGray),
            };

            let progress_bar = if task.status == TaskStatus::Running {
                format!(" [{:.0}%]", task.progress * 100.0)
            } else {
                String::new()
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", status_char), Style::default().fg(status_color)),
                    Span::styled(&task.name, style),
                    Span::styled(progress_bar, style.fg(Color::DarkGray)),
                    Span::styled(format!("  {}", task.started_at), style.fg(Color::DarkGray)),
                ]),
                Line::from(vec![
                    Span::styled("    ", Style::default()),
                    Span::styled(&task.description, style.fg(Color::Gray)),
                ]),
            ];
            ListItem::new(lines).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Background Tasks "))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::White));
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let auto_text = if self.auto_refresh { "ON" } else { "OFF" };
        let footer_text = format!(
            "Tab: switch · ↑↓: navigate · Enter: detail · Ctrl+R: retry · Ctrl+X: cancel · Ctrl+C: clear done · A: auto-refresh [{}] · Esc: close detail",
            auto_text
        );
        let footer = Paragraph::new(footer_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(footer, area);
    }
}
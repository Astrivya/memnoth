//! Keybinding help overlay: lists commands and their keys.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::util::{muted_style, normal_style, wrap};

/// A single keybinding entry
#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: String,
    pub action: String,
    pub description: String,
}

impl KeyBinding {
    pub fn new(
        key: impl Into<String>,
        action: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
            description: description.into(),
        }
    }
}

/// Group of keybindings
#[derive(Debug, Clone)]
pub struct KeyBindingGroup {
    pub name: String,
    pub bindings: Vec<KeyBinding>,
}

impl KeyBindingGroup {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            bindings: Vec::new(),
        }
    }

    pub fn with_binding(mut self, binding: KeyBinding) -> Self {
        self.bindings.push(binding);
        self
    }

    pub fn add_binding(&mut self, binding: KeyBinding) {
        self.bindings.push(binding);
    }
}

/// Keybinding help overlay widget
#[derive(Debug, Default)]
pub struct KeyBindingHelp {
    pub groups: Vec<KeyBindingGroup>,
    pub title: String,
    pub visible: bool,
    pub key_column_width: usize,
}

impl KeyBindingHelp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn add_group(&mut self, group: KeyBindingGroup) {
        self.groups.push(group);
    }

    pub fn show(&mut self) {
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Render the help overlay
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        if area.width < 40 || area.height < 8 {
            return;
        }

        let width = ((area.width as u32 * 4 / 5) as u16).clamp(40, area.width);
        let height = ((area.height as u32 * 4 / 5) as u16).clamp(8, area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );

        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(muted_style())
            .title(Line::from(Span::styled(
                format!(" {} ", self.title),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));

        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut lines: Vec<Line> = Vec::new();
        let width = inner.width as usize;

        for group in &self.groups {
            // Group header
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" {}", group.name),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!(" {}", "─".repeat(width.saturating_sub(1))),
                muted_style(),
            )));

            // Bindings
            for binding in &group.bindings {
                let key_col = format!(
                    " {:<key_column_width$} ",
                    binding.key,
                    key_column_width = self.key_column_width
                );
                let desc_col = format!("{}", binding.description);
                lines.push(Line::from(vec![
                    Span::styled(
                        key_col,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(desc_col, normal_style()),
                ]));
            }
        }

        // Footer hint
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " esc or ? close · ↑↓ scroll",
            muted_style(),
        )));

        // Center vertically
        let content_height = lines.len();
        let inner_height = inner.height as usize;
        if content_height < inner_height {
            let top_pad = (inner_height - content_height) / 2;
            for _ in 0..top_pad {
                lines.insert(0, Line::from(""));
            }
        }

        frame.render_widget(Paragraph::new(lines).wrap(wrap()), inner);
    }

    pub fn render_modal(&self, frame: &mut Frame, area: Rect) {
        self.render(frame, area);
    }

    /// Handle key press, returns true if the key was consumed
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        if !self.visible {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.hide();
                true
            }
            _ => true, // consume all keys while visible
        }
    }
}

/// Build default memnoth keybinding help
pub fn default_memnoth_help() -> KeyBindingHelp {
    let mut help = KeyBindingHelp::new().with_title("keybindings");
    help.key_column_width = 16;

    let mut nav = KeyBindingGroup::new("navigation");
    nav.add_binding(KeyBinding::new("↑↓", "history", "previous/next input"));
    nav.add_binding(KeyBinding::new("←→", "edit", "move cursor left/right"));
    nav.add_binding(KeyBinding::new(
        "PgUp/PgDn",
        "scroll",
        "scroll transcript up/down",
    ));
    nav.add_binding(KeyBinding::new("End", "newest", "jump to newest entry"));
    help.add_group(nav);

    let mut session = KeyBindingGroup::new("session");
    session.add_binding(KeyBinding::new("Enter", "send", "submit the current line"));
    session.add_binding(KeyBinding::new("Ctrl-C", "quit", "leave the session"));
    session.add_binding(KeyBinding::new("/help", "help", "show this list"));
    session.add_binding(KeyBinding::new("/clear", "clear", "empty the transcript"));
    help.add_group(session);

    let mut vault = KeyBindingGroup::new("vault");
    vault.add_binding(KeyBinding::new(
        "/vault init",
        "init",
        "create the temporal vault",
    ));
    vault.add_binding(KeyBinding::new(
        "/vault status",
        "status",
        "what the vault holds",
    ));
    vault.add_binding(KeyBinding::new(
        "/vault remember",
        "remember",
        "record a fact",
    ));
    vault.add_binding(KeyBinding::new(
        "/vault recall",
        "recall",
        "search the vault",
    ));
    help.add_group(vault);

    let mut kg = KeyBindingGroup::new("code graph");
    kg.add_binding(KeyBinding::new(
        "/kg index",
        "index",
        "build or update the code graph",
    ));
    kg.add_binding(KeyBinding::new(
        "/kg status",
        "status",
        "what was built, when, from where",
    ));
    kg.add_binding(KeyBinding::new(
        "/kg impact",
        "impact",
        "definitions, references, tests",
    ));
    kg.add_binding(KeyBinding::new(
        "/kg search",
        "search",
        "find symbols by name",
    ));
    help.add_group(kg);

    let mut confirm = KeyBindingGroup::new("confirmation");
    confirm.add_binding(KeyBinding::new("y", "approve", "approve a pending change"));
    confirm.add_binding(KeyBinding::new(
        "n / Esc",
        "reject",
        "reject a pending change",
    ));
    help.add_group(confirm);

    help
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_help(help: &KeyBindingHelp, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| help.render(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn help_not_rendered_when_hidden() {
        let help = KeyBindingHelp::new();
        let text = render_help(&help, 80, 30);
        assert_eq!(text.trim(), "");
    }

    #[test]
    fn help_renders_groups() {
        let mut help = KeyBindingHelp::new().with_title("test");
        help.add_group(KeyBindingGroup::new("nav").with_binding(KeyBinding::new(
            "↑↓",
            "history",
            "previous/next",
        )));
        help.show();
        let text = render_help(&help, 80, 30);
        assert!(text.contains("test"));
        assert!(text.contains("nav"));
        assert!(text.contains("↑↓"));
    }

    #[test]
    fn default_help_has_groups() {
        let help = default_memnoth_help();
        assert!(!help.groups.is_empty());
        assert!(help.groups.iter().any(|g| g.name == "navigation"));
        assert!(help.groups.iter().any(|g| g.name == "session"));
    }

    #[test]
    fn help_hides_on_esc() {
        let mut help = default_memnoth_help();
        help.show();
        assert!(help.is_visible());
        let consumed = help.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(consumed);
        assert!(!help.is_visible());
    }
}

//! Surface shell: eight surfaces, one focus owner, back stack.
//!
//! Ctrl+1..8 switches surfaces. `Enter` acts on focus, `Esc` pops one layer,
//! `Tab` completes/cycles, `Space` peeks. The status line names the focus
//! owner: chat, review, agents, memory, context, tasks, sessions, approval.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// The eight surfaces of the session shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Surface {
    #[default]
    Chat,
    Review,
    Agents,
    Memory,
    Context,
    Tasks,
    Sessions,
    Brief,
}

impl Surface {
    /// Move to the surface bound to ctrl key `index` (ignored when unknown).
    pub fn switch(&mut self, index: u8) {
        if let Some(surface) = Self::from_ctrl_key(index) {
            *self = surface;
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Surface::Chat => "Chat",
            Surface::Review => "Review",
            Surface::Agents => "Agents",
            Surface::Memory => "Memory",
            Surface::Context => "Context",
            Surface::Tasks => "Tasks",
            Surface::Sessions => "Sessions",
            Surface::Brief => "Brief",
        }
    }

    pub fn from_ctrl_key(index: u8) -> Option<Self> {
        match index {
            1 => Some(Surface::Chat),
            2 => Some(Surface::Review),
            3 => Some(Surface::Agents),
            4 => Some(Surface::Memory),
            5 => Some(Surface::Context),
            6 => Some(Surface::Tasks),
            7 => Some(Surface::Sessions),
            8 => Some(Surface::Brief),
            _ => None,
        }
    }
}

/// Which surface owns the composer right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusOwner {
    #[default]
    Chat,
    Review,
    Agents,
    Memory,
    Context,
    Tasks,
    Sessions,
    Approval,
}

impl FocusOwner {
    pub fn label(self) -> &'static str {
        match self {
            FocusOwner::Chat => "chat",
            FocusOwner::Review => "review",
            FocusOwner::Agents => "agent",
            FocusOwner::Memory => "memory",
            FocusOwner::Context => "context",
            FocusOwner::Tasks => "tasks",
            FocusOwner::Sessions => "sessions",
            FocusOwner::Approval => "approval",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            FocusOwner::Chat => "●",
            FocusOwner::Review => "◐",
            FocusOwner::Agents => "⬡",
            FocusOwner::Memory => "◆",
            FocusOwner::Context => "▣",
            FocusOwner::Tasks => "▲",
            FocusOwner::Sessions => "◐",
            FocusOwner::Approval => "!",
        }
    }
}

/// One entry in the back stack: where we were before an overlay opened.
#[derive(Debug, Clone)]
pub struct BackEntry {
    pub surface: Surface,
    pub focus: FocusOwner,
}

/// The surface shell: current surface, focus owner, back stack.
#[derive(Debug, Default)]
pub struct SurfaceShell {
    pub surface: Surface,
    pub focus: FocusOwner,
    pub back: Vec<BackEntry>,
    /// Whether a surface-specific overlay is active (e.g. Agent Deck open on Agents surface).
    pub overlay_active: bool,
}

impl SurfaceShell {
    pub fn new() -> Self {
        Self {
            surface: Surface::Chat,
            focus: FocusOwner::Chat,
            back: Vec::new(),
            overlay_active: false,
        }
    }

    /// Switch to a surface by Ctrl+N key. Pushes the current surface onto the back stack.
    pub fn switch(&mut self, index: u8) {
        if let Some(next) = Surface::from_ctrl_key(index)
            && next != self.surface
        {
            self.back.push(BackEntry {
                surface: self.surface,
                focus: self.focus,
            });
            self.surface = next;
            self.focus = match next {
                Surface::Chat => FocusOwner::Chat,
                Surface::Review => FocusOwner::Review,
                Surface::Agents => FocusOwner::Agents,
                Surface::Memory => FocusOwner::Memory,
                Surface::Context => FocusOwner::Context,
                Surface::Tasks => FocusOwner::Tasks,
                Surface::Sessions => FocusOwner::Sessions,
                Surface::Brief => FocusOwner::Chat,
            };
        }
    }

    /// Pop the back stack one layer (Esc from an overlay).
    pub fn pop(&mut self) -> Option<BackEntry> {
        self.back.pop()
    }

    /// Push an overlay: remember where we were.
    pub fn push_overlay(&mut self) {
        self.overlay_active = true;
    }

    /// Pop an overlay: restore focus to the surface's default.
    pub fn pop_overlay(&mut self) {
        self.overlay_active = false;
    }

    /// Render the surface title bar at the top of the screen.
    pub fn render_title(&self, frame: &mut Frame, area: Rect) {
        let title = Line::from(vec![
            Span::styled(
                format!(" {} ", self.surface.title()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" · "),
            Span::styled(
                format!("focus: {}", self.focus.label()),
                Style::default().fg(Color::Gray),
            ),
        ]);
        let block = Block::default().borders(Borders::ALL);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(title), area);
    }
}

//! Overlay system: composable modal UIs stacked above the session.
//!
//! Each overlay is a self-contained widget with its own state, key handling,
//! and rendering. Overlays are managed as a stack — the top overlay receives
//! input first, and `Esc` closes the current overlay.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use std::fmt::Debug;

use crate::tui::app::App;

/// Action an overlay can request from the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    /// Close this overlay (pop from stack)
    Close,
    /// Close all overlays
    CloseAll,
    /// Push a new overlay onto the stack
    Push(OverlayKind),
    /// Replace the current overlay with a new one
    Replace(OverlayKind),
    /// No action
    None,
}

/// Known overlay kinds for dynamic dispatch via the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    Search,
    Context,
    Diff,
    Rewind,
    Model,
    Models,
    Permissions,
    Help,
    Feedback,
    Palette,
    Connect,
}

/// Trait for all overlays.
pub trait Overlay: Debug + Send {
    /// Unique kind identifier.
    fn kind(&self) -> OverlayKind;

    /// Whether this overlay is currently visible.
    fn is_visible(&self) -> bool;

    /// Show the overlay (called when pushed).
    fn show(&mut self);

    /// Hide the overlay (called when popped).
    fn hide(&mut self);

    /// Handle a key event. Returns an action for the host.
    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction;

    /// Render the overlay onto the given area.
    fn render(&mut self, frame: &mut Frame, area: Rect, app: &App);

    /// Get help text for this overlay's keybindings.
    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![]
    }
}

/// Overlay stack manager.
#[derive(Debug, Default)]
pub struct OverlayStack {
    overlays: Vec<Box<dyn Overlay>>,
}

impl OverlayStack {
    /// Create a new empty overlay stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an overlay onto the stack.
    pub fn push(&mut self, mut overlay: Box<dyn Overlay>) {
        overlay.show();
        self.overlays.push(overlay);
    }

    /// Pop the top overlay.
    pub fn pop(&mut self) -> Option<Box<dyn Overlay>> {
        let mut overlay = self.overlays.pop()?;
        overlay.hide();
        Some(overlay)
    }

    /// Get the top overlay mutably.
    pub fn top_mut(&mut self) -> Option<&mut Box<dyn Overlay>> {
        self.overlays.last_mut()
    }

    /// Get the top overlay immutably.
    pub fn top(&self) -> Option<&Box<dyn Overlay>> {
        self.overlays.last()
    }

    /// Check if any overlay is visible.
    pub fn has_overlay(&self) -> bool {
        !self.overlays.is_empty()
    }

    /// Get the number of overlays in the stack.
    pub fn len(&self) -> usize {
        self.overlays.len()
    }

    /// Handle a key event for the top overlay.
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if let Some(overlay) = self.top_mut() {
            overlay.handle_key(key, app)
        } else {
            OverlayAction::None
        }
    }

    /// Render all overlays (bottom to top).
    pub fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        for overlay in &mut self.overlays {
            if overlay.is_visible() {
                overlay.render(frame, area, app);
            }
        }
    }

    /// Get combined help text for all visible overlays.
    pub fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        let mut help = Vec::new();
        for overlay in &self.overlays {
            if overlay.is_visible() {
                help.extend(overlay.help_text());
            }
        }
        help
    }
}

/// Factory function to create an overlay by kind.
pub fn create_overlay(kind: OverlayKind) -> Box<dyn Overlay> {
    match kind {
        OverlayKind::Search => Box::new(SearchOverlay::new()),
        OverlayKind::Context => Box::new(ContextOverlay::new()),
        OverlayKind::Diff => Box::new(DiffOverlay::new()),
        OverlayKind::Rewind => Box::new(RewindOverlay::new()),
        OverlayKind::Model => Box::new(ModelOverlay::new()),
        OverlayKind::Models => Box::new(ModelsOverlay::new()),
        OverlayKind::Help => Box::new(HelpOverlay::new()),
        OverlayKind::Feedback => Box::new(crate::feedback::FeedbackOverlay::new()),
        OverlayKind::Palette => Box::new(PaletteOverlay::new()),
        OverlayKind::Connect => Box::new(ConnectOverlay::new()),
        OverlayKind::Permissions => Box::new(PermissionsOverlay::new()),
    }
}

/// Slash command parser for overlay triggers.
pub fn parse_slash_command(input: &str) -> Option<OverlayKind> {
    let input = input.trim_start_matches('/').trim();
    match input {
        "search" => Some(OverlayKind::Search),
        "context" => Some(OverlayKind::Context),
        "diff" => Some(OverlayKind::Diff),
        "rewind" => Some(OverlayKind::Rewind),
        "model" => Some(OverlayKind::Model),
        "models" => Some(OverlayKind::Models),
        "permissions" => Some(OverlayKind::Permissions),
        "help" => Some(OverlayKind::Help),
        "feedback" => Some(OverlayKind::Feedback),
        "palette" => Some(OverlayKind::Palette),
        "connect" => Some(OverlayKind::Connect),
        _ => None,
    }
}

// Import all overlay implementations
mod context;
mod connect;
mod diff;
mod help;
mod model;
mod models;
mod palette;
mod permissions;
mod rewind;
mod search;

pub use context::ContextOverlay;
pub use connect::ConnectOverlay;
pub use diff::DiffOverlay;
pub use help::HelpOverlay;
pub use model::ModelOverlay;
pub use models::ModelsOverlay;
pub use palette::PaletteOverlay;
pub use permissions::PermissionsOverlay;
pub use rewind::RewindOverlay;
pub use search::SearchOverlay;

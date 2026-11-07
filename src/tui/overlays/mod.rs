//! Overlay system: composable modal UIs stacked above the session.
//!
//! Each overlay is a self-contained widget with its own state, key handling,
//! and rendering. Overlays are managed as a stack — the top overlay receives
//! input first, and `Esc` closes the current overlay.

use crossterm::event::KeyEvent;
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
    Models,
    Permissions,
    Help,
    Feedback,
    Palette,
    /// `/commands`: the whole registry, grouped, with no row limit.
    Commands,
    Connect,
    /// `/teams members` with no id: pick the team from the org's list.
    Teams,
    /// `/keys create`: the form, because the secret must not reach the transcript.
    KeyCreate,
    /// `/org invite`: the same, for the link and the code.
    Invite,
    /// The window a list-shaped verb answer opens: rows to walk, and the
    /// buttons that act on them, instead of a table flattened into prose.
    Popover,
}

/// Trait for all overlays.
pub trait Overlay: Debug + Send {
    /// Unique kind identifier.
    fn kind(&self) -> OverlayKind;

    /// Whether this overlay is currently visible.
    fn is_visible(&self) -> bool;

    /// Show the overlay (called when pushed).
    fn show(&mut self);

    /// Read what opening the overlay needs, against the app it was opened over.
    ///
    /// `show` has no `App` — it is what the stack can do on its own — but a
    /// browser that opens empty is a bug, and loading needs the config. The host
    /// calls this once, immediately after `show`.
    fn on_open(&mut self, _app: &mut App) {}

    /// A provider test finished on its own thread. Reported to the overlay
    /// that started it, so the row it belongs to can leave "Testing…".
    fn on_provider_test(&mut self, _result: &crate::tui::app::ProviderTestResult) {}

    /// A provider answered its own model list on another thread. Delivered to
    /// every open overlay, like a provider test: the browser that asked is not
    /// always the one on top when the reply arrives.
    fn on_models_loaded(&mut self, _result: &crate::tui::app::ModelsLoaded) {}

    /// One frame has passed with this overlay open.
    ///
    /// The place for work that arrives on its own thread — a team list, a
    /// minted key — because `render` must not be where state changes and
    /// `handle_key` never runs when nobody is typing.
    fn on_tick(&mut self, _app: &mut App) {}

    /// Hide the overlay (called when popped).
    fn hide(&mut self);

    /// Handle a key event. Returns an action for the host.
    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction;

    /// Text that arrived in one piece (bracketed paste).
    ///
    /// Returns true when the overlay took it. An API key is always pasted, so
    /// the field that asks for one has to be able to receive it; the host never
    /// sends a paste behind an open overlay.
    fn handle_paste(&mut self, _text: &str) -> bool {
        false
    }

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

    /// Push an overlay the host has already shown and opened.
    ///
    /// `push` cannot do this itself: `on_open` needs the `App`, and taking
    /// `&mut App` beside `&mut self` is two mutable borrows of the same value.
    pub fn adopt(&mut self, overlay: Box<dyn Overlay>) {
        self.overlays.push(overlay);
    }

    /// Offer a pasted block to the overlay that owns the keyboard.
    ///
    /// Only the top overlay, because that is the one the keys would have gone
    /// to.
    pub fn handle_paste(&mut self, text: &str) -> bool {
        self.overlays
            .last_mut()
            .is_some_and(|overlay| overlay.handle_paste(text))
    }

    /// Offer a finished provider test to every open overlay.
    pub fn deliver_provider_test(&mut self, result: &crate::tui::app::ProviderTestResult) {
        for overlay in &mut self.overlays {
            overlay.on_provider_test(result);
        }
    }

    /// Offer a fetched model list to every open overlay.
    pub fn deliver_models(&mut self, result: &crate::tui::app::ModelsLoaded) {
        for overlay in &mut self.overlays {
            overlay.on_models_loaded(result);
        }
    }

    /// Let every open overlay collect what arrived on its own thread.
    ///
    /// Every overlay, not only the top one: a reply belongs to the request that
    /// started it, and the screen that asked is not always the one on top when
    /// it lands.
    pub fn tick(&mut self, app: &mut App) {
        for overlay in &mut self.overlays {
            overlay.on_tick(app);
        }
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

    /// Check if any overlay is visible.
    pub fn has_overlay(&self) -> bool {
        !self.overlays.is_empty()
    }

    /// Handle a key event for the top overlay.
    ///
    /// `None` means there was no overlay at all. An overlay that is open
    /// consumes every key, even the ones it ignores: `OverlayAction::None` is a
    /// handled key with no consequence, not a key the session may also act on.
    /// Without that distinction, typing in a modal also typed into the prompt
    /// behind it, and Tab in the help overlay completed a file path.
    pub fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> Option<OverlayAction> {
        self.top_mut().map(|overlay| overlay.handle_key(key, app))
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
        OverlayKind::Models => Box::new(ModelsOverlay::new()),
        OverlayKind::Help => Box::new(HelpOverlay::new()),
        OverlayKind::Feedback => Box::new(crate::feedback::FeedbackOverlay::new()),
        OverlayKind::Palette => Box::new(PaletteOverlay::new()),
        OverlayKind::Commands => Box::new(CommandsOverlay::new()),
        OverlayKind::Connect => Box::new(ConnectOverlay::new()),
        OverlayKind::Permissions => Box::new(PermissionsOverlay::new()),
        OverlayKind::Teams => Box::new(TeamsOverlay::new()),
        OverlayKind::KeyCreate => Box::new(KeyCreateOverlay::new()),
        OverlayKind::Invite => Box::new(InviteOverlay::new()),
        // Opened with a spec, never bare: `present` is the door for this one.
        OverlayKind::Popover => Box::new(PopoverOverlay::new(Default::default())),
    }
}

/// Show a boxed overlay the caller has already built.
///
/// `create_overlay` cannot carry arguments, and two of these screens arrive with
/// values already typed (`/keys create --name laptop`). One place still decides
/// what opening means: `show`, then `on_open` with the app, then adopt.
pub fn present(app: &mut App, mut overlay: Box<dyn Overlay>) {
    overlay.show();
    overlay.on_open(app);
    app.overlay_stack.adopt(overlay);
}

/// Slash command parser for overlay triggers.
pub fn parse_slash_command(input: &str) -> Option<OverlayKind> {
    let input = input.trim_start_matches('/').trim();
    match input {
        "search" => Some(OverlayKind::Search),
        "context" => Some(OverlayKind::Context),
        "diff" => Some(OverlayKind::Diff),
        "rewind" => Some(OverlayKind::Rewind),
        // `/model` was a second browser over the same data; it is now an alias,
        // so muscle memory keeps working and there is only one list to fix.
        "model" | "models" => Some(OverlayKind::Models),
        "permissions" => Some(OverlayKind::Permissions),
        "help" => Some(OverlayKind::Help),
        "feedback" => Some(OverlayKind::Feedback),
        "palette" => Some(OverlayKind::Palette),
        "commands" => Some(OverlayKind::Commands),
        "connect" => Some(OverlayKind::Connect),
        _ => None,
    }
}

// Import all overlay implementations
mod cloud_form;
mod commands;
mod connect;
mod context;
mod diff;
mod help;
mod models;
// Crate-visible because the inline slash dropdown reads the same command list
// the modal palette does.
pub(crate) mod palette;
mod permissions;
pub(crate) mod popover;
/// Bespoke windows for the verb families whose rows are worth more than their
/// fields: one file per family, dispatched from `spec_for`.
pub(crate) mod popover_specs;
mod rewind;
mod search;
mod teams;

pub use cloud_form::{InviteOverlay, KeyCreateOverlay};
pub use commands::CommandsOverlay;
pub use connect::ConnectOverlay;
pub use context::ContextOverlay;
pub use diff::DiffOverlay;
pub use help::HelpOverlay;
pub use models::ModelsOverlay;
pub use palette::PaletteOverlay;
pub use permissions::PermissionsOverlay;
pub use popover::PopoverOverlay;
pub use rewind::RewindOverlay;
pub use search::SearchOverlay;
pub use teams::TeamsOverlay;

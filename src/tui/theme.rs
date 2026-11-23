//! Theme system for accessibility: high contrast, no color, etc.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

use crate::tui::app::Theme;

/// The theme the environment asks for, if it asks for one.
///
/// `NO_COLOR` (set to anything non-empty) is the cross-tool convention, and
/// `TERM=dumb` says the terminal cannot do it. Both were advertised in the help
/// screen while nothing read them, which is the worst of both worlds: a person
/// who needs plain text got escape codes and a key that did nothing.
///
pub fn theme_for(no_color: Option<&str>, term: Option<&str>) -> Option<Theme> {
    if no_color.is_some_and(|value| !value.is_empty()) {
        return Some(Theme::NoColor);
    }
    if term.is_some_and(|value| value.eq_ignore_ascii_case("dumb")) {
        return Some(Theme::NoColor);
    }
    None
}

/// [`theme_for`] against this process's environment.
pub fn theme_from_env() -> Option<Theme> {
    theme_for(
        std::env::var_os("NO_COLOR")
            .map(|value| value.to_string_lossy().into_owned())
            .as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// Remove every colour from a finished frame.
///
/// Stripping here rather than at each call site is what makes `no-color` true
/// everywhere: 63 places in `ui.rs` pick a colour, and a theme that is applied
/// by remembering all of them is a theme that will be wrong somewhere.
/// Modifiers (bold, reversal) are kept — they are the part a plain terminal and
/// a screen reader can still use to tell one row from another.
pub fn strip_colors(buffer: &mut Buffer) {
    let area = *buffer.area();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            cell.set_fg(Color::Reset);
            cell.set_bg(Color::Reset);
        }
    }
}

/// Color scheme for a theme
#[derive(Debug, Clone)]
pub struct ColorScheme {
    pub background: Color,
    pub foreground: Color,
    pub border: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub title: Color,
    pub user: Color,
    pub assistant: Color,
    pub tool: Color,
    pub info: Color,
    pub error: Color,
    pub system: Color,
    pub highlight: Color,
    pub warning: Color,
    pub success: Color,
    pub dim: Color,
    pub accent: Color,
    pub focus_border: Color,
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self::default_theme()
    }
}

impl ColorScheme {
    /// Default theme
    pub fn default_theme() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::White,
            border: Color::Gray,
            selected_bg: Color::Blue,
            selected_fg: Color::White,
            title: Color::Yellow,
            user: Color::Cyan,
            assistant: Color::Green,
            tool: Color::Yellow,
            info: Color::Blue,
            error: Color::Red,
            system: Color::Magenta,
            highlight: Color::Yellow,
            warning: Color::Yellow,
            success: Color::Green,
            dim: Color::DarkGray,
            accent: Color::Cyan,
            focus_border: Color::Yellow,
        }
    }

    /// High contrast theme
    pub fn high_contrast() -> Self {
        Self {
            background: Color::Black,
            foreground: Color::White,
            border: Color::White,
            selected_bg: Color::White,
            selected_fg: Color::Black,
            title: Color::White,
            user: Color::White,
            assistant: Color::White,
            tool: Color::White,
            info: Color::White,
            error: Color::White,
            system: Color::White,
            highlight: Color::White,
            warning: Color::White,
            success: Color::White,
            dim: Color::Gray,
            accent: Color::White,
            focus_border: Color::White,
        }
    }

    /// No color theme (for NO_COLOR env or screen readers)
    pub fn no_color() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::Reset,
            border: Color::Reset,
            selected_bg: Color::Reset,
            selected_fg: Color::Reset,
            title: Color::Reset,
            user: Color::Reset,
            assistant: Color::Reset,
            tool: Color::Reset,
            info: Color::Reset,
            error: Color::Reset,
            system: Color::Reset,
            highlight: Color::Reset,
            warning: Color::Reset,
            success: Color::Reset,
            dim: Color::Reset,
            accent: Color::Reset,
            focus_border: Color::Reset,
        }
    }

    /// Get color scheme for a theme
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Default => Self::default_theme(),
            Theme::HighContrast => Self::high_contrast(),
            Theme::NoColor => Self::no_color(),
        }
    }

    /// Get style for a role with modifiers
    pub fn role_style(
        &self,
        role: crate::tui::app::EntryRole,
        selected: bool,
    ) -> (Style, &'static str) {
        let (base_color, glyph) = match role {
            crate::tui::app::EntryRole::User => (self.user, "▶"),
            crate::tui::app::EntryRole::Answer => (self.assistant, "▶"),
            crate::tui::app::EntryRole::Info => (self.info, "ℹ"),
            crate::tui::app::EntryRole::Error => (self.error, "✗"),
            // A tool row's glyph is its state, so it is not themed away.
            crate::tui::app::EntryRole::Tool => (self.tool, "▌"),
            crate::tui::app::EntryRole::ToolDone => (self.success, "✓"),
            crate::tui::app::EntryRole::ToolFailed => (self.error, "✗"),
            crate::tui::app::EntryRole::ToolInterrupted => (self.dim, "·"),
            crate::tui::app::EntryRole::Thinking => (self.dim, "▸"),
        };

        let mut style = Style::default().fg(base_color).add_modifier(Modifier::BOLD);
        if role == crate::tui::app::EntryRole::Thinking {
            // The transcript's rule, mirrored here: reasoning is grey italic,
            // never the answer's weight.
            style = style
                .remove_modifier(Modifier::BOLD)
                .add_modifier(Modifier::ITALIC);
        }
        if selected {
            style = style.bg(self.selected_bg).fg(self.selected_fg);
        }
        (style, glyph)
    }

    /// Check if we should use colors
    pub fn use_colors(&self) -> bool {
        self.foreground != Color::Reset
    }
}

// ── Semantic token palette ────────────────────────────────────────────────
//
// Implementation of docs/research/cli-theme-and-design-2026-11-23.md. These
// are *semantic roles*, not decoration: one accent, meaning-only, with
// audited contrast in both modes. Render code must reference `tokens()`
// rather than picking raw colours, so a theme change cannot drift across
// surfaces. Values are the Radix Sand + Orange scale with the Rosé Pine Dawn
// cream canvas; all pairs clear WCAG AA (>= 4.5:1 text, >= 3:1 borders).

/// The palette family selected for this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// Radix Sand + Orange on Rosé Pine Dawn cream (light mode).
    Cream,
    /// Radix Sand + Orange on near-black (default dark mode).
    Dark,
    /// Maximal contrast; keeps meaning but removes subtlety.
    HighContrast,
    /// No colour at all; modifiers and glyphs still carry meaning.
    NoColor,
}

impl Palette {
    /// Resolve from the existing theme plus the optional palette override.
    /// `MEMNOTH_PALETTE` accepts `cream`/`light`, `dark`, `high-contrast`,
    /// `no-color`. `Theme::NoColor` always wins (accessibility gate).
    pub fn from_env(theme: Theme) -> Self {
        if theme == Theme::NoColor {
            return Self::NoColor;
        }
        if theme == Theme::HighContrast {
            return Self::HighContrast;
        }
        match std::env::var("MEMNOTH_PALETTE")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("cream") | Some("light") => Self::Cream,
            Some("dark") => Self::Dark,
            Some("high-contrast") | Some("contrast") => Self::HighContrast,
            Some("no-color") => Self::NoColor,
            _ => Self::Dark,
        }
    }
}

/// Audited semantic colours for one palette. Ratios are in the research doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    pub canvas: Color,
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
}

impl Tokens {
    pub fn for_palette(palette: Palette) -> Self {
        match palette {
            Palette::Cream => Self {
                canvas: Color::Rgb(250, 244, 237),  // #faf4ed
                surface: Color::Rgb(253, 253, 252), // #fdfdfc
                text: Color::Rgb(33, 32, 28),       // #21201c — 16.0:1
                muted: Color::Rgb(99, 99, 94),      // #63635e — ~6:1
                border: Color::Rgb(157, 156, 150),  // ~3:1 non-text
                accent: Color::Rgb(184, 67, 0),     // #b84300 — 5.0:1
                success: Color::Rgb(33, 131, 88),   // #218358 — 4.6:1
                warning: Color::Rgb(154, 103, 0),   // #9a6700 — 5.6:1
                error: Color::Rgb(206, 44, 49),     // #ce2c31 — 5.1:1
                selected_bg: Color::Rgb(184, 67, 0),
                selected_fg: Color::Rgb(255, 253, 252),
            },
            Palette::Dark => Self {
                canvas: Color::Rgb(17, 17, 16),     // #111110
                surface: Color::Rgb(28, 28, 26),    // #1c1c1a
                text: Color::Rgb(238, 238, 236),    // #eeeeec — 16.3:1
                muted: Color::Rgb(181, 179, 173),   // #b5b3ad — ~9:1
                border: Color::Rgb(90, 89, 84),     // ~3:1 non-text
                accent: Color::Rgb(255, 160, 87),   // #ffa057 — 9.4:1
                success: Color::Rgb(61, 214, 140),  // #3dd68c — 10.1:1
                warning: Color::Rgb(235, 193, 119), // #ebc177 — ~12:1
                error: Color::Rgb(255, 149, 146),   // #ff9592 — ~9:1
                selected_bg: Color::Rgb(255, 160, 87),
                selected_fg: Color::Rgb(17, 17, 16),
            },
            Palette::HighContrast => Self {
                canvas: Color::Black,
                surface: Color::Black,
                text: Color::White,
                muted: Color::Gray,
                border: Color::White,
                accent: Color::Yellow,
                success: Color::Green,
                warning: Color::Yellow,
                error: Color::Red,
                selected_bg: Color::White,
                selected_fg: Color::Black,
            },
            Palette::NoColor => Self {
                canvas: Color::Reset,
                surface: Color::Reset,
                text: Color::Reset,
                muted: Color::Reset,
                border: Color::Reset,
                accent: Color::Reset,
                success: Color::Reset,
                warning: Color::Reset,
                error: Color::Reset,
                selected_bg: Color::Reset,
                selected_fg: Color::Reset,
            },
        }
    }

    /// Whether these tokens carry any colour at all.
    pub fn colored(&self) -> bool {
        self.text != Color::Reset
    }
}

// An atomic rather than a OnceLock: `/set-theme` changes the palette at runtime,
// so `tokens()` must follow it. Constructing 11 Colors per draw is trivial.
static ACTIVE_PALETTE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

fn palette_index(p: Palette) -> u8 {
    match p {
        Palette::Cream => 0,
        Palette::Dark => 1,
        Palette::HighContrast => 2,
        Palette::NoColor => 3,
    }
}

/// Set the process-wide palette. Called at startup and on `/set-theme` so every
/// surface shares one audited set of tokens.
pub fn init_palette(palette: Palette) {
    ACTIVE_PALETTE.store(palette_index(palette), std::sync::atomic::Ordering::Release);
}

/// The current palette. Defaults from the environment until first set.
pub fn palette() -> Palette {
    match ACTIVE_PALETTE.load(std::sync::atomic::Ordering::Acquire) {
        0 => Palette::Cream,
        1 => Palette::Dark,
        2 => Palette::HighContrast,
        3 => Palette::NoColor,
        _ => Palette::from_env(Theme::Default),
    }
}

/// The current semantic tokens, following `/set-theme` changes.
pub fn tokens() -> Tokens {
    Tokens::for_palette(palette())
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn every_palette_has_distinct_roles_and_meaningful_contrast() {
        for p in [Palette::Cream, Palette::Dark, Palette::HighContrast] {
            let t = Tokens::for_palette(p);
            assert!(t.colored(), "{p:?} must carry colour");
            assert_ne!(t.text, t.muted, "{p:?}: muted must be a lighter step");
            // High-contrast deliberately uses white borders on black; the
            // tonal-step rule applies only to the audited Cream/Dark scales.
            if p != Palette::HighContrast {
                assert_ne!(t.text, t.border, "{p:?}: border must be a lighter step");
            }
            assert_ne!(t.accent, t.error, "{p:?}: accent is not error");
            assert_ne!(t.selected_bg, t.canvas, "{p:?}: selection must be visible");
        }
    }

    #[test]
    fn no_color_has_no_colour_but_keeps_roles() {
        let t = Tokens::for_palette(Palette::NoColor);
        assert!(!t.colored());
        assert_eq!(t.accent, Color::Reset);
        assert_eq!(t.error, Color::Reset);
    }

    #[test]
    fn no_color_env_overrides_palette_choice() {
        assert_eq!(Palette::from_env(Theme::NoColor), Palette::NoColor);
        assert_eq!(
            Palette::from_env(Theme::HighContrast),
            Palette::HighContrast
        );
    }
}

/// Screen reader output formatter
pub struct ScreenReaderOutput;

impl ScreenReaderOutput {
    /// Format an entry for plain text output (screen reader mode)
    pub fn format_entry(entry: &crate::tui::app::Entry) -> String {
        let role_str = match entry.role {
            crate::tui::app::EntryRole::User => "[USER]",
            crate::tui::app::EntryRole::Answer => "[ASSISTANT]",
            crate::tui::app::EntryRole::Info => "[INFO]",
            crate::tui::app::EntryRole::Error => "[ERROR]",
            crate::tui::app::EntryRole::Tool => "[TOOL]",
            crate::tui::app::EntryRole::ToolDone => "[TOOL]",
            crate::tui::app::EntryRole::ToolFailed => "[TOOL FAILED]",
            crate::tui::app::EntryRole::ToolInterrupted => "[TOOL INTERRUPTED]",
            crate::tui::app::EntryRole::Thinking => "[THINKING]",
        };
        format!("{} {}", role_str, entry.text)
    }

    /// Format a transcript for plain text output
    pub fn format_transcript(entries: &[crate::tui::app::Entry]) -> String {
        entries
            .iter()
            .map(Self::format_entry)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Format help text for plain text output
    pub fn format_help() -> String {
        r#"memnoth session — commands

/help                      this list
/quit                      leave (also :q, ctrl-c)
/clear                     clear the transcript
/home                      the brief: what is here, what is missing
/new [name]                start a fresh conversation in this window
/sessions                  every conversation, with a preview (alias: /resume)
/models                    browse, search and switch models (alias: /model)
/search                    open search overlay
/search <query>            ask the vault and the graph (no model)
/context                   open context overlay
/diff                      open diff overlay
/rewind                    open rewind overlay
/permissions               open permissions overlay
/doctor                    paths, vault, graph, model, MCP, profiles
/vault init                create the vault
/vault status              what the vault holds
/vault asof [--at WHEN] [--name N]   what was believed at an instant
/vault history <name>      every version, with the reason it changed
/vault contradictions [name]         beliefs explicitly contradicted
/vault remember <name> [--kind K] <fact…>   record a fact
/vault recall <query>      search the vault
/kg index [--full] [--no-embed]      build or update the code graph
/kg status                 what was built, when, and from where
/kg impact <symbol>        definitions, references, tests
/kg search <query>         find symbols by name
/mcp doctor                what clients are registered, and where
/teams join <code>         accept an invite and join an organization
/init                      generate AGENTS.md
/memory                    edit project memory
!<cmd>                     shell passthrough (sandbox policy applies)

Keys: enter send · ↑↓ history · ←→ edit · pgup/pgdn scroll · end newest · ctrl-c quit"#
            .to_string()
    }

    /// Format keybindings for plain text output
    pub fn format_keybindings() -> String {
        r#"Global keybindings:
  Enter              Send message
  ↑/↓                History navigation
  ←/→                Edit cursor movement
  PgUp/PgDn          Scroll transcript
  End                Jump to newest
  Ctrl+C             Quit / Cancel

Overlay keybindings:
  Tab / Shift+Tab    Switch tabs
  ↑/↓                Navigate entries
  Enter              View detail / Select
  Esc                Close overlay / Back from detail
  Ctrl+C             Copy entry
  P                  Pin entry
  R                  Refresh

Diff overlay:
  a / r              Accept / Reject current hunk
  A / R              Accept / Reject all hunks
  n / N              Next / Previous conflict
  s                  Toggle side-by-side view
  w                  Toggle whitespace
  l                  Toggle line numbers
  ?                  Show help

Rewind overlay:
  Enter              Resume session / View detail
  B                  Branch from turn
  D                  Delete session

Context overlay:
  Ctrl+X             Compact context

Tasks overlay:
  Ctrl+R             Retry failed task
  Ctrl+X             Cancel running task
  Ctrl+C             Clear completed tasks
  A                  Toggle auto-refresh"#
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn no_color_yields_the_plain_theme() {
        assert_eq!(theme_for(Some("1"), None), Some(Theme::NoColor));
        assert_eq!(theme_for(Some("anything"), None), Some(Theme::NoColor));
        assert_eq!(theme_for(None, Some("dumb")), Some(Theme::NoColor));
        assert_eq!(theme_for(None, Some("DUMB")), Some(Theme::NoColor));
    }

    #[test]
    fn an_empty_or_absent_setting_changes_nothing() {
        // The NO_COLOR spec: presence with a non-empty value means "no colour".
        assert_eq!(theme_for(Some(""), None), None);
        assert_eq!(theme_for(None, Some("xterm-256color")), None);
        assert_eq!(theme_for(None, None), None);
    }

    #[test]
    fn stripping_a_frame_leaves_glyphs_and_modifiers() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        buffer[(0, 0)]
            .set_symbol("x")
            .set_fg(Color::Red)
            .set_bg(Color::Blue);
        buffer[(0, 0)].modifier = Modifier::BOLD;
        buffer[(1, 0)].set_symbol("y").set_fg(Color::Green);

        strip_colors(&mut buffer);

        assert_eq!(buffer[(0, 0)].symbol(), "x");
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        assert_eq!(buffer[(0, 0)].modifier, Modifier::BOLD);
        assert_eq!(buffer[(1, 0)].fg, Color::Reset);
    }
}

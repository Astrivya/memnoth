//! Theme system for accessibility: high contrast, no color, etc.

use ratatui::style::{Color, Modifier, Style};

use crate::tui::app::Theme;

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
        };

        let mut style = Style::default().fg(base_color).add_modifier(Modifier::BOLD);
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
/sessions                  sessions on disk, and how to resume them
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

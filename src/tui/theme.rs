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
///
/// These are the stable variants other modules name directly (`Palette::Dark`,
/// `Palette::NoColor`, …). The colours themselves live in the [`PALETTES`]
/// registry, so adding a theme is one table row rather than another match arm
/// in every renderer. `Palette::all()` walks every selectable theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// Radix Sand + Orange on Rosé Pine Dawn cream (light mode).
    Cream,
    /// Radix Sand + Orange on near-black (default dark mode).
    Dark,
    /// Catppuccin Mocha, the warm dark flavour.
    CatppuccinMocha,
    /// Catppuccin Latte, the warm light flavour.
    CatppuccinLatte,
    /// Nord, a cool desaturated dark theme.
    Nord,
    /// Everforest, a low-contrast green dark theme.
    Everforest,
    /// Tokyo Night, a cool blue-violet dark theme.
    TokyoNight,
    /// Gruvbox, a warm retro dark theme.
    Gruvbox,
    /// Maximal contrast; keeps meaning but removes subtlety.
    HighContrast,
    /// No colour at all; modifiers and glyphs still carry meaning.
    NoColor,
}

impl Palette {
    /// Every selectable palette, in registry order.
    pub fn all() -> &'static [Palette] {
        &PALETTE_ORDER
    }

    /// Stable id for config, `MEMNOTH_PALETTE` and the picker.
    pub fn id(self) -> &'static str {
        PALETTES[palette_index(self) as usize].id
    }

    /// Human-readable name for picker rows; the stable key is still [`Self::id`].
    pub fn label(self) -> &'static str {
        PALETTE_LABELS[palette_index(self) as usize]
    }

    /// Inverse of [`Palette::id`], with a few compatibility aliases.
    pub fn parse_id(s: &str) -> Option<Palette> {
        let normalized = s.trim().to_ascii_lowercase();
        for (idx, def) in PALETTES.iter().enumerate() {
            if def.id == normalized.as_str() {
                return PALETTE_ORDER.get(idx).copied();
            }
        }
        match normalized.as_str() {
            "light" => Some(Self::Cream),
            "default" => Some(Self::Dark),
            "contrast" => Some(Self::HighContrast),
            _ => None,
        }
    }

    /// Resolve from the existing theme plus the optional palette override.
    /// `MEMNOTH_PALETTE` accepts any [`Palette::id`] (plus the `light`,
    /// `default` and `contrast` aliases). `Theme::NoColor` and
    /// `Theme::HighContrast` always win (accessibility gate).
    pub fn from_env(theme: Theme) -> Self {
        if theme == Theme::NoColor {
            return Self::NoColor;
        }
        if theme == Theme::HighContrast {
            return Self::HighContrast;
        }
        std::env::var("MEMNOTH_PALETTE")
            .ok()
            .as_deref()
            .and_then(Self::parse_id)
            .unwrap_or(Self::Dark)
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

/// One row in the theme registry: a stable id and its audited tokens.
#[derive(Debug, Clone, Copy)]
struct PaletteDef {
    id: &'static str,
    tokens: Tokens,
}

/// The registry. Its index is exactly the `u8` held in `ACTIVE_PALETTE`, so a
/// live `/set-theme` switch is visible on the next draw with no OnceLock.
static PALETTES: [PaletteDef; 10] = [
    PaletteDef {
        id: "cream",
        tokens: Tokens {
            canvas: Color::Rgb(250, 244, 237),      // #faf4ed
            surface: Color::Rgb(253, 253, 252),     // #fdfdfc
            text: Color::Rgb(33, 32, 28),           // #21201c
            muted: Color::Rgb(99, 99, 94),          // #63635e
            border: Color::Rgb(157, 156, 150),      // #9d9c96
            accent: Color::Rgb(184, 67, 0),         // #b84300
            success: Color::Rgb(33, 131, 88),       // #218358
            warning: Color::Rgb(154, 103, 0),       // #9a6700
            error: Color::Rgb(206, 44, 49),         // #ce2c31
            selected_bg: Color::Rgb(184, 67, 0),    // #b84300
            selected_fg: Color::Rgb(255, 253, 252), // #fffdfc
        },
    },
    PaletteDef {
        id: "dark",
        tokens: Tokens {
            canvas: Color::Rgb(17, 17, 16),        // #111110
            surface: Color::Rgb(28, 28, 26),       // #1c1c1a
            text: Color::Rgb(238, 238, 236),       // #eeeeec
            muted: Color::Rgb(181, 179, 173),      // #b5b3ad
            border: Color::Rgb(90, 89, 84),        // #5a5954
            accent: Color::Rgb(255, 160, 87),      // #ffa057
            success: Color::Rgb(61, 214, 140),     // #3dd68c
            warning: Color::Rgb(235, 193, 119),    // #ebc177
            error: Color::Rgb(255, 149, 146),      // #ff9592
            selected_bg: Color::Rgb(255, 160, 87), // #ffa057
            selected_fg: Color::Rgb(17, 17, 16),   // #111110
        },
    },
    PaletteDef {
        id: "catppuccin-mocha",
        tokens: Tokens {
            canvas: Color::Rgb(30, 30, 46),         // #1e1e2e
            surface: Color::Rgb(24, 24, 37),        // #181825
            text: Color::Rgb(205, 214, 244),        // #cdd6f4
            muted: Color::Rgb(166, 173, 200),       // #a6adc8
            border: Color::Rgb(88, 91, 112),        // #585b70
            accent: Color::Rgb(137, 180, 250),      // #89b4fa
            success: Color::Rgb(166, 227, 161),     // #a6e3a1
            warning: Color::Rgb(249, 226, 175),     // #f9e2af
            error: Color::Rgb(243, 139, 168),       // #f38ba8
            selected_bg: Color::Rgb(203, 166, 247), // #cba6f7
            selected_fg: Color::Rgb(30, 30, 46),    // #1e1e2e
        },
    },
    PaletteDef {
        id: "catppuccin-latte",
        tokens: Tokens {
            canvas: Color::Rgb(239, 241, 245),      // #eff1f5
            surface: Color::Rgb(230, 233, 239),     // #e6e9ef
            text: Color::Rgb(76, 79, 105),          // #4c4f69
            muted: Color::Rgb(108, 111, 133),       // #6c6f85
            border: Color::Rgb(172, 176, 190),      // #acb0be
            accent: Color::Rgb(30, 102, 245),       // #1e66f5
            success: Color::Rgb(64, 160, 43),       // #40a02b
            warning: Color::Rgb(223, 142, 29),      // #df8e1d
            error: Color::Rgb(210, 15, 57),         // #d20f39
            selected_bg: Color::Rgb(136, 57, 239),  // #8839ef
            selected_fg: Color::Rgb(239, 241, 245), // #eff1f5
        },
    },
    PaletteDef {
        id: "nord",
        tokens: Tokens {
            canvas: Color::Rgb(46, 52, 64),         // #2e3440
            surface: Color::Rgb(59, 66, 82),        // #3b4252
            text: Color::Rgb(216, 222, 233),        // #d8dee9
            muted: Color::Rgb(76, 86, 106),         // #4c566a
            border: Color::Rgb(59, 66, 82),         // #3b4252
            accent: Color::Rgb(136, 192, 208),      // #88c0d0
            success: Color::Rgb(163, 190, 140),     // #a3be8c
            warning: Color::Rgb(235, 203, 139),     // #ebcb8b
            error: Color::Rgb(191, 97, 106),        // #bf616a
            selected_bg: Color::Rgb(136, 192, 208), // #88c0d0
            selected_fg: Color::Rgb(46, 52, 64),    // #2e3440
        },
    },
    PaletteDef {
        id: "everforest",
        tokens: Tokens {
            canvas: Color::Rgb(45, 53, 59),         // #2d353b
            surface: Color::Rgb(52, 63, 68),        // #343f44
            text: Color::Rgb(211, 198, 170),        // #d3c6aa
            muted: Color::Rgb(133, 146, 137),       // #859289
            border: Color::Rgb(61, 72, 77),         // #3d484d
            accent: Color::Rgb(167, 192, 128),      // #a7c080
            success: Color::Rgb(131, 192, 146),     // #83c092
            warning: Color::Rgb(219, 188, 127),     // #dbbc7f
            error: Color::Rgb(230, 126, 128),       // #e67e80
            selected_bg: Color::Rgb(167, 192, 128), // #a7c080
            selected_fg: Color::Rgb(45, 53, 59),    // #2d353b
        },
    },
    PaletteDef {
        id: "tokyo-night",
        tokens: Tokens {
            canvas: Color::Rgb(26, 27, 38),         // #1a1b26
            surface: Color::Rgb(36, 40, 59),        // #24283b
            text: Color::Rgb(192, 202, 245),        // #c0caf5
            muted: Color::Rgb(86, 95, 137),         // #565f89
            border: Color::Rgb(59, 66, 97),         // #3b4261
            accent: Color::Rgb(122, 162, 247),      // #7aa2f7
            success: Color::Rgb(158, 206, 106),     // #9ece6a
            warning: Color::Rgb(224, 175, 104),     // #e0af68
            error: Color::Rgb(247, 118, 142),       // #f7768e
            selected_bg: Color::Rgb(122, 162, 247), // #7aa2f7
            selected_fg: Color::Rgb(26, 27, 38),    // #1a1b26
        },
    },
    PaletteDef {
        id: "gruvbox",
        tokens: Tokens {
            canvas: Color::Rgb(40, 40, 40),        // #282828
            surface: Color::Rgb(50, 48, 47),       // #32302f
            text: Color::Rgb(235, 219, 178),       // #ebdbb2
            muted: Color::Rgb(168, 153, 132),      // #a89984
            border: Color::Rgb(80, 73, 69),        // #504945
            accent: Color::Rgb(254, 128, 25),      // #fe8019
            success: Color::Rgb(184, 187, 38),     // #b8bb26
            warning: Color::Rgb(250, 189, 47),     // #fabd2f
            error: Color::Rgb(251, 73, 52),        // #fb4934
            selected_bg: Color::Rgb(254, 128, 25), // #fe8019
            selected_fg: Color::Rgb(40, 40, 40),   // #282828
        },
    },
    PaletteDef {
        id: "high-contrast",
        tokens: Tokens {
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
    },
    PaletteDef {
        id: "no-color",
        tokens: Tokens {
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
    },
];

/// Variant order paired one-to-one with `PALETTES`. Keeping the mapping here
/// makes `all()`, `palette_index`, `parse_id` and `palette()` agree by
/// construction instead of by four hand-maintained match arms.
static PALETTE_ORDER: [Palette; 10] = [
    Palette::Cream,
    Palette::Dark,
    Palette::CatppuccinMocha,
    Palette::CatppuccinLatte,
    Palette::Nord,
    Palette::Everforest,
    Palette::TokyoNight,
    Palette::Gruvbox,
    Palette::HighContrast,
    Palette::NoColor,
];

/// Picker display names, in the same order as `PALETTE_ORDER`.
static PALETTE_LABELS: [&str; 10] = [
    "Cream",
    "Dark",
    "Catppuccin Mocha",
    "Catppuccin Latte",
    "Nord",
    "Everforest",
    "Tokyo Night",
    "Gruvbox",
    "High Contrast",
    "No Color",
];

impl Tokens {
    pub fn for_palette(palette: Palette) -> Self {
        PALETTES[palette_index(palette) as usize].tokens
    }

    /// Whether these tokens carry any colour at all.
    pub fn colored(&self) -> bool {
        self.text != Color::Reset
    }
}

// An atomic rather than a OnceLock: `/set-theme` changes the palette at runtime,
// so `tokens()` must follow it. The stored u8 is a plain index into PALETTES and
// every write is a store, so repeated `init_palette` calls overwrite cleanly.
static ACTIVE_PALETTE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

fn palette_index(p: Palette) -> u8 {
    PALETTE_ORDER
        .iter()
        .position(|&candidate| candidate == p)
        .expect("every Palette variant is in PALETTE_ORDER") as u8
}

/// Set the process-wide palette. Called at startup and on `/set-theme` so every
/// surface shares one audited set of tokens. Calling it again simply replaces
/// the previous choice; there is no one-shot initialisation to fight.
pub fn init_palette(palette: Palette) {
    ACTIVE_PALETTE.store(palette_index(palette), std::sync::atomic::Ordering::Release);
}

/// The current palette. Defaults from the environment until first set.
pub fn palette() -> Palette {
    let idx = ACTIVE_PALETTE.load(std::sync::atomic::Ordering::Acquire) as usize;
    PALETTE_ORDER
        .get(idx)
        .copied()
        .unwrap_or_else(|| Palette::from_env(Theme::Default))
}

/// The current semantic tokens, following `/set-theme` changes.
pub fn tokens() -> Tokens {
    Tokens::for_palette(palette())
}

#[cfg(test)]
mod token_tests {
    use super::*;

    const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::Rgb(r, g, b)
    }

    #[test]
    fn every_palette_has_distinct_roles() {
        for &p in Palette::all() {
            let t = Tokens::for_palette(p);
            if p == Palette::NoColor {
                continue; // checked separately: every role is Reset by design
            }
            assert!(t.colored(), "{p:?} must carry colour");
            assert_ne!(t.text, t.muted, "{p:?}: muted must be a lighter step");
            assert_ne!(t.accent, t.error, "{p:?}: accent is not error");
            assert_ne!(t.selected_bg, t.canvas, "{p:?}: selection must be visible");
            // High contrast deliberately uses a white border on black; the
            // tonal-step rule applies only to the audited colour scales.
            if p != Palette::HighContrast {
                assert_ne!(t.text, t.border, "{p:?}: border must be a lighter step");
            }
        }
    }

    #[test]
    fn every_palette_text_and_background_are_distinct() {
        for &p in Palette::all() {
            if p == Palette::NoColor {
                continue;
            }
            let t = Tokens::for_palette(p);
            assert_ne!(t.text, t.canvas, "{p:?}: text must be readable on canvas");
            if p != Palette::HighContrast {
                assert_ne!(t.border, t.text, "{p:?}: border must differ from text");
            }
        }
    }

    #[test]
    fn verified_hex_values_are_locked_in() {
        // A snapshot of the researched values: changing a swatch must be a
        // deliberate edit to this test as well as to the registry.
        let expected: [(Palette, Tokens); 10] = [
            (
                Palette::Cream,
                Tokens {
                    canvas: rgb(250, 244, 237),
                    surface: rgb(253, 253, 252),
                    text: rgb(33, 32, 28),
                    muted: rgb(99, 99, 94),
                    border: rgb(157, 156, 150),
                    accent: rgb(184, 67, 0),
                    success: rgb(33, 131, 88),
                    warning: rgb(154, 103, 0),
                    error: rgb(206, 44, 49),
                    selected_bg: rgb(184, 67, 0),
                    selected_fg: rgb(255, 253, 252),
                },
            ),
            (
                Palette::Dark,
                Tokens {
                    canvas: rgb(17, 17, 16),
                    surface: rgb(28, 28, 26),
                    text: rgb(238, 238, 236),
                    muted: rgb(181, 179, 173),
                    border: rgb(90, 89, 84),
                    accent: rgb(255, 160, 87),
                    success: rgb(61, 214, 140),
                    warning: rgb(235, 193, 119),
                    error: rgb(255, 149, 146),
                    selected_bg: rgb(255, 160, 87),
                    selected_fg: rgb(17, 17, 16),
                },
            ),
            (
                Palette::CatppuccinMocha,
                Tokens {
                    canvas: rgb(30, 30, 46),
                    surface: rgb(24, 24, 37),
                    text: rgb(205, 214, 244),
                    muted: rgb(166, 173, 200),
                    border: rgb(88, 91, 112),
                    accent: rgb(137, 180, 250),
                    success: rgb(166, 227, 161),
                    warning: rgb(249, 226, 175),
                    error: rgb(243, 139, 168),
                    selected_bg: rgb(203, 166, 247),
                    selected_fg: rgb(30, 30, 46),
                },
            ),
            (
                Palette::CatppuccinLatte,
                Tokens {
                    canvas: rgb(239, 241, 245),
                    surface: rgb(230, 233, 239),
                    text: rgb(76, 79, 105),
                    muted: rgb(108, 111, 133),
                    border: rgb(172, 176, 190),
                    accent: rgb(30, 102, 245),
                    success: rgb(64, 160, 43),
                    warning: rgb(223, 142, 29),
                    error: rgb(210, 15, 57),
                    selected_bg: rgb(136, 57, 239),
                    selected_fg: rgb(239, 241, 245),
                },
            ),
            (
                Palette::Nord,
                Tokens {
                    canvas: rgb(46, 52, 64),
                    surface: rgb(59, 66, 82),
                    text: rgb(216, 222, 233),
                    muted: rgb(76, 86, 106),
                    border: rgb(59, 66, 82),
                    accent: rgb(136, 192, 208),
                    success: rgb(163, 190, 140),
                    warning: rgb(235, 203, 139),
                    error: rgb(191, 97, 106),
                    selected_bg: rgb(136, 192, 208),
                    selected_fg: rgb(46, 52, 64),
                },
            ),
            (
                Palette::Everforest,
                Tokens {
                    canvas: rgb(45, 53, 59),
                    surface: rgb(52, 63, 68),
                    text: rgb(211, 198, 170),
                    muted: rgb(133, 146, 137),
                    border: rgb(61, 72, 77),
                    accent: rgb(167, 192, 128),
                    success: rgb(131, 192, 146),
                    warning: rgb(219, 188, 127),
                    error: rgb(230, 126, 128),
                    selected_bg: rgb(167, 192, 128),
                    selected_fg: rgb(45, 53, 59),
                },
            ),
            (
                Palette::TokyoNight,
                Tokens {
                    canvas: rgb(26, 27, 38),
                    surface: rgb(36, 40, 59),
                    text: rgb(192, 202, 245),
                    muted: rgb(86, 95, 137),
                    border: rgb(59, 66, 97),
                    accent: rgb(122, 162, 247),
                    success: rgb(158, 206, 106),
                    warning: rgb(224, 175, 104),
                    error: rgb(247, 118, 142),
                    selected_bg: rgb(122, 162, 247),
                    selected_fg: rgb(26, 27, 38),
                },
            ),
            (
                Palette::Gruvbox,
                Tokens {
                    canvas: rgb(40, 40, 40),
                    surface: rgb(50, 48, 47),
                    text: rgb(235, 219, 178),
                    muted: rgb(168, 153, 132),
                    border: rgb(80, 73, 69),
                    accent: rgb(254, 128, 25),
                    success: rgb(184, 187, 38),
                    warning: rgb(250, 189, 47),
                    error: rgb(251, 73, 52),
                    selected_bg: rgb(254, 128, 25),
                    selected_fg: rgb(40, 40, 40),
                },
            ),
            (
                Palette::HighContrast,
                Tokens {
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
            ),
            (
                Palette::NoColor,
                Tokens {
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
            ),
        ];
        for (p, want) in expected {
            assert_eq!(Tokens::for_palette(p), want, "{p:?}");
        }
    }

    #[test]
    fn no_color_has_no_colour_but_keeps_roles() {
        let t = Tokens::for_palette(Palette::NoColor);
        assert!(!t.colored());
        for role in [
            t.canvas,
            t.surface,
            t.text,
            t.muted,
            t.border,
            t.accent,
            t.success,
            t.warning,
            t.error,
            t.selected_bg,
            t.selected_fg,
        ] {
            assert_eq!(role, Color::Reset);
        }
    }

    #[test]
    fn parse_id_round_trips_for_every_palette() {
        for &p in Palette::all() {
            let id = p.id();
            assert_eq!(Palette::parse_id(id), Some(p), "{id}");
            assert_eq!(
                Palette::parse_id(id.to_ascii_uppercase().as_str()),
                Some(p),
                "{id}"
            );
            assert_eq!(Palette::parse_id(&format!("  {id}  ")), Some(p), "{id}");
        }
        assert_eq!(Palette::parse_id("light"), Some(Palette::Cream));
        assert_eq!(Palette::parse_id("default"), Some(Palette::Dark));
        assert_eq!(Palette::parse_id("contrast"), Some(Palette::HighContrast));
        assert_eq!(Palette::parse_id("not-a-theme"), None);
    }

    #[test]
    fn registry_is_one_to_one_and_indexed() {
        assert_eq!(Palette::all().len(), PALETTES.len());
        assert_eq!(PALETTE_LABELS.len(), PALETTES.len());
        let mut ids = std::collections::HashSet::new();
        let mut labels = std::collections::HashSet::new();
        for (idx, &p) in Palette::all().iter().enumerate() {
            assert_eq!(palette_index(p), idx as u8);
            assert!(ids.insert(p.id()), "duplicate id: {}", p.id());
            assert!(labels.insert(p.label()), "duplicate label: {}", p.label());
            assert_eq!(Tokens::for_palette(p), PALETTES[idx].tokens);
        }
        assert_eq!(ids.len(), PALETTES.len());
        assert_eq!(labels.len(), PALETTES.len());
    }

    #[test]
    fn no_color_env_overrides_palette_choice() {
        assert_eq!(Palette::from_env(Theme::NoColor), Palette::NoColor);
        assert_eq!(
            Palette::from_env(Theme::HighContrast),
            Palette::HighContrast
        );
    }

    #[test]
    fn init_palette_follows_repeated_calls() {
        // An AtomicU8 store overwrites cleanly: the picker can call this on
        // every cursor move and again on Enter without a one-shot lock.
        for &p in Palette::all() {
            init_palette(p);
            assert_eq!(palette(), p);
            assert_eq!(tokens(), Tokens::for_palette(p));
        }
        // Leave shared process state as it was found (env-derived default).
        ACTIVE_PALETTE.store(u8::MAX, std::sync::atomic::Ordering::Release);
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

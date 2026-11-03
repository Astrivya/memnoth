//! Reusable ratatui widgets extracted from the memnoth TUI.
//!
//! This crate contains no application logic — only pure rendering components
//! that can be composed by any ratatui-based interface.

pub mod confirm;
pub mod diff;
pub mod keybindings;
pub mod palette;
pub mod spinner;
pub mod table;
pub mod toast;
pub mod tree;
pub mod util;

#[cfg(test)]
mod tests {
    use super::*;
}

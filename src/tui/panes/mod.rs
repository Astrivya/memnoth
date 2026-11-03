//! TUI Panes: composable UI components for the session screen.

pub mod context;
pub mod diff;
pub mod tasks;
pub mod transcript;

pub use context::ContextPane;
pub use diff::DiffPane;
pub use tasks::TasksPane;
pub use transcript::TranscriptPane;
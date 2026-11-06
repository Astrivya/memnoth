//! Command specification, registry, and derive macro for memnoth CLI.
//!
//! This crate provides the single source of truth for all memnoth commands.
//! Every surface (CLI, TUI palette, slash commands, MCP tools, docs, completions)
//! derives its shape from the [`CommandSpec`] rows registered in the
//! [`CommandRegistry`].
//!
//! The design principle is **parity**: one spec row → identical behavior across
//! all surfaces. The parity test in `tests/parity.rs` asserts this invariant.

#![warn(missing_docs)]

pub mod commands;
pub mod registry;
pub mod spec;
pub mod test_parity;

pub use commands::build_p0_registry;
pub use registry::{CommandRegistry, RegistryBuilder};
pub use spec::{AuthLevel, CommandSpec, DangerLevel, OverlayType, ParamSpec, ParamType, SlashSpec};
pub use test_parity::assert_parity;

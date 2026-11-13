//! memnoth-lsp — the memnoth language-server boundary.
//!
//! A small, dependency-light JSON-RPC client over stdio that starts, talks to,
//! and stops per-language servers (rust-analyzer, pyright, tsserver, gopls,
//! jdtls, clangd, ruby-lsp, ...). It owns the protocol framing and the
//! lifecycle policy; the code-graph crate (`memnoth-codegraph`) owns the
//! per-language server table and the auto-start allowlist.
//!
//! Every answer this client helps produce carries a [`Precision`] label from
//! `memnoth-codegraph`, so a caller can always tell whether a definition or a
//! reference came from a resolved language-server edge or from the lexical
//! fallback (W7-01, W7-02).
//!
//! # Surface
//!
//! - [`LspClient`] — spawns one server per language, speaks JSON-RPC over stdio,
//!   keeps documents in sync, answers definition/references/symbols/hover.
//! - [`AutoStartPolicy`] — re-exported from `memnoth-codegraph::lsp`; nothing is
//!   spawned unless `auto_lsp` is set, an extension matches, a root marker is
//!   present, and the language is on the allowlist (W7-05).
//! - [`PositionEncoding`] — byte/UTF-16/UTF-32 offset conversion negotiated with
//!   the server (W7-03).
//! - [`encode_frame`] / [`decode_frame`] — `Content-Length` JSON-RPC framing.

pub mod apply;
pub mod client;
pub mod protocol;
pub mod server;

pub use memnoth_codegraph::lsp::{
    AutoStartPolicy, PositionEncoding, Refusal, ServerSpec, StartDecision, decode_frame,
    encode_frame, server_for_extension, server_table,
};
pub use memnoth_codegraph::precision::{Capabilities, GraphAnswer, Precision, best_available};

pub use apply::{apply_to_text, apply_workspace_edit};
/// Re-export the LSP boundary items the CLI and MCP surfaces consume.
pub use client::{LspClient, LspClientHandle};

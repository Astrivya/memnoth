//! One tool registry for every memnoth surface.
//!
//! The product had two tool lists: eleven hand-written runtime tools and a
//! forty-eight-tool MCP server, in the same workspace, sharing nothing. The
//! lists drifted because nothing forced them to agree, and the CLI — the surface
//! scripts and CI actually have — could not reach the larger one at all.
//!
//! This crate is the fix, and it is deliberately small: a tool is a
//! [`ToolDef`] (name, description, schema, approval, cost class, cache class)
//! plus one `call`. Everything else — profiles, receipts, ordering, JSON shape
//! on the wire — is derived from that, so a new tool is one entry rather than
//! three registrations that can disagree.
//!
//! ```no_run
//! use memnoth_capability::{Profile, Registry, ToolContext};
//! use serde_json::json;
//!
//! let registry = Registry::with_builtins();
//! let context = ToolContext::new("/repo");
//! let call = registry.execute("fs_list", &json!({ "path": "." }), &context, Profile::Code);
//! println!("{} bytes out in {} ms", call.receipt().bytes_out, call.receipt().ms);
//! ```

#![warn(missing_docs)]

pub mod profile;
pub mod receipt;
pub mod registry;
pub mod tools;

pub use profile::{Profile, ProfileSurface, RUNTIME_TOOL_PROFILES, TOOL_PROFILES, surface};
pub use receipt::{Approval, CacheClass, CostClass, Receipt};
pub use registry::{Call, Registry, Tool, ToolContext, ToolDef};

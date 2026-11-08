//! The registry: one definition per tool, one path to run it.
//!
//! `execute` is the only way a surface runs a tool. That matters more than it
//! sounds: because every surface goes through this function, the profile check,
//! the receipt and the error shape cannot be forgotten on one of them — which is
//! exactly how the runtime ended up with eleven tools and no receipts while the
//! server had forty-eight and its own.

use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::profile::Profile;
use crate::receipt::{Approval, CacheClass, CostClass, Receipt};

/// Everything static about a tool.
///
/// `schema` is a function rather than a value so a definition costs nothing to
/// construct and is rebuilt only when a surface asks for `tools/list`.
pub struct ToolDef {
    /// The name on the wire, e.g. `kg_impact`.
    pub name: &'static str,
    /// One paragraph, written for routing rather than for documentation.
    pub description: &'static str,
    /// JSON Schema for the arguments.
    pub schema: fn() -> Value,
    /// What the caller must consent to.
    pub approval: Approval,
    /// How expensive the call is, for routing hints.
    pub cost: CostClass,
    /// Where the answer may be cached.
    pub cache: CacheClass,
}

impl ToolDef {
    /// The MCP/`tools/list` shape, so a server can serve definitions it does not
    /// own.
    pub fn to_wire(&self) -> Value {
        let annotations = match self.approval {
            Approval::ReadOnly => json!({ "readOnlyHint": true }),
            Approval::MutatesWorkspace => {
                json!({ "readOnlyHint": false, "destructiveHint": false })
            }
            Approval::Destructive => json!({ "readOnlyHint": false, "destructiveHint": true }),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": (self.schema)(),
            "annotations": annotations,
        })
    }
}

/// What a tool runs against.
///
/// The scratchpad is here rather than in a tool because `think` is a conversation
/// with itself: the chain has to survive between calls in one session, and a
/// tool that owned it would be a second place that knows about sessions.
pub struct ToolContext {
    /// The workspace a tool may read. Absolute paths outside it are the caller's
    /// decision, not the tool's.
    pub workspace: PathBuf,
    scratch: Mutex<Vec<String>>,
    /// A surface's own context, carried through the registry untouched.
    ///
    /// This is what lets one registry run tools that belong to different surfaces: the
    /// runtime's tools need its vault path and its consent flag, the MCP server needs its
    /// database handle, and neither can be named here without this crate learning every
    /// surface. A tool that needs it downcasts; a tool that does not never looks.
    extra: Option<Arc<dyn Any + Send + Sync>>,
}

impl ToolContext {
    /// A context rooted at `workspace`.
    pub fn new(workspace: impl AsRef<Path>) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
            scratch: Mutex::new(Vec::new()),
            extra: None,
        }
    }

    /// Attach the calling surface's own context.
    pub fn with_extra<T: Any + Send + Sync>(mut self, extra: T) -> Self {
        self.extra = Some(Arc::new(extra));
        self
    }

    /// The attached context, when it is the type the caller expects.
    pub fn extra<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.extra.as_ref()?.downcast_ref::<T>()
    }

    /// Append one reasoning step and return the chain so far.
    pub fn push_thought(&self, step: &str) -> Vec<String> {
        // A poisoned lock means a previous call panicked while holding it. The
        // chain is still readable, and losing the session's scratchpad would be
        // worse than continuing with it, so recover rather than abort.
        let mut chain = match self.scratch.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        chain.push(step.to_string());
        chain.clone()
    }
}

/// A capability.
pub trait Tool: Send + Sync {
    /// The static definition; `&'static` because definitions live for the
    /// process and copying them per call would allocate on the hot path.
    fn definition(&self) -> &'static ToolDef;

    /// Run the tool. Arguments have already been parsed as JSON by the caller.
    fn call(&self, args: &Value, context: &ToolContext) -> Result<Value>;
}

/// The result of `Registry::execute`.
///
/// A refusal is not an error: it is an answer with a reason, and a surface that
/// treats it as a crash shows the user a stack trace instead of "this profile
/// does not include `fs_list`".
#[derive(Debug)]
pub enum Call {
    /// The tool ran.
    Ok {
        /// Its JSON result.
        value: Value,
        /// What it cost.
        receipt: Receipt,
    },
    /// The profile does not expose this tool.
    Refused {
        /// Why, in a sentence a user can act on.
        reason: String,
        /// A receipt marked `refused`.
        receipt: Receipt,
    },
    /// The tool ran and failed.
    Failed {
        /// The failure, already formatted for a human.
        error: String,
        /// What the attempt cost before it failed.
        receipt: Receipt,
    },
}

impl Call {
    /// The receipt for any outcome.
    pub fn receipt(&self) -> &Receipt {
        match self {
            Self::Ok { receipt, .. }
            | Self::Refused { receipt, .. }
            | Self::Failed { receipt, .. } => receipt,
        }
    }

    /// The value, or the reason it is not there.
    pub fn into_result(self) -> Result<Value> {
        match self {
            Self::Ok { value, .. } => Ok(value),
            Self::Refused { reason, .. } => bail!("{reason}"),
            Self::Failed { error, .. } => bail!("{error}"),
        }
    }
}

/// Every tool this build can run.
pub struct Registry {
    tools: BTreeMap<&'static str, Box<dyn Tool>>,
}

impl Registry {
    /// A registry with the ported tools.
    ///
    /// Adding a tool is one line here; the exposure table in [`crate::TOOL_PROFILES`]
    /// is the other half, and a test fails if the two disagree.
    pub fn with_builtins() -> Self {
        let mut registry = Self {
            tools: BTreeMap::new(),
        };
        registry.add(crate::tools::Now);
        registry.add(crate::tools::Think);
        registry.add(crate::tools::FsList);
        registry
    }

    /// An empty registry, for a surface that installs its own set.
    pub fn empty() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    /// Install a tool, replacing one of the same name.
    pub fn add(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.definition().name, Box::new(tool));
    }

    /// Every tool name, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }

    /// The `tools/list` payload for `profile`, in name order.
    ///
    /// The bytes here are the token cost of a profile, which is why the order is
    /// stable: a stable list is a cacheable list.
    pub fn definitions(&self, profile: Profile) -> Vec<Value> {
        self.tools
            .values()
            .filter(|tool| profile.allows(tool.definition().name))
            .map(|tool| tool.definition().to_wire())
            .collect()
    }

    /// The OAuth scope a tool needs on an authenticated transport.
    ///
    /// Derived from the approval class rather than declared a second time: a
    /// tool that changes the workspace needs `mcp:write`, a destructive one
    /// needs `mcp:admin`, and a read needs nothing beyond the profile that
    /// already gates it. One table means a new mutating tool cannot ship
    /// unscoped by forgetting a field.
    pub fn required_scope(&self, name: &str) -> Option<&'static str> {
        let tool = self.tools.get(name)?;
        match tool.definition().approval {
            Approval::ReadOnly => None,
            Approval::MutatesWorkspace => Some("mcp:write"),
            Approval::Destructive => Some("mcp:admin"),
        }
    }

    /// Run one tool under one profile.
    pub fn execute(
        &self,
        name: &str,
        args: &Value,
        context: &ToolContext,
        profile: Profile,
    ) -> Call {
        let Some(tool) = self.tools.get(name) else {
            return Call::Failed {
                error: format!("unknown tool `{name}`"),
                receipt: Receipt::refused(
                    name,
                    profile.name(),
                    CacheClass::NoStore,
                    "unknown tool",
                ),
            };
        };
        let definition = tool.definition();

        if !profile.allows(name) {
            return Call::Refused {
                reason: format!(
                    "tool `{name}` is not in profile `{}`; use `full`, or a profile that lists it",
                    profile.name()
                ),
                receipt: Receipt::refused(name, profile.name(), definition.cache, "not in profile"),
            };
        }

        let started = Instant::now();
        match tool.call(args, context) {
            Ok(value) => {
                let bytes = serde_json::to_string(&value)
                    .map(|text| text.len() as u64)
                    .unwrap_or(0);
                Call::Ok {
                    value,
                    receipt: Receipt::measured(
                        name,
                        profile.name(),
                        definition.cache,
                        bytes,
                        started,
                    ),
                }
            }
            Err(error) => Call::Failed {
                error: format!("{error:#}"),
                receipt: Receipt::measured(name, profile.name(), definition.cache, 0, started),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    #[test]
    fn every_builtin_is_callable_and_carries_a_receipt() {
        let registry = Registry::with_builtins();
        let names = registry.names();
        assert!(names.contains(&"now"), "names: {names:?}");
        assert!(names.contains(&"think"));
        assert!(names.contains(&"fs_list"));

        for name in names {
            let call = registry.execute(name, &json!({ "path": "." }), &workspace(), Profile::Full);
            assert!(!call.receipt().refused, "{name} refused under full");
            assert_eq!(call.receipt().tool, name);
        }
    }

    #[test]
    fn a_profile_that_does_not_list_a_tool_refuses_it_with_a_reason() {
        let registry = Registry::with_builtins();
        let call = registry.execute(
            "fs_list",
            &json!({ "path": "." }),
            &workspace(),
            Profile::Memory,
        );

        match call {
            Call::Refused { reason, receipt } => {
                assert!(reason.contains("fs_list"));
                assert!(reason.contains("memory"));
                assert!(receipt.refused);
                assert_eq!(receipt.bytes_out, 0);
            }
            other => panic!("expected a refusal, got {}", other.receipt().tool),
        }
    }

    #[test]
    fn definitions_are_filtered_by_profile() {
        let registry = Registry::with_builtins();
        let code = registry.definitions(Profile::Code);
        let memory = registry.definitions(Profile::Memory);

        assert!(
            code.len() > memory.len(),
            "code should expose more than memory here"
        );
        assert!(memory.iter().all(|def| def["name"] != json!("fs_list")));
    }

    #[test]
    fn definitions_are_stable_and_mcp_shaped() {
        let registry = Registry::with_builtins();
        let first = registry.definitions(Profile::Full);
        let second = registry.definitions(Profile::Full);
        assert_eq!(first, second, "a stable list is a cacheable list");

        for def in first {
            assert!(def["name"].is_string());
            assert!(
                def["inputSchema"].is_object(),
                "{} needs a schema",
                def["name"]
            );
            assert!(def["annotations"].is_object());
        }
    }

    #[test]
    fn an_unknown_tool_fails_rather_than_inventing_one() {
        let registry = Registry::with_builtins();
        let call = registry.execute("fs_purge", &json!({}), &workspace(), Profile::Full);
        let message = call.into_result().unwrap_err().to_string();
        assert!(message.contains("unknown tool"));
    }

    #[test]
    fn a_tool_error_is_a_failed_call_not_a_panic() {
        let registry = Registry::with_builtins();
        let call = registry.execute(
            "fs_list",
            &json!({ "path": "/definitely/not/a/directory/anywhere" }),
            &workspace(),
            Profile::Code,
        );
        assert!(matches!(call, Call::Failed { .. }));
    }
}

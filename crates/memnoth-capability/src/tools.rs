//! The first three ported tools.
//!
//! They are the ones with no store behind them, which is why they come first:
//! porting them proves the registry, the profile gate and the receipt without
//! dragging SQLite or the graph along. The rest of the MCP tool set follows the
//! same shape, one `Tool` impl each.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::receipt::{Approval, CacheClass, CostClass};
use crate::registry::{Tool, ToolContext, ToolDef};

/// Bounded, so one listing cannot become the turn's context.
const MAX_ENTRIES: usize = 500;

/// The wall clock.
///
/// Models drift on relative dates; this is the cheapest correction available and
/// it costs four bytes of schema.
pub struct Now;

static NOW: ToolDef = ToolDef {
    name: "now",
    description: "Current wall-clock time as epoch milliseconds and Unix seconds. Call it when the task \
                  mentions 'now', 'today', a date or a relative time — models drift without a clock.",
    schema: now_schema,
    approval: Approval::ReadOnly,
    cost: CostClass::Cheap,
    cache: CacheClass::NoStore,
};

fn now_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

impl Tool for Now {
    fn definition(&self) -> &'static ToolDef {
        &NOW
    }

    fn call(&self, _args: &Value, _context: &ToolContext) -> Result<Value> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("the system clock is before the Unix epoch")?;
        Ok(json!({
            "epoch_ms": elapsed.as_millis() as u64,
            "unix_s": elapsed.as_secs(),
        }))
    }
}

/// A reasoning scratchpad.
///
/// The value is that the chain is returned whole: a model that writes step 3 can
/// see steps 1 and 2 without them being re-sent as history, which is cheaper than
/// re-reading its own transcript.
pub struct Think;

static THINK: ToolDef = ToolDef {
    name: "think",
    description: "Append one reasoning step and get the chain so far back. Use it for multi-step plans or \
                  tradeoff analysis instead of reasoning in a single response.",
    schema: think_schema,
    approval: Approval::MutatesWorkspace,
    cost: CostClass::Cheap,
    cache: CacheClass::NoStore,
};

fn think_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "step": {
                "type": "string",
                "description": "One reasoning step to append, e.g. 'option A fails because X'"
            }
        },
        "required": ["step"],
        "additionalProperties": false
    })
}

impl Tool for Think {
    fn definition(&self) -> &'static ToolDef {
        &THINK
    }

    fn call(&self, args: &Value, context: &ToolContext) -> Result<Value> {
        let Some(step) = args.get("step").and_then(Value::as_str).map(str::trim) else {
            bail!("`step` must be a non-empty string");
        };
        if step.is_empty() {
            bail!("`step` must be a non-empty string");
        }

        let chain = context.push_thought(step);
        let steps: Vec<Value> = chain
            .iter()
            .enumerate()
            .map(|(index, text)| json!({ "n": index + 1, "text": text }))
            .collect();
        Ok(json!({ "steps": steps, "count": steps.len() }))
    }
}

/// A bounded directory listing.
pub struct FsList;

static FS_LIST: ToolDef = ToolDef {
    name: "fs_list",
    description: "List a directory: one entry per line with kind and size, bounded to 500 entries. Set \
                  depth:1 for one level below. Prefer this over a shell `ls` — the output is already \
                  structured and capped.",
    schema: fs_list_schema,
    approval: Approval::ReadOnly,
    cost: CostClass::Moderate,
    cache: CacheClass::Session,
};

fn fs_list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Directory path, relative to the workspace or absolute"
            },
            "depth": {
                "type": "integer",
                "description": "Recursion depth (0 = top level only, default 0)"
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

impl Tool for FsList {
    fn definition(&self) -> &'static ToolDef {
        &FS_LIST
    }

    fn call(&self, args: &Value, context: &ToolContext) -> Result<Value> {
        let Some(raw) = args.get("path").and_then(Value::as_str).map(str::trim) else {
            bail!("`path` must be a non-empty string");
        };
        if raw.is_empty() {
            bail!("`path` must be a non-empty string");
        }
        let depth = match args.get("depth").and_then(Value::as_u64) {
            Some(requested) => requested.min(2) as usize,
            None => 0,
        };

        let requested = context.workspace.join(raw);
        let root = requested
            .canonicalize()
            .with_context(|| format!("no such directory: {}", requested.display()))?;
        if !root.is_dir() {
            bail!("not a directory: {}", root.display());
        }

        let mut entries = Vec::new();
        let mut truncated = false;
        collect(&root, 0, depth, &mut entries, &mut truncated)?;

        Ok(json!({
            "path": root.display().to_string(),
            "entries": entries,
            "count": entries.len(),
            "truncated": truncated,
        }))
    }
}

fn collect(
    directory: &std::path::Path,
    level: usize,
    max_depth: usize,
    entries: &mut Vec<Value>,
    truncated: &mut bool,
) -> Result<()> {
    // An unreadable entry is an error, not a silently shorter listing: a caller
    // that cannot see a file would conclude the file is not there.
    let mut children: Vec<_> = Vec::new();
    for entry in
        fs::read_dir(directory).with_context(|| format!("cannot read {}", directory.display()))?
    {
        children.push(entry.with_context(|| format!("cannot read {}", directory.display()))?);
    }
    children.sort_by_key(|entry| entry.file_name());

    for child in children {
        if entries.len() >= MAX_ENTRIES {
            *truncated = true;
            return Ok(());
        }
        // A path that vanished between listing and stat is reported as an entry
        // with no size; anything else is a real error.
        let metadata = match child.metadata() {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot stat {}", child.path().display()));
            }
        };
        let is_dir = metadata.as_ref().is_some_and(|meta| meta.is_dir());
        entries.push(json!({
            "name": child.file_name().to_string_lossy(),
            "kind": if is_dir { "dir" } else { "file" },
            "size": metadata.as_ref().filter(|_| !is_dir).map(|meta| meta.len()),
        }));
        if is_dir && level < max_depth {
            collect(&child.path(), level + 1, max_depth, entries, truncated)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Each fixture gets its own directory: these tests run in parallel, and a
    // shared name means one test's cleanup deletes the other's fixture mid-run.
    fn fixture() -> (ToolContext, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "memnoth-capability-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested")).expect("fixture dir");
        fs::write(root.join("a.txt"), "hello").expect("fixture file");
        fs::write(root.join("nested/b.txt"), "hi").expect("nested file");
        (ToolContext::new(&root), root)
    }

    #[test]
    fn now_answers_in_epoch_units() {
        let value = Now.call(&json!({}), &ToolContext::new(".")).expect("now");
        let ms = value["epoch_ms"].as_u64().expect("epoch_ms");
        assert!(ms > 1_600_000_000_000, "epoch_ms looks like seconds: {ms}");
    }

    #[test]
    fn think_returns_the_whole_chain_numbered() {
        let context = ToolContext::new(".");
        Think
            .call(&json!({ "step": "first" }), &context)
            .expect("step 1");
        let value = Think
            .call(&json!({ "step": "second" }), &context)
            .expect("step 2");
        assert_eq!(value["count"], 2);
        assert_eq!(value["steps"][0]["n"], 1);
        assert_eq!(value["steps"][1]["text"], "second");
    }

    #[test]
    fn think_refuses_an_empty_step() {
        let error = Think
            .call(&json!({ "step": "   " }), &ToolContext::new("."))
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-empty"));
    }

    #[test]
    fn fs_list_is_sorted_and_reports_kinds() {
        let (context, root) = fixture();
        let value = FsList
            .call(&json!({ "path": "." }), &context)
            .expect("listing");
        let names: Vec<_> = value["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, vec!["a.txt", "nested"]);
        assert_eq!(value["entries"][1]["kind"], "dir");
        assert!(!value["truncated"].as_bool().unwrap_or(true));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fs_list_honours_depth() {
        let (context, root) = fixture();
        let value = FsList
            .call(&json!({ "path": ".", "depth": 1 }), &context)
            .expect("listing");
        let names: Vec<_> = value["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(names.contains(&"b.txt".to_string()), "names: {names:?}");
        let _ = fs::remove_dir_all(root);
    }
}

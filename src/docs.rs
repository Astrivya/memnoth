use anyhow::Context;
use memnoth_command::{CommandRegistry, build_p0_registry};
use std::fs;

use crate::cli::DocsArgs;
use crate::output::{ErrorOutcome, Outcome};

/// `mn docs` — generate Markdown documentation from the command registry.
pub fn run(_ctx: &crate::context::AppContext, args: &DocsArgs) -> Result<Outcome, ErrorOutcome> {
    let registry: CommandRegistry = build_p0_registry().build();
    let markdown = registry.build_docs();

    match &args.output {
        Some(path) => {
            fs::write(path, &markdown)
                .with_context(|| format!("Failed to write docs to {}", path.display()))?;
            Ok(Outcome::new(
                "docs",
                format!("Documentation written to {}", path.display()),
                serde_json::json!({ "output": path.display().to_string() }),
            ))
        }
        None => Ok(Outcome::new(
            "docs",
            markdown,
            serde_json::json!({ "output": null }),
        )),
    }
}

use anyhow::Context;
use memnoth_command::{build_p0_registry, CommandRegistry};
use std::fs;
use std::path::PathBuf;

use crate::output::{ErrorOutcome, Outcome};

/// `mn docs` — generate Markdown documentation from the command registry.
pub struct DocsArgs {
    /// Output file path. If not provided, writes to stdout.
    pub output: Option<PathBuf>,
}

pub fn run(ctx: &crate::context::AppContext, args: &DocsArgs) -> Result<Outcome, ErrorOutcome> {
    let registry: CommandRegistry = build_p0_registry().build();
    let markdown = registry.build_docs();

    match &args.output {
        Some(path) => {
            fs::write(path, markdown)
                .with_context(|| format!("Failed to write docs to {}", path.display()))?;
            Ok(Outcome::success(&format!("Documentation written to {}", path.display())))
        }
        None => {
            println!("{}", markdown);
            Ok(Outcome::success("Documentation printed to stdout"))
        }
    }
}
//! `cargo run -p memnoth-command -- docs --check` / `schema --check`.
//!
//! The CI `generated-surfaces-ci` job runs these against the crate directly;
//! `--check` fails when the checked-in surface is stale.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn docs_text() -> String {
    memnoth_command::build_p0_registry().build().build_docs()
}

fn schema_text() -> String {
    let registry = memnoth_command::build_p0_registry().build();
    let commands: Vec<_> = registry.all().collect();
    let document = serde_json::json!({
        "schema": "memnoth.commands/1",
        "command_count": registry.len(),
        "commands": commands,
    });
    serde_json::to_string_pretty(&document).expect("json renders")
}

fn run_one(
    kind: &str,
    generated: String,
    check: bool,
    output: Option<String>,
) -> Result<(), String> {
    let default = match kind {
        "docs" => "contract/command-surface.md",
        "schema" => "contract/commands.json",
        _ => return Err(format!("unknown kind {kind}")),
    };
    let path = PathBuf::from(output.unwrap_or_else(|| default.to_string()));
    if check {
        let existing = fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if existing != generated {
            return Err(format!(
                "{} is stale: the registry and the file disagree; \
                 run `cargo run -p memnoth-command -- {kind} --output {}` and review the diff",
                path.display(),
                path.display()
            ));
        }
        println!("{} matches the registry", path.display());
    } else {
        fs::write(&path, &generated)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let kind = args.first().map(String::as_str).unwrap_or("");
    let mut check = false;
    let mut output: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => check = true,
            "--output" => {
                output = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 1;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let generated = match kind {
        "docs" => docs_text(),
        "schema" => schema_text(),
        _ => {
            eprintln!("usage: memnoth-command <docs|schema> [--check] [--output <path>]");
            return ExitCode::from(2);
        }
    };
    match run_one(kind, generated, check, output) {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

use anyhow::Context;
use memnoth_command::{CommandRegistry, build_p0_registry};
use std::fs;

use crate::cli::DocsArgs;
use crate::output::{ErrorOutcome, ExitCode, Outcome};

/// `mn docs` — generate Markdown documentation from the command registry.
///
/// With `--check` the file is not written: it is generated and compared. Drift
/// exits 1 rather than printing a diff, because the caller that cares is a CI
/// step, and a check that rewrites what it checks proves nothing.
pub fn run(_ctx: &crate::context::AppContext, args: &DocsArgs) -> Result<Outcome, ErrorOutcome> {
    if args.man {
        return run_man(args);
    }

    let registry: CommandRegistry = build_p0_registry().build();
    let markdown = registry.build_docs();

    if args.check {
        return check(
            &markdown,
            args,
            "reference",
            "mn docs --check --output contract/command-surface.md",
            "mn docs",
        );
    }

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

/// Render the root command as a roff man page.
///
/// The same clap projection the binary parses is the source, so a verb cannot
/// be in the man page and missing from `--help`.
fn run_man(args: &DocsArgs) -> Result<Outcome, ErrorOutcome> {
    use clap::CommandFactory;

    let command = crate::cli::Cli::command().bin_name("memnoth");
    let man = clap_mangen::Man::new(command);
    let mut buffer = Vec::new();
    man.render(&mut buffer)
        .map_err(|error| ErrorOutcome::new(ExitCode::Unexpected, error.to_string()))?;
    let page = String::from_utf8_lossy(&buffer).into_owned();

    if args.check {
        return check(
            &page,
            args,
            "man page",
            "mn docs --check --man --output contract/memnoth.1",
            "mn docs --man",
        );
    }
    match &args.output {
        Some(path) => {
            fs::write(path, &page)
                .with_context(|| format!("Failed to write the man page to {}", path.display()))?;
            Ok(Outcome::new(
                "docs.man",
                format!("Man page written to {}", path.display()),
                serde_json::json!({ "output": path.display().to_string(), "man": true }),
            ))
        }
        None => Ok(Outcome::new(
            "docs.man",
            page,
            serde_json::json!({ "output": null, "man": true }),
        )),
    }
}

/// Compare one generated artifact against what the registry would write.
fn check(
    generated: &str,
    args: &DocsArgs,
    kind: &str,
    hint: &str,
    write_hint: &str,
) -> Result<Outcome, ErrorOutcome> {
    let Some(path) = &args.output else {
        return Err(ErrorOutcome::usage(
            "`docs --check` needs --output: there is no file to compare against",
        )
        .with_fix(format!("run `{hint}`")));
    };

    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ErrorOutcome::not_found(format!(
                "{} does not exist, so there is nothing to compare",
                path.display()
            ))
            .with_fix(format!("run `{write_hint} --output {}`", path.display())));
        }
        Err(error) => {
            return Err(ErrorOutcome::new(
                ExitCode::Unexpected,
                format!("cannot read {}: {error}", path.display()),
            ));
        }
    };

    if existing == generated {
        return Ok(Outcome::new(
            "docs",
            format!("{} matches the generated {kind}", path.display()),
            serde_json::json!({
                "output": path.display().to_string(),
                "check": true,
                "matches": true,
            }),
        ));
    }

    // Count the lines that differ rather than printing them: a stale file's diff
    // belongs in the review that regenerates it, and a firehose here would be
    // skimmed. The one number that matters is that it is not zero.
    let changed = existing
        .lines()
        .zip(generated.lines())
        .filter(|(old, new)| old != new)
        .count()
        + existing.lines().count().abs_diff(generated.lines().count());

    Err(ErrorOutcome::new(
        ExitCode::Unexpected,
        format!(
            "{} is stale: {changed} line(s) differ from the registry",
            path.display()
        ),
    )
    .with_fix(format!("run `{hint}` without --check and review the diff")))
}

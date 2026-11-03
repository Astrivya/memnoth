//! Command registry with builders for all surfaces.

use crate::spec::{CommandSpec, ParamSpec, ParamType};
use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::boxed::Box;

/// Built artifacts for a single surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltSurface {
    /// Surface name.
    pub name: String,
    /// JSON representation of the built surface.
    pub data: Value,
}

/// Command registry — the single source of truth for all commands.
#[derive(Debug, Default, Clone)]
pub struct CommandRegistry {
    commands: IndexMap<String, CommandSpec>,
    categories: std::collections::BTreeMap<String, Vec<String>>,
}

impl CommandRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command spec.
    pub fn register(&mut self, spec: CommandSpec) -> Result<()> {
        if self.commands.contains_key(&spec.id) {
            bail!("Command with id '{}' already registered", spec.id);
        }
        for cat in &spec.slash.categories {
            self.categories
                .entry(cat.clone())
                .or_default()
                .push(spec.id.clone());
        }
        self.commands.insert(spec.id.clone(), spec);
        Ok(())
    }

    /// Get a command by ID.
    pub fn get(&self, id: &str) -> Option<&CommandSpec> {
        self.commands.get(id)
    }

    /// Get all commands.
    pub fn all(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands.values()
    }

    /// Get commands by category.
    pub fn by_category(&self, category: &str) -> Vec<&CommandSpec> {
        self.categories
            .get(category)
            .map(|ids| ids.iter().filter_map(|id| self.commands.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get all categories.
    pub fn categories(&self) -> Vec<&String> {
        self.categories.keys().collect()
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Build a clap Command from the registry.
    pub fn build_clap(&self) -> clap::Command {
        use clap::{Arg, ArgAction, Command as ClapCommand};

        let mut root = ClapCommand::new("memnoth")
            .version(env!("CARGO_PKG_VERSION"))
            .about("Terminal agent, bi-temporal vault, code graph and MCP server")
            .long_about("One binary for the whole local product.")
            .after_help("Run `memnoth doctor` when something is not wired.");

        // Global arguments
        root = root
            .arg(
                Arg::new("root")
                    .long("root")
                    .global(true)
                    .value_name("DIR")
                    .help("Workspace to read."),
            )
            .arg(
                Arg::new("vault")
                    .long("vault")
                    .global(true)
                    .value_name("PATH")
                    .help("Vault database."),
            )
            .arg(
                Arg::new("namespace")
                    .long("namespace")
                    .global(true)
                    .value_name("NAME")
                    .default_value("default")
                    .help("Vault namespace."),
            )
            .arg(
                Arg::new("json")
                    .long("json")
                    .global(true)
                    .action(ArgAction::SetTrue)
                    .help("JSON output."),
            )
            .arg(
                Arg::new("quiet")
                    .long("quiet")
                    .global(true)
                    .action(ArgAction::SetTrue)
                    .help("Suppress progress."),
            )
            .arg(
                Arg::new("resume")
                    .long("resume")
                    .global(true)
                    .value_name("NAME")
                    .help("Resume session."),
            );

        // Build subcommands from registry
        for spec in self.all() {
            if spec.cli_path.is_empty() {
                continue;
            }

            let name: &'static str = &*Box::leak(spec.name().to_string().into_boxed_str());
            let about: &'static str = &*Box::leak(spec.slash.description.clone().into_boxed_str());
            let long: &'static str = &*Box::leak(
                spec.slash
                    .long_description
                    .clone()
                    .unwrap_or_else(|| spec.slash.description.clone())
                    .into_boxed_str(),
            );
            let mut cmd = ClapCommand::new(name).about(about).long_about(long);

            for param in &spec.slash.params {
                if param.hidden {
                    continue;
                }
                let long_name: &'static str = &*Box::leak(
                    param
                        .long
                        .clone()
                        .unwrap_or_else(|| param.name.replace('_', "-"))
                        .into_boxed_str(),
                );
                let help: &'static str = &*Box::leak(param.description.clone().into_boxed_str());
                let param_name: &str = &*Box::leak(param.name.clone().into_boxed_str());
                let mut arg = Arg::new(param_name).long(long_name).help(help);

                if let Some(short) = param.short {
                    arg = arg.short(short);
                }

                if param.required && !param.positional {
                    arg = arg.required(true);
                }

                if param.positional {
                    arg = arg.index(param.position.unwrap_or(0));
                }

                match &param.param_type {
                    ParamType::Boolean => {
                        arg = arg.action(ArgAction::SetTrue).num_args(0);
                    }
                    ParamType::Enum { choices } => {
                        let vals: Vec<&str> = choices
                            .iter()
                            .map(|s| &*Box::leak(s.clone().into_boxed_str()))
                            .collect();
                        arg = arg.value_parser(clap::builder::PossibleValuesParser::new(vals));
                    }
                    ParamType::Integer { .. } => {
                        arg = arg.value_parser(clap::value_parser!(i64));
                    }
                    ParamType::Float { .. } => {
                        arg = arg.value_parser(clap::value_parser!(f64));
                    }
                    ParamType::String { .. } => {}
                    ParamType::Array { .. } => {
                        arg = arg.num_args(1..);
                    }
                    ParamType::Path { .. } => {
                        arg = arg.value_parser(clap::value_parser!(std::path::PathBuf));
                    }
                    ParamType::Json => {
                        arg = arg.value_parser(clap::value_parser!(serde_json::Value));
                    }
                    ParamType::Duration => {}
                    ParamType::Timestamp => {}
                }

                cmd = cmd.arg(arg);
            }

            for alias in &spec.slash.aliases {
                let alias_static: &'static str = &*Box::leak(alias.clone().into_boxed_str());
                cmd = cmd.alias(alias_static);
            }

            root = root.subcommand(cmd);
        }

        root
    }

    /// Build palette data (for TUI command palette).
    pub fn build_palette(&self) -> Value {
        let items: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "name": c.name(),
                    "description": c.slash.description,
                    "cli_command": c.cli_command(),
                    "categories": c.slash.categories,
                    "overlay": c.slash.overlay,
                    "danger": c.slash.danger,
                    "auth": c.slash.auth,
                    "aliases": c.slash.aliases,
                    "examples": c.slash.examples,
                    "params": c.slash.params.iter().map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                            "required": p.required,
                            "type": p.param_type,
                            "default": p.default,
                            "short": p.short,
                            "positional": p.positional,
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();

        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commands": items,
        })
    }

    /// Build slash command data (for TUI slash commands).
    pub fn build_slash(&self) -> Value {
        let commands: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                serde_json::json!({
                    "name": c.slash.name,
                    "description": c.slash.description,
                    "long_description": c.slash.long_description,
                    "params": c.slash.params,
                    "overlay": c.slash.overlay,
                    "danger": c.slash.danger,
                    "auth": c.slash.auth,
                    "profile": c.slash.profile,
                    "categories": c.slash.categories,
                    "aliases": c.slash.aliases,
                    "examples": c.slash.examples,
                    "deprecated": c.slash.deprecated,
                })
            })
            .collect();

        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commands": commands,
        })
    }

    /// Build MCP tool definitions.
    pub fn build_mcp(&self, _profile: Option<&str>) -> Value {
        let tools: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                let input_schema = Self::params_to_json_schema(&c.slash.params);
                serde_json::json!({
                    "name": c.id.replace('.', "_"),
                    "description": c.slash.description,
                    "inputSchema": input_schema,
                })
            })
            .collect();

        serde_json::json!({
            "tools": tools,
        })
    }

    /// Convert parameters to JSON Schema.
    fn params_to_json_schema(params: &[ParamSpec]) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for param in params {
            if param.hidden {
                continue;
            }

            let schema = Self::param_type_to_json_schema(&param.param_type);
            let mut prop = serde_json::json!({
                "type": schema["type"],
                "description": param.description,
            });

            if let Some(default) = &param.default {
                prop["default"] = default.clone();
            }

            if let Some(schema_obj) = schema.as_object() {
                for (k, v) in schema_obj {
                    if k != "type" {
                        prop[k] = v.clone();
                    }
                }
            }

            properties.insert(param.name.clone(), prop);

            if param.required {
                required.push(param.name.clone());
            }
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    /// Convert ParamType to JSON Schema.
    fn param_type_to_json_schema(param_type: &ParamType) -> Value {
        match param_type {
            ParamType::String {
                pattern,
                completions,
            } => {
                let mut schema = serde_json::json!({ "type": "string" });
                if let Some(pat) = pattern {
                    schema["pattern"] = Value::String(pat.clone());
                }
                if let Some(comps) = completions {
                    schema["enum"] =
                        Value::Array(comps.iter().map(|c| Value::String(c.clone())).collect());
                }
                schema
            }
            ParamType::Integer { min, max } => {
                let mut schema = serde_json::json!({ "type": "integer" });
                if let Some(min) = min {
                    schema["minimum"] = Value::Number((*min).into());
                }
                if let Some(max) = max {
                    schema["maximum"] = Value::Number((*max).into());
                }
                schema
            }
            ParamType::Float { min, max } => {
                let mut schema = serde_json::json!({ "type": "number" });
                if let Some(min) = min {
                    schema["minimum"] = Value::Number(serde_json::Number::from_f64(*min).unwrap());
                }
                if let Some(max) = max {
                    schema["maximum"] = Value::Number(serde_json::Number::from_f64(*max).unwrap());
                }
                schema
            }
            ParamType::Boolean => serde_json::json!({ "type": "boolean" }),
            ParamType::Enum { choices } => {
                serde_json::json!({
                    "type": "string",
                    "enum": choices,
                })
            }
            ParamType::Array {
                items,
                min_items,
                max_items,
            } => {
                let mut schema = serde_json::json!({
                    "type": "array",
                    "items": Self::param_type_to_json_schema(items),
                });
                if let Some(min) = min_items {
                    schema["minItems"] = Value::Number((*min).into());
                }
                if let Some(max) = max_items {
                    schema["maxItems"] = Value::Number((*max).into());
                }
                schema
            }
            ParamType::Path { .. } => serde_json::json!({ "type": "string", "format": "path" }),
            ParamType::Json => serde_json::json!({ "type": "object" }),
            ParamType::Duration => serde_json::json!({ "type": "string", "format": "duration" }),
            ParamType::Timestamp => serde_json::json!({ "type": "string", "format": "date-time" }),
        }
    }

    /// Build documentation (markdown).
    pub fn build_docs(&self) -> String {
        let mut md = String::new();
        md.push_str("# Memnoth CLI Command Reference\n\n");
        md.push_str(&format!("Version: {}\n\n", env!("CARGO_PKG_VERSION")));
        md.push_str("---\n\n");

        for category in self.categories() {
            let commands = self.by_category(category);
            if commands.is_empty() {
                continue;
            }

            md.push_str(&format!("## {}\n\n", category.to_uppercase()));

            for cmd in commands {
                md.push_str(&format!("### `memnoth {}`\n\n", cmd.cli_command()));
                md.push_str(&format!("{}\n\n", cmd.slash.description));

                if let Some(long) = &cmd.slash.long_description {
                    md.push_str(&format!("{}\n\n", long));
                }

                if !cmd.slash.aliases.is_empty() {
                    md.push_str(&format!(
                        "**Aliases:** `{}`\n\n",
                        cmd.slash.aliases.join("`, `")
                    ));
                }

                if !cmd.slash.examples.is_empty() {
                    md.push_str("**Examples:**\n\n");
                    for ex in &cmd.slash.examples {
                        md.push_str(&format!("```bash\nmemnoth {}\n```\n\n", ex));
                    }
                }

                let required_params: Vec<_> = cmd
                    .slash
                    .params
                    .iter()
                    .filter(|p| p.required && !p.hidden)
                    .collect();

                let optional_params: Vec<_> = cmd
                    .slash
                    .params
                    .iter()
                    .filter(|p| !p.required && !p.hidden)
                    .collect();

                if !required_params.is_empty() {
                    md.push_str("**Required arguments:**\n\n");
                    for param in required_params {
                        md.push_str(&format!(
                            "- `--{}` ({}) — {}\n",
                            param.long.as_ref().unwrap_or(&param.name),
                            Self::param_type_short(&param.param_type),
                            param.description
                        ));
                    }
                    md.push_str("\n");
                }

                if !optional_params.is_empty() {
                    md.push_str("**Optional arguments:**\n\n");
                    for param in optional_params {
                        let default = param
                            .default
                            .as_ref()
                            .map(|d| format!(" (default: {})", d))
                            .unwrap_or_default();
                        md.push_str(&format!(
                            "- `--{}` ({}) — {}{}\n",
                            param.long.as_ref().unwrap_or(&param.name),
                            Self::param_type_short(&param.param_type),
                            param.description,
                            default
                        ));
                    }
                    md.push_str("\n");
                }

                md.push_str(&format!(
                    "**Overlay:** `{:?}` | **Danger:** `{:?}` | **Auth:** `{:?}`\n\n",
                    cmd.slash.overlay, cmd.slash.danger, cmd.slash.auth
                ));

                md.push_str("---\n\n");
            }
        }

        md
    }

    fn param_type_short(param_type: &ParamType) -> &'static str {
        match param_type {
            ParamType::String { .. } => "string",
            ParamType::Integer { .. } => "integer",
            ParamType::Float { .. } => "float",
            ParamType::Boolean => "bool",
            ParamType::Enum { .. } => "enum",
            ParamType::Array { .. } => "array",
            ParamType::Path { .. } => "path",
            ParamType::Json => "json",
            ParamType::Duration => "duration",
            ParamType::Timestamp => "timestamp",
        }
    }

    /// Build shell completions.
    pub fn build_completions(&self, shell: clap_complete::Shell) -> String {
        let mut cmd = self.build_clap();
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut cmd, "memnoth", &mut buf);
        String::from_utf8(buf).unwrap_or_default()
    }

    /// Build JSON schema for all commands.
    pub fn build_json_schema(&self) -> Value {
        let commands: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "cli_path": c.cli_path,
                    "name": c.name(),
                    "description": c.slash.description,
                    "handler": c.handler,
                    "json_output": c.json_output,
                    "streaming": c.streaming,
                    "timeout_secs": c.timeout_secs,
                    "min_version": c.min_version,
                    "tags": c.tags,
                    "related": c.related,
                    "params": c.slash.params,
                    "overlay": c.slash.overlay,
                    "danger": c.slash.danger,
                    "auth": c.slash.auth,
                    "profile": c.slash.profile,
                    "categories": c.slash.categories,
                    "aliases": c.slash.aliases,
                    "examples": c.slash.examples,
                })
            })
            .collect();

        serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Memnoth Command Schema",
            "version": env!("CARGO_PKG_VERSION"),
            "type": "object",
            "properties": {
                "commands": {
                    "type": "array",
                    "items": {
                        "type": "object",
                    },
                },
            },
            "commands": commands,
        })
    }
}

/// Builder for CommandRegistry with fluent API.
pub struct RegistryBuilder {
    registry: CommandRegistry,
}

impl RegistryBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            registry: CommandRegistry::new(),
        }
    }

    /// Add a command spec.
    pub fn command(mut self, spec: CommandSpec) -> Result<Self> {
        self.registry.register(spec)?;
        Ok(self)
    }

    /// Add multiple command specs.
    pub fn commands(mut self, specs: Vec<CommandSpec>) -> Result<Self> {
        for spec in specs {
            self.registry.register(spec)?;
        }
        Ok(self)
    }

    /// Build the registry.
    pub fn build(self) -> CommandRegistry {
        self.registry
    }
}

impl Default for RegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of parity check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityReport {
    /// Whether all surfaces match.
    pub ok: bool,
    /// Mismatches found.
    pub mismatches: Vec<ParityMismatch>,
}

/// A single parity mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityMismatch {
    /// Command ID.
    pub command_id: String,
    /// Surface that differs.
    pub surface: String,
    /// Expected value.
    pub expected: Value,
    /// Actual value.
    pub actual: Value,
    /// Description of the mismatch.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{AuthLevel, DangerLevel, OverlayType, SlashSpec};

    #[test]
    fn test_registry_basic() {
        let mut registry = CommandRegistry::new();
        assert!(registry.is_empty());

        let spec = CommandSpec {
            id: "test.cmd".into(),
            cli_path: vec!["test".into(), "cmd".into()],
            slash: SlashSpec {
                name: "cmd".into(),
                description: "Test command".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["test".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![],
            },
            handler: "test_cmd".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec![],
            related: vec![],
        };

        registry.register(spec).unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.get("test.cmd").is_some());
    }
}

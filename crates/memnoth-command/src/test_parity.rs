//! Parity test asserting clap == palette == slash == mcp == docs.

use crate::registry::CommandRegistry;
use crate::spec::CommandSpec;
use anyhow::Result;
use serde_json::Value;

/// Assert parity across all built surfaces.
///
/// This test verifies that a single CommandSpec produces identical
/// semantic output across all surfaces (CLI, palette, slash, MCP, docs).
pub fn assert_parity(registry: &CommandRegistry) -> Result<()> {
    let mut mismatches = Vec::new();

    // Build all surfaces
    let clap_cmd = registry.build_clap();
    let palette = registry.build_palette();
    let slash = registry.build_slash();
    let mcp = registry.build_mcp(None);
    let docs = registry.build_docs();
    let json_schema = registry.build_json_schema();

    // The clap tree is the CLI's own parse tree, so it has to be read, not just
    // built: a spec with a `cli_path` whose first word is absent there is a
    // command that is in the palette and cannot be typed. That is exactly the
    // drift this test exists for, and it was the one surface left unread.
    let typed: Vec<String> = clap_cmd
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();
    for spec in registry.all() {
        let Some(first) = spec.cli_path.first() else {
            continue;
        };
        if !typed.iter().any(|name| name == first) {
            mismatches.push(ParityMismatch {
                command_id: spec.id.clone(),
                surface: "clap".into(),
                expected: Value::String(first.clone()),
                actual: Value::Null,
                description: "Command missing from the CLI parse tree".into(),
            });
        }
    }

    // Verify each command appears in all surfaces
    for spec in registry.all() {
        if spec.slash.hidden {
            continue;
        }

        // Check palette
        if let Some(cmds) = palette.get("commands").and_then(|v| v.as_array()) {
            let found = cmds
                .iter()
                .any(|c| c.get("id").and_then(|v| v.as_str()) == Some(&spec.id));
            if !found {
                mismatches.push(ParityMismatch {
                    command_id: spec.id.clone(),
                    surface: "palette".into(),
                    expected: Value::Bool(true),
                    actual: Value::Bool(false),
                    description: "Command missing from palette".into(),
                });
            }
        }

        // Check slash
        if let Some(cmds) = slash.get("commands").and_then(|v| v.as_array()) {
            let found = cmds
                .iter()
                .any(|c| c.get("name").and_then(|v| v.as_str()) == Some(&spec.slash.name));
            if !found {
                mismatches.push(ParityMismatch {
                    command_id: spec.id.clone(),
                    surface: "slash".into(),
                    expected: Value::Bool(true),
                    actual: Value::Bool(false),
                    description: "Command missing from slash".into(),
                });
            }
        }

        // Check MCP (if not profile-gated)
        if spec.slash.profile.is_none()
            && let Some(tools) = mcp.get("tools").and_then(|v| v.as_array())
        {
            let mcp_name = spec.id.replace('.', "_");
            let found = tools
                .iter()
                .any(|t| t.get("name").and_then(|v| v.as_str()) == Some(&mcp_name));
            if !found {
                mismatches.push(ParityMismatch {
                    command_id: spec.id.clone(),
                    surface: "mcp".into(),
                    expected: Value::Bool(true),
                    actual: Value::Bool(false),
                    description: "Command missing from MCP tools".into(),
                });
            }
        }

        // Check docs contains command
        let cli_cmd = spec.cli_command();
        if !docs.contains(&cli_cmd) {
            mismatches.push(ParityMismatch {
                command_id: spec.id.clone(),
                surface: "docs".into(),
                expected: Value::Bool(true),
                actual: Value::Bool(false),
                description: "Command missing from docs".into(),
            });
        }

        // Check JSON schema
        if let Some(cmds) = json_schema.get("commands").and_then(|v| v.as_array()) {
            let found = cmds
                .iter()
                .any(|c| c.get("id").and_then(|v| v.as_str()) == Some(&spec.id));
            if !found {
                mismatches.push(ParityMismatch {
                    command_id: spec.id.clone(),
                    surface: "json_schema".into(),
                    expected: Value::Bool(true),
                    actual: Value::Bool(false),
                    description: "Command missing from JSON schema".into(),
                });
            }
        }

        // Verify parameter parity across surfaces
        check_param_parity(spec, &palette, &slash, &mcp, &json_schema, &mut mismatches);
    }

    if !mismatches.is_empty() {
        let report = ParityReport {
            ok: false,
            mismatches,
        };
        let json = serde_json::to_string_pretty(&report)?;
        anyhow::bail!("Parity check failed:\n{}", json);
    }

    Ok(())
}

/// Check parameter parity across surfaces.
fn check_param_parity(
    spec: &CommandSpec,
    palette: &Value,
    slash: &Value,
    mcp: &Value,
    json_schema: &Value,
    mismatches: &mut Vec<ParityMismatch>,
) {
    let param_names: Vec<&str> = spec.slash.params.iter().map(|p| p.name.as_str()).collect();

    // Check palette params
    if let Some(cmds) = palette.get("commands").and_then(|v| v.as_array())
        && let Some(cmd) = cmds
            .iter()
            .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(&spec.id))
        && let Some(params) = cmd.get("params").and_then(|v| v.as_array())
    {
        let palette_names: Vec<&str> = params
            .iter()
            .filter_map(|p| p.get("name").and_then(|v| v.as_str()))
            .collect();
        if palette_names != param_names {
            mismatches.push(ParityMismatch {
                command_id: spec.id.clone(),
                surface: "palette".into(),
                expected: Value::Array(
                    param_names
                        .iter()
                        .map(|n| Value::String(n.to_string()))
                        .collect(),
                ),
                actual: Value::Array(
                    palette_names
                        .iter()
                        .map(|n| Value::String(n.to_string()))
                        .collect(),
                ),
                description: "Parameter names differ from spec".into(),
            });
        }
    }

    // Check slash params
    if let Some(cmds) = slash.get("commands").and_then(|v| v.as_array())
        && let Some(cmd) = cmds
            .iter()
            .find(|c| c.get("name").and_then(|v| v.as_str()) == Some(&spec.slash.name))
        && let Some(params) = cmd.get("params").and_then(|v| v.as_array())
    {
        let slash_names: Vec<&str> = params
            .iter()
            .filter_map(|p| p.get("name").and_then(|v| v.as_str()))
            .collect();
        if slash_names != param_names {
            mismatches.push(ParityMismatch {
                command_id: spec.id.clone(),
                surface: "slash".into(),
                expected: Value::Array(
                    param_names
                        .iter()
                        .map(|n| Value::String(n.to_string()))
                        .collect(),
                ),
                actual: Value::Array(
                    slash_names
                        .iter()
                        .map(|n| Value::String(n.to_string()))
                        .collect(),
                ),
                description: "Parameter names differ from spec".into(),
            });
        }
    }

    // Check MCP inputSchema params
    if spec.slash.profile.is_none()
        && let Some(tools) = mcp.get("tools").and_then(|v| v.as_array())
    {
        let mcp_name = spec.id.replace('.', "_");
        if let Some(tool) = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(&mcp_name))
            && let Some(schema) = tool.get("inputSchema")
            && let Some(props) = schema.get("properties").and_then(|v| v.as_object())
        {
            // JSON Schema `properties` is an unordered object and
            // serde_json sorts it, so the contract is the same set
            // of names, not the declaration order.
            let mut mcp_names: Vec<&str> = props.keys().map(|k| k.as_str()).collect();
            let mut expected_names = param_names.clone();
            mcp_names.sort_unstable();
            expected_names.sort_unstable();
            if mcp_names != expected_names {
                mismatches.push(ParityMismatch {
                    command_id: spec.id.clone(),
                    surface: "mcp".into(),
                    expected: Value::Array(
                        expected_names
                            .iter()
                            .map(|n| Value::String(n.to_string()))
                            .collect(),
                    ),
                    actual: Value::Array(
                        mcp_names
                            .iter()
                            .map(|n| Value::String(n.to_string()))
                            .collect(),
                    ),
                    description: "MCP inputSchema parameter names differ from spec".into(),
                });
            }
        }
    }

    // Check JSON schema params
    if let Some(cmds) = json_schema.get("commands").and_then(|v| v.as_array())
        && let Some(cmd) = cmds
            .iter()
            .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(&spec.id))
        && let Some(params) = cmd.get("params").and_then(|v| v.as_array())
    {
        let schema_names: Vec<&str> = params
            .iter()
            .filter_map(|p| p.get("name").and_then(|v| v.as_str()))
            .collect();
        if schema_names != param_names {
            mismatches.push(ParityMismatch {
                command_id: spec.id.clone(),
                surface: "json_schema".into(),
                expected: Value::Array(
                    param_names
                        .iter()
                        .map(|n| Value::String(n.to_string()))
                        .collect(),
                ),
                actual: Value::Array(
                    schema_names
                        .iter()
                        .map(|n| Value::String(n.to_string()))
                        .collect(),
                ),
                description: "JSON schema parameter names differ from spec".into(),
            });
        }
    }
}

/// Parity check result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParityReport {
    /// Whether every compared surface agreed with the spec.
    pub ok: bool,
    /// Every disagreement found, in the order they were checked.
    pub mismatches: Vec<ParityMismatch>,
}

/// A single parity mismatch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParityMismatch {
    /// The command whose description or schema was compared.
    pub command_id: String,
    /// Which projection disagreed (`clap`, `json_schema`, ...).
    pub surface: String,
    /// What the spec says the surface should hold.
    pub expected: Value,
    /// What the surface actually holds.
    pub actual: Value,
    /// The difference, in words a reader can act on.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegistryBuilder;
    use crate::spec::{AuthLevel, CommandSpec, DangerLevel, OverlayType, ParamSpec, SlashSpec};

    fn make_test_registry() -> CommandRegistry {
        let spec = CommandSpec {
            id: "test.echo".into(),
            cli_path: vec!["test".into(), "echo".into()],
            slash: SlashSpec {
                name: "echo".into(),
                description: "Echo a message".into(),
                long_description: None,
                palette: None,
                params: vec![
                    ParamSpec::string("message", "Message to echo").positional(0),
                    ParamSpec::bool("uppercase", "Convert to uppercase").short('u'),
                    ParamSpec::integer("repeat", "Number of times to repeat").short('n'),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["test".into()],
                aliases: vec!["e".into()],
                hidden: false,
                deprecated: None,
                examples: vec!["test echo hello --uppercase".into()],
            },
            handler: "test_echo".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec![],
            related: vec![],
        };

        RegistryBuilder::new().command(spec).unwrap().build()
    }

    #[test]
    fn test_parity() {
        let registry = make_test_registry();
        assert_parity(&registry).unwrap();
    }
}

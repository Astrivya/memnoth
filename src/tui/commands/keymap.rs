//! `/keymap` — view, remap, persist; reject conflicts.
//!
//! The table is `tui::keymap::Keymap`, and the file is
//! `$MEMNOTH_HOME/keymap.toml`. A rebind is refused when the key already means
//! something else: silently stealing `ctrl+1` from the surface switch is the
//! kind of change a person discovers by losing a key they use.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::KeymapArgs;
use crate::context::AppContext;
use crate::output::{ErrorOutcome, Outcome};
use crate::tui::keymap::{KeyBinding, Keymap, persist};

/// `/keymap [set KEY=ACTION | unset KEY | reset]`.
pub fn run(ctx: &AppContext, args: &KeymapArgs) -> Result<Outcome, ErrorOutcome> {
    keymap(ctx, args).map_err(|error| ErrorOutcome::from_anyhow(&error))
}

fn keymap(ctx: &AppContext, args: &KeymapArgs) -> Result<Outcome> {
    let path = ctx.home.join("keymap.toml");
    let mut table = load(&path)?;
    let changed = apply(&mut table, args)?;

    if let Some(note) = changed {
        if args.reset && path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove {}", path.display()))?;
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, persist::to_string(&table))
                .with_context(|| format!("cannot write {}", path.display()))?;
        }
        return Ok(Outcome::new(
            "keymap",
            format!("{note}\n  keymap: {}", path.display()),
            json!({
                "verb": "keymap",
                "available": true,
                "changed": note,
                "path": path.display().to_string(),
                "bindings": table.global.len(),
            }),
        ));
    }

    let mut lines = vec![format!(
        "keymap — {} bindings ({})",
        table.global.len(),
        if path.is_file() {
            path.display().to_string()
        } else {
            "built-in table".to_string()
        }
    )];
    for binding in table.global.values() {
        lines.push(format!(
            "  {:<12} {} — {}",
            binding.key, binding.action, binding.description
        ));
    }
    lines.push("  /keymap set alt+d=surface: review · unset alt+d · reset".to_string());
    let bindings: Vec<serde_json::Value> = table
        .global
        .values()
        .map(|binding| {
            json!({
                "key": binding.key,
                "action": binding.action,
                "description": binding.description,
            })
        })
        .collect();

    Ok(Outcome::new(
        "keymap",
        lines.join("\n"),
        json!({
            "verb": "keymap",
            "available": true,
            "path": path.display().to_string(),
            "bindings": bindings,
        }),
    ))
}

fn load(path: &Path) -> Result<Keymap> {
    if !path.is_file() {
        return Ok(Keymap::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    persist::from_string(&text).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Apply one mutation, returning what to tell the person, or `None` to list.
///
/// Split out of [`run`] so the conflict rules can be tested without a context.
pub(crate) fn apply(table: &mut Keymap, args: &KeymapArgs) -> Result<Option<String>> {
    if args.reset {
        *table = Keymap::new();
        return Ok(Some("restored the built-in keymap".to_string()));
    }

    if let Some(spec) = &args.set {
        let (key, action) = spec.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("--set takes KEY=ACTION, e.g. --set alt+d=surface: review")
        })?;
        let key = key.trim();
        let action = action.trim();
        if key.is_empty() || action.is_empty() {
            bail!("--set needs both a key and an action");
        }
        if let Some(existing) = table.global.get(key)
            && existing.action != action
        {
            bail!(
                "key conflict: {key} is already bound to {} — --unset {key} first, or choose another key",
                existing.action
            );
        }
        table.rebind(KeyBinding {
            key: key.to_string(),
            action: action.to_string(),
            description: "bound by /keymap".to_string(),
        });
        return Ok(Some(format!("bound {key} → {action}")));
    }

    if let Some(key) = &args.unset {
        let key = key.trim();
        if table.global.remove(key).is_none() {
            bail!("{key} is not bound");
        }
        return Ok(Some(format!("unbound {key}")));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(set: Option<&str>, unset: Option<&str>, reset: bool) -> KeymapArgs {
        KeymapArgs {
            set: set.map(str::to_string),
            unset: unset.map(str::to_string),
            reset,
        }
    }

    #[test]
    fn a_new_key_is_bound_and_persists_through_toml() {
        let mut table = Keymap::new();
        let note = apply(
            &mut table,
            &args(Some("alt+d=surface: review"), None, false),
        )
        .unwrap()
        .unwrap();
        assert!(note.contains("alt+d"), "{note}");

        let saved = persist::to_string(&table);
        let loaded = persist::from_string(&saved).unwrap();
        assert_eq!(
            loaded.lookup("alt+d", None).map(|b| b.action.as_str()),
            Some("surface: review")
        );
    }

    #[test]
    fn stealing_a_bound_key_is_refused_by_name() {
        let mut table = Keymap::new();
        let error = apply(
            &mut table,
            &args(Some("ctrl+1=surface: review"), None, false),
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("key conflict"), "{text}");
        assert!(text.contains("surface: chat"), "{text}");
    }

    #[test]
    fn rebinding_a_key_to_the_same_action_is_not_a_conflict() {
        let mut table = Keymap::new();
        apply(&mut table, &args(Some("ctrl+1=surface: chat"), None, false)).unwrap();
    }

    #[test]
    fn unset_refuses_a_key_that_is_not_bound() {
        let mut table = Keymap::new();
        let error = apply(&mut table, &args(None, Some("alt+zz"), false)).unwrap_err();
        assert!(error.to_string().contains("not bound"), "{error}");
    }

    #[test]
    fn reset_restores_the_built_in_table() {
        let mut table = Keymap::new();
        apply(
            &mut table,
            &args(Some("alt+d=surface: review"), None, false),
        )
        .unwrap();
        apply(&mut table, &args(None, None, true)).unwrap();
        assert!(table.lookup("alt+d", None).is_none());
        assert!(table.lookup("ctrl+1", None).is_some());
    }

    #[test]
    fn a_spec_without_an_action_is_a_usage_error() {
        let mut table = Keymap::new();
        let error = apply(&mut table, &args(Some("alt+d"), None, false)).unwrap_err();
        assert!(error.to_string().contains("KEY=ACTION"), "{error}");
    }
}

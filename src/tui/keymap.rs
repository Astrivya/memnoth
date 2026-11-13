//! Keymap: one table, conflict detection, persistence.
//!
//! Every key binding is a projection of a command, not a second command.
//! Conflicts are errors, not help typos.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One key or chord binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    /// The key/chord, e.g. `alt+a`, `ctrl+x i`, `esc`, `tab`.
    pub key: String,
    /// The command or action it triggers.
    pub action: String,
    /// Human-readable description.
    pub description: String,
}

/// The entire keymap: one table. Surface-specific overlays layer on top.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Keymap {
    /// Global bindings active on every surface.
    pub global: BTreeMap<String, KeyBinding>,
    /// Surface-scoped bindings (only active when that surface is focused).
    pub surface: BTreeMap<String, KeyBinding>,
}

impl Keymap {
    pub fn new() -> Self {
        let mut global = BTreeMap::new();

        global.insert(
            "ctrl+1".to_string(),
            KeyBinding {
                key: "ctrl+1".into(),
                action: "surface: chat".into(),
                description: "switch to chat surface".into(),
            },
        );
        global.insert(
            "ctrl+2".to_string(),
            KeyBinding {
                key: "ctrl+2".into(),
                action: "surface: review".into(),
                description: "switch to review surface".into(),
            },
        );
        global.insert(
            "ctrl+3".to_string(),
            KeyBinding {
                key: "ctrl+3".into(),
                action: "surface: agents".into(),
                description: "switch to agents surface".into(),
            },
        );
        global.insert(
            "ctrl+4".to_string(),
            KeyBinding {
                key: "ctrl+4".into(),
                action: "surface: memory".into(),
                description: "switch to memory surface".into(),
            },
        );
        global.insert(
            "ctrl+5".to_string(),
            KeyBinding {
                key: "ctrl+5".into(),
                action: "surface: context".into(),
                description: "switch to context surface".into(),
            },
        );
        global.insert(
            "ctrl+6".to_string(),
            KeyBinding {
                key: "ctrl+6".into(),
                action: "surface: tasks".into(),
                description: "switch to tasks surface".into(),
            },
        );
        global.insert(
            "ctrl+7".to_string(),
            KeyBinding {
                key: "ctrl+7".into(),
                action: "surface: sessions".into(),
                description: "switch to sessions surface".into(),
            },
        );
        global.insert(
            "ctrl+8".to_string(),
            KeyBinding {
                key: "ctrl+8".into(),
                action: "surface: brief".into(),
                description: "switch to brief surface".into(),
            },
        );
        global.insert(
            "alt+a".to_string(),
            KeyBinding {
                key: "alt+a".into(),
                action: "open: agent deck".into(),
                description: "open the live agent deck".into(),
            },
        );
        global.insert(
            "alt+t".to_string(),
            KeyBinding {
                key: "alt+t".into(),
                action: "open: trace".into(),
                description: "open the agent trace tree".into(),
            },
        );
        global.insert(
            "enter".to_string(),
            KeyBinding {
                key: "enter".into(),
                action: "act on focus".into(),
                description: "execute the focused command".into(),
            },
        );
        global.insert(
            "esc".to_string(),
            KeyBinding {
                key: "esc".into(),
                action: "pop one layer".into(),
                description: "close overlay, return to parent".into(),
            },
        );
        global.insert(
            "tab".to_string(),
            KeyBinding {
                key: "tab".into(),
                action: "complete or cycle".into(),
                description: "complete argument or cycle focus".into(),
            },
        );
        global.insert(
            "space".to_string(),
            KeyBinding {
                key: "space".into(),
                action: "peek".into(),
                description: "read-only preview of focused item".into(),
            },
        );
        global.insert(
            "ctrl+x i".to_string(),
            KeyBinding {
                key: "ctrl+x i".into(),
                action: "interrupt child".into(),
                description: "interrupt the focused child agent".into(),
            },
        );
        global.insert(
            "x".to_string(),
            KeyBinding {
                key: "x".into(),
                action: "stop child".into(),
                description: "stop the focused child agent".into(),
            },
        );
        global.insert(
            "r".to_string(),
            KeyBinding {
                key: "r".into(),
                action: "revive child".into(),
                description: "revive a stopped child agent".into(),
            },
        );
        global.insert(
            "ctrl+up".to_string(),
            KeyBinding {
                key: "ctrl+up".into(),
                action: "focus parent".into(),
                description: "return focus to the parent session".into(),
            },
        );

        Self {
            global,
            surface: BTreeMap::new(),
        }
    }

    /// Register a binding, returning an error if the key is already bound.
    pub fn insert(&mut self, binding: KeyBinding) -> Result<(), String> {
        if self.global.contains_key(&binding.key) {
            return Err(format!(
                "key conflict: {} is already bound to {}",
                binding.key, self.global[&binding.key].action
            ));
        }
        self.global.insert(binding.key.clone(), binding);
        Ok(())
    }

    /// Replace (or add) a binding.
    ///
    /// This is the explicit override path: unlike [`Keymap::insert`] the caller
    /// has already decided why the old binding goes, so a replacement is not a
    /// conflict. `/keymap set` refuses a *different* meaning for a bound key
    /// before it reaches here.
    pub fn rebind(&mut self, binding: KeyBinding) {
        self.global.insert(binding.key.clone(), binding);
    }

    /// Look up the action for a key, preferring surface-scoped over global.
    pub fn lookup(&self, key: &str, surface: Option<&str>) -> Option<&KeyBinding> {
        if let Some(s) = surface
            .and_then(|s| self.surface.get(s))
            .and_then(|_| self.surface.get(key))
        {
            return Some(s);
        }
        self.global.get(key)
    }
}

/// Persist/load a keymap to/from TOML text.
pub mod persist {
    use super::*;

    /// Serialize the keymap to TOML text.
    pub fn to_string(keymap: &Keymap) -> String {
        // Minimal TOML without a toml dependency: hand-rolled, one binding per line.
        let mut out = String::from("# Memnoth keymap — one table, conflicts are errors\n");
        for binding in keymap.global.values() {
            out.push_str("\n[[keybindings]]\n");
            out.push_str(&format!(
                "key = {}\n",
                serde_json::to_string(&binding.key).unwrap()
            ));
            out.push_str(&format!(
                "action = {}\n",
                serde_json::to_string(&binding.action).unwrap()
            ));
            out.push_str(&format!(
                "description = {}\n",
                serde_json::to_string(&binding.description).unwrap()
            ));
        }
        out
    }

    /// Deserialize the keymap from TOML text.
    pub fn from_string(text: &str) -> Result<Keymap, String> {
        // Minimal parse: enough to round-trip the hand-rolled format. The table
        // starts empty so `insert` checks for duplicates *within the file*;
        // starting from `Keymap::new()` made every built-in key a "duplicate"
        // and every saved keymap unreadable.
        let mut keymap = Keymap {
            global: BTreeMap::new(),
            surface: BTreeMap::new(),
        };
        let mut current_key: Option<String> = None;
        let mut current_action: Option<String> = None;
        let mut current_desc: Option<String> = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("key = ") {
                current_key = Some(rest.trim_matches('"').to_string());
            } else if let Some(rest) = line.strip_prefix("action = ") {
                current_action = Some(rest.trim_matches('"').to_string());
            } else if let Some(rest) = line.strip_prefix("description = ") {
                current_desc = Some(rest.trim_matches('"').to_string());
            } else if line == "[[keybindings]]"
                && let (Some(k), Some(a), Some(d)) = (
                    current_key.take(),
                    current_action.take(),
                    current_desc.take(),
                )
            {
                let binding = KeyBinding {
                    key: k.clone(),
                    action: a,
                    description: d,
                };
                if keymap.insert(binding).is_err() {
                    return Err(format!("duplicate key in keymap: {k}"));
                }
            }
        }
        // The format puts the header *before* each entry, so the last entry has
        // no following header to flush it. Without this the final binding of
        // every saved keymap was silently dropped on load.
        if let (Some(k), Some(a), Some(d)) = (
            current_key.take(),
            current_action.take(),
            current_desc.take(),
        ) {
            let binding = KeyBinding {
                key: k.clone(),
                action: a,
                description: d,
            };
            if keymap.insert(binding).is_err() {
                return Err(format!("duplicate key in keymap: {k}"));
            }
        }
        Ok(keymap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_keymap_reads_back_with_its_override() {
        let mut keymap = Keymap::new();
        keymap.rebind(KeyBinding {
            key: "alt+d".into(),
            action: "surface: review".into(),
            description: "bound by /keymap".into(),
        });
        let text = persist::to_string(&keymap);
        let loaded = persist::from_string(&text).expect("a saved keymap must load");
        assert_eq!(loaded.global.len(), keymap.global.len());
        assert_eq!(
            loaded.lookup("alt+d", None).map(|b| b.action.as_str()),
            Some("surface: review")
        );
        assert!(loaded.lookup("ctrl+1", None).is_some());
    }

    #[test]
    fn a_duplicate_key_inside_the_file_is_refused() {
        let text = "[[keybindings]]\nkey = \"alt+d\"\naction = \"a\"\ndescription = \"x\"\n\n[[keybindings]]\nkey = \"alt+d\"\naction = \"b\"\ndescription = \"y\"\n";
        let error = persist::from_string(text).unwrap_err();
        assert!(error.contains("duplicate key"), "{error}");
    }

    #[test]
    fn rebind_replaces_a_built_in_by_design() {
        let mut keymap = Keymap::new();
        keymap.rebind(KeyBinding {
            key: "ctrl+1".into(),
            action: "surface: memory".into(),
            description: "rebound".into(),
        });
        assert_eq!(
            keymap.lookup("ctrl+1", None).map(|b| b.action.as_str()),
            Some("surface: memory")
        );
    }
}

//! Precision provenance: how the edges behind a graph answer were derived.
//!
//! Every answer in this crate states one of three tiers, on the wire and in the
//! TUI. The tier is a fact about the code that produced the edges, never an
//! estimate of quality:
//!
//! - [`Precision::Lsp`] — a language server resolved the definition and the
//!   references.
//! - [`Precision::TreeSitter`] — a parse gave scopes and call shapes without
//!   type resolution.
//! - [`Precision::Lexical`] — the zero-dependency scanner matched names and
//!   same-file order.
//!
//! The lexical tier is always available and is the fallback (W7-02). A build
//! that cannot reach a higher tier says so instead of returning an unlabelled
//! approximation: [`capabilities`] is the probe, [`best_available`] is what a
//! caller should label the answer with before it knows the edges.
//!
//! The wire labels are stable — `"lsp"`, `"tree-sitter"`, `"lexical"` — so two
//! answers produced by different builds can be compared without either side
//! knowing which build the other is.

use serde::{Deserialize, Serialize};

/// How the edges behind an answer were produced, weakest first.
///
/// Ordering is deliberate: [`Ord`] compares by confidence, so a mixed answer
/// takes the weakest tier it used (`a.min(b)`) and never claims more than its
/// least-resolved edge.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    /// Regex/lexical scanner: names, not resolved types.
    ///
    /// This is the default because it is the only tier that cannot be absent:
    /// the crate carries no parser and no language server, so the fallback is
    /// the honest answer rather than an empty one.
    #[default]
    Lexical,
    /// Tree-sitter parse: scopes and call shapes, still not type resolution.
    #[serde(rename = "tree-sitter")]
    TreeSitter,
    /// Language server: resolved definitions and references.
    #[serde(rename = "lsp")]
    Lsp,
}

/// Factor applied to an edge that is a name match rather than a resolution.
///
/// A `likely` edge keeps the class in the payload and halves its contribution to
/// the path confidence, so a chain of name matches cannot add up to the
/// confidence of one resolved call.
pub const LIKELY_EDGE_FACTOR: f64 = 0.5;

impl Precision {
    /// Stable label for JSON, the TUI, and MCP payloads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::TreeSitter => "tree-sitter",
            Self::Lsp => "lsp",
        }
    }

    /// Parse a label back, accepting both spellings of the tree-sitter tier.
    pub fn parse(label: &str) -> Option<Self> {
        match label.trim().to_ascii_lowercase().as_str() {
            "lexical" => Some(Self::Lexical),
            "tree-sitter" | "tree_sitter" | "treesitter" => Some(Self::TreeSitter),
            "lsp" => Some(Self::Lsp),
            _ => None,
        }
    }

    /// Base confidence of one edge at this tier, in `[0, 1]`.
    ///
    /// The lexical scanner resolves a same-file/known-name mention, which is
    /// right most of the time but carries no type information; the number
    /// encodes that, and every path multiplies it per hop.
    pub fn edge_confidence(self) -> f64 {
        match self {
            Self::Lexical => 0.90,
            Self::TreeSitter => 0.85,
            Self::Lsp => 0.99,
        }
    }

    /// Confidence of one edge, with a name-only match discounted.
    pub fn edge_confidence_for(self, resolved: bool) -> f64 {
        let base = self.edge_confidence();
        if resolved {
            base
        } else {
            base * LIKELY_EDGE_FACTOR
        }
    }

    /// The weaker of two tiers, used when an answer mixes sources.
    pub fn weakest(self, other: Self) -> Self {
        self.min(other)
    }

    /// The human sentence that goes beside a precision label.
    pub fn explain(self) -> &'static str {
        match self {
            Self::Lexical => "name matching only; no resolved types",
            Self::TreeSitter => "parsed scopes; calls not type-resolved",
            Self::Lsp => "definitions and references resolved by a language server",
        }
    }
}

/// Which tiers this build can actually produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// The zero-dependency scanner is compiled in.
    pub lexical: bool,
    /// A tree-sitter parser is compiled in.
    pub tree_sitter: bool,
    /// A language-server client is compiled in.
    pub lsp: bool,
}

impl Capabilities {
    /// Whether this build can produce edges at `precision`.
    pub fn supports(self, precision: Precision) -> bool {
        match precision {
            Precision::Lexical => self.lexical,
            Precision::TreeSitter => self.tree_sitter,
            Precision::Lsp => self.lsp,
        }
    }

    /// The strongest tier this build can produce.
    ///
    /// Falls back to [`Precision::Lexical`] even if the flag were wrong: an
    /// answer must carry a tier, and the scanner is the one that always exists.
    pub fn best(self) -> Precision {
        if self.lsp {
            Precision::Lsp
        } else if self.tree_sitter {
            Precision::TreeSitter
        } else {
            Precision::Lexical
        }
    }
}

/// Probe the tiers compiled into this build.
///
/// Tree-sitter parsing is compiled in for the top eight languages
/// (rust, python, typescript/javascript, go, java, cpp, ruby); a language
/// server is not, so the strongest tier this build can reach is tree-sitter,
/// with lexical always available as the zero-dependency fallback.
pub fn capabilities() -> Capabilities {
    Capabilities {
        lexical: true,
        tree_sitter: true,
        lsp: false,
    }
}

/// Whether a language-server-backed analysis is available in this build.
pub fn lsp_available() -> bool {
    capabilities().lsp
}

/// Whether a tree-sitter-backed analysis is available in this build.
pub fn tree_sitter_available() -> bool {
    capabilities().tree_sitter
}

/// The tier a caller should label an answer with before the edges are known.
pub fn best_available() -> Precision {
    capabilities().best()
}

/// Anything that answers a graph question and must state its evidence.
///
/// One method, so the CLI, the TUI, and the MCP surface add the label in one
/// place instead of each inventing a field name.
pub trait GraphAnswer {
    /// How the edges in this answer were produced.
    fn precision(&self) -> Precision;

    /// The stable wire label, ready for a JSON object.
    fn precision_label(&self) -> &'static str {
        self.precision().label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_labels_match_the_contract() {
        assert_eq!(Precision::Lexical.label(), "lexical");
        assert_eq!(Precision::TreeSitter.label(), "tree-sitter");
        assert_eq!(Precision::Lsp.label(), "lsp");
        // The hyphen matters: `tree_sitter` is a different claim than
        // `tree-sitter`, and a consumer filtering on it would miss the row.
        assert_eq!(
            serde_json::to_string(&Precision::TreeSitter).unwrap(),
            "\"tree-sitter\""
        );
        assert_eq!(serde_json::to_string(&Precision::Lsp).unwrap(), "\"lsp\"");
        assert_eq!(Precision::parse("tree_sitter"), Some(Precision::TreeSitter));
        assert_eq!(Precision::parse("nonsense"), None);
    }

    #[test]
    fn ordering_takes_the_weakest_tier() {
        assert!(Precision::Lexical < Precision::TreeSitter);
        assert!(Precision::TreeSitter < Precision::Lsp);
        assert_eq!(
            Precision::Lsp.weakest(Precision::Lexical),
            Precision::Lexical
        );
        assert_eq!(
            Precision::Lsp.weakest(Precision::TreeSitter),
            Precision::TreeSitter
        );
    }

    #[test]
    fn this_build_labels_tree_sitter_and_says_so() {
        // This build compiles the tree-sitter parsers in (see the crate
        // Cargo.toml), so its best tier is tree-sitter, not lexical.
        let caps = capabilities();
        assert!(caps.lexical);
        assert!(caps.tree_sitter);
        assert!(!caps.lsp);
        assert_eq!(best_available(), Precision::TreeSitter);
        assert_eq!(caps.best(), Precision::TreeSitter);
        assert!(!lsp_available());
        assert!(caps.supports(Precision::Lexical));
        assert!(caps.supports(Precision::TreeSitter));
        assert_eq!(Precision::default(), Precision::Lexical);
    }

    #[test]
    fn a_name_match_is_discounted_below_a_resolved_edge() {
        let resolved = Precision::Lexical.edge_confidence_for(true);
        let likely = Precision::Lexical.edge_confidence_for(false);
        assert!(likely < resolved, "{likely} !< {resolved}");
        assert!((likely - 0.45).abs() < f64::EPSILON);
    }
}

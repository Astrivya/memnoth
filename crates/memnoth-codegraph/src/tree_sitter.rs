//! Tree-sitter parsing for top eight languages (W7-08).
//!
//! This module compiles parsers for the languages in [`crate::lsp::TOP_EIGHT_LANGUAGES`]
//! and uses tree-sitter queries to extract symbols and references with
//! [`Precision::TreeSitter`] — stronger than lexical name matching, weaker
//! than a fully type-resolved language server. When tree-sitter is not
//! compiled in (or a language has no grammar), callers fall back to the
//! zero-dependency lexical scanner ([`crate::Precision::Lexical`]).
//!
//! The grammars are loaded once at startup from the `tree-sitter-*` crates'
//! embedded language pointers.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

use crate::lsp::TOP_EIGHT_LANGUAGES;
use crate::precision::Precision;

/// One parsed file: the tree plus the language it was parsed with.
#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub tree: Tree,
    pub language: String,
    pub source: Vec<u8>,
}

/// A symbol extracted by tree-sitter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeSymbol {
    pub name: String,
    pub kind: String,
    pub byte_range: (usize, usize),
    pub row_range: (usize, usize),
    pub precision: Precision,
}

/// A reference extracted by tree-sitter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeReference {
    pub name: String,
    pub byte_range: (usize, usize),
    pub row_range: (usize, usize),
    pub precision: Precision,
}

/// The tree-sitter query pool: one query set per language.
pub struct TreeSitter {
    queries: OnceLock<HashMap<String, HashMap<String, Query>>>,
}

/// Query keys stored per language.
const SYMBOL_QUERY: &str = "symbols";
const REFERENCE_QUERY: &str = "references";

impl TreeSitter {
    /// Build the query pool for the top eight languages.
    pub fn new() -> Result<Self> {
        Ok(Self {
            queries: OnceLock::new(),
        })
    }

    /// Parse `source` as `language`, returning the syntax tree.
    pub fn parse(&self, language: &str, source: &[u8]) -> Result<ParsedFile> {
        let lang_ptr = language_pointer(language)?;
        let mut parser = Parser::new();
        parser.set_language(&lang_ptr)?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("failed to parse {language}"))?;
        Ok(ParsedFile {
            tree,
            language: language.to_string(),
            source: source.to_vec(),
        })
    }

    /// Extract symbols from `source` using tree-sitter queries.
    pub fn symbols(&self, language: &str, source: &[u8]) -> Result<Vec<TreeSymbol>> {
        let parsed = self.parse(language, source)?;
        let query = self.query_for(language, SYMBOL_QUERY)?;
        let mut symbols = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut captures =
            cursor.captures(query, parsed.tree.root_node(), parsed.source.as_slice());
        while let Some((mat, _i)) = captures.next() {
            for capture in mat.captures {
                let node = capture.node;
                let name = node
                    .utf8_text(parsed.source.as_slice())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let range = node.byte_range();
                let start_line = node.start_position().row;
                let end_line = node.end_position().row;
                symbols.push(TreeSymbol {
                    name,
                    kind: node.kind().to_string(),
                    byte_range: (range.start, range.end),
                    row_range: (start_line, end_line),
                    precision: Precision::TreeSitter,
                });
            }
        }
        Ok(symbols)
    }

    /// Extract references from `source` using tree-sitter queries.
    pub fn references(&self, language: &str, source: &[u8]) -> Result<Vec<TreeReference>> {
        let parsed = self.parse(language, source)?;
        let query = self.query_for(language, REFERENCE_QUERY)?;
        let mut refs = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut captures =
            cursor.captures(query, parsed.tree.root_node(), parsed.source.as_slice());
        while let Some((mat, _i)) = captures.next() {
            for capture in mat.captures {
                let node = capture.node;
                let name = node
                    .utf8_text(parsed.source.as_slice())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let range = node.byte_range();
                let start_line = node.start_position().row;
                let end_line = node.end_position().row;
                refs.push(TreeReference {
                    name,
                    byte_range: (range.start, range.end),
                    row_range: (start_line, end_line),
                    precision: Precision::TreeSitter,
                });
            }
        }
        Ok(refs)
    }

    /// Load (lazily) the symbol and reference queries for `language`.
    fn query_for(&self, language: &str, kind: &str) -> Result<&Query> {
        let queries = self.queries.get_or_init(|| {
            let mut map = HashMap::new();
            for lang in TOP_EIGHT_LANGUAGES {
                let mut lang_map = HashMap::new();
                if let Ok(lang_ptr) = language_pointer(lang) {
                    let symbol_q = symbol_queries().get(lang).copied().unwrap_or("");
                    let reference_q = reference_queries().get(lang).copied().unwrap_or("");
                    if let Ok(q) = Query::new(&lang_ptr, symbol_q) {
                        lang_map.insert(SYMBOL_QUERY.to_string(), q);
                    }
                    if let Ok(q) = Query::new(&lang_ptr, reference_q) {
                        lang_map.insert(REFERENCE_QUERY.to_string(), q);
                    }
                }
                map.insert(lang.to_string(), lang_map);
            }
            map
        });
        let lang_map = queries
            .get(language)
            .ok_or_else(|| anyhow::anyhow!("no queries for {language}"))?;
        lang_map
            .get(kind)
            .ok_or_else(|| anyhow::anyhow!("no {kind} query for {language}"))
    }
}

impl Default for TreeSitter {
    fn default() -> Self {
        Self::new().expect("tree-sitter pool should build")
    }
}

/// Tree-sitter language pointers for the top eight languages.
///
/// Each grammar crate exposes a `LANGUAGE` constant (`LanguageFn`) that we
/// convert into a [`Language`] via `Into`.
fn language_pointer(language: &str) -> Result<Language> {
    match language {
        "rust" => Ok(tree_sitter_rust::LANGUAGE.into()),
        "python" => Ok(tree_sitter_python::LANGUAGE.into()),
        "typescript" => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "javascript" => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "go" => Ok(tree_sitter_go::LANGUAGE.into()),
        "java" => Ok(tree_sitter_java::LANGUAGE.into()),
        "cpp" => Ok(tree_sitter_cpp::LANGUAGE.into()),
        "ruby" => Ok(tree_sitter_ruby::LANGUAGE.into()),
        other => Err(anyhow::anyhow!("unsupported tree-sitter language: {other}")),
    }
}

/// Query sources for symbol extraction per language.
static SYMBOL_QUERIES: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
static REFERENCE_QUERIES: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn symbol_queries() -> &'static HashMap<&'static str, &'static str> {
    SYMBOL_QUERIES.get_or_init(|| {
        HashMap::from([
            (
                "rust",
                r#"
(function_item name: (identifier) @name)
(struct_item name: (type_identifier) @name)
(enum_item name: (type_identifier) @name)
(trait_item name: (type_identifier) @name)
(mod_item name: (identifier) @name)
(const_item name: (identifier) @name)
(type_item name: (type_identifier) @name)
(macro_definition name: (identifier) @name)
"#,
            ),
            (
                "python",
                r#"
(function_definition name: (identifier) @name)
(class_definition name: (identifier) @name)
"#,
            ),
            (
                "typescript",
                r#"
(function_declaration name: (identifier) @name)
(class_declaration name: (type_identifier) @name)
(method_definition name: (property_identifier) @name)
(interface_declaration name: (type_identifier) @name)
(type_alias_declaration name: (type_identifier) @name)
(variable_declarator name: (identifier) @name)
"#,
            ),
            (
                // JavaScript is parsed by the TypeScript grammar, which is why
                // `language_pointer` hands the same pointer to both names — so
                // the query names have to be the TypeScript ones.
                "javascript",
                r#"
(function_declaration name: (identifier) @name)
(class_declaration name: (type_identifier) @name)
(method_definition name: (property_identifier) @name)
(variable_declarator name: (identifier) @name)
"#,
            ),
            (
                "go",
                r#"
(function_declaration name: (identifier) @name)
(method_declaration name: (field_identifier) @name)
(type_spec name: (type_identifier) @name)
"#,
            ),
            (
                "java",
                r#"
(method_declaration name: (identifier) @name)
(class_declaration name: (identifier) @name)
(interface_declaration name: (identifier) @name)
(enum_declaration name: (identifier) @name)
(constructor_declaration name: (identifier) @name)
"#,
            ),
            (
                "cpp",
                r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name))
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name)))
(class_specifier name: (type_identifier) @name)
(struct_specifier name: (type_identifier) @name)
"#,
            ),
            (
                "ruby",
                r#"
(method name: (identifier) @name)
(class name: (constant) @name)
(module name: (constant) @name)
"#,
            ),
        ])
    })
}

fn reference_queries() -> &'static HashMap<&'static str, &'static str> {
    REFERENCE_QUERIES.get_or_init(|| {
        HashMap::from([
            (
                "rust",
                r#"
(call_expression function: (identifier) @ref)
(call_expression function: (field_expression field: (field_identifier) @ref))
(call_expression function: (scoped_identifier name: (identifier) @ref))
"#,
            ),
            (
                "python",
                r#"
(call function: (identifier) @ref)
(call function: (attribute attribute: (identifier) @ref))
"#,
            ),
            (
                "typescript",
                r#"
(call_expression function: (identifier) @ref)
(call_expression function: (member_expression property: (property_identifier) @ref))
"#,
            ),
            (
                "javascript",
                r#"
(call_expression function: (identifier) @ref)
(call_expression function: (member_expression property: (property_identifier) @ref))
"#,
            ),
            (
                "go",
                r#"
(call_expression function: (identifier) @ref)
(call_expression function: (selector_expression field: (field_identifier) @ref))
"#,
            ),
            (
                "java",
                r#"
(method_invocation name: (identifier) @ref)
(object_creation_expression type: (type_identifier) @ref)
"#,
            ),
            (
                "cpp",
                r#"
(call_expression function: (identifier) @ref)
(call_expression function: (field_expression field: (field_identifier) @ref))
"#,
            ),
            (
                "ruby",
                r#"
(call method: (identifier) @ref)
"#,
            ),
        ])
    })
}

#[cfg(test)]
mod eight_language_tests {
    use super::*;
    use crate::lsp::TOP_EIGHT_LANGUAGES;

    /// One declaration named `alpha` per language, in that language's own syntax.
    const FIXTURES: [(&str, &str); 8] = [
        ("rust", "fn alpha() {}\n"),
        ("python", "def alpha():\n    pass\n"),
        ("typescript", "function alpha(): void {}\n"),
        ("javascript", "function alpha() {}\n"),
        ("go", "package main\nfunc alpha() {}\n"),
        ("java", "class Box { void alpha() {} }\n"),
        ("cpp", "int alpha() { return 0; }\n"),
        ("ruby", "def alpha\nend\n"),
    ];

    #[test]
    fn every_top_eight_language_yields_the_declaration_it_parsed() {
        let parser = TreeSitter::new().expect("the query pool builds");
        for (language, source) in FIXTURES {
            let symbols = parser
                .symbols(language, source.as_bytes())
                .unwrap_or_else(|error| panic!("{language} has no symbol query: {error}"));
            assert!(
                symbols.iter().any(|symbol| symbol.name == "alpha"),
                "{language} produced no `alpha` declaration: {symbols:?}"
            );
            assert!(
                symbols
                    .iter()
                    .all(|symbol| symbol.precision == Precision::TreeSitter),
                "{language} must report tree-sitter precision, not a guess"
            );
        }
    }

    #[test]
    fn every_top_eight_language_has_a_reference_query() {
        let parser = TreeSitter::new().expect("the query pool builds");
        for (language, source) in FIXTURES {
            parser
                .references(language, source.as_bytes())
                .unwrap_or_else(|error| panic!("{language} has no reference query: {error}"));
        }
    }

    #[test]
    fn the_fixture_list_covers_the_declared_surface() {
        for language in TOP_EIGHT_LANGUAGES {
            assert!(
                FIXTURES.iter().any(|(name, _)| *name == language),
                "{language} is promised but has no fixture"
            );
        }
    }
}

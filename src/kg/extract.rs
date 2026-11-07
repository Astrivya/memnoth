//! Language detection and structural symbol extraction.
//!
//! This is a **line scanner**, not a parser. It recognises the declarations each
//! language spells out on one line and pairs an opening brace (or an
//! indentation change, for Python) with its close. That is enough for the two
//! questions `kg` exists to answer — "where is this defined" and "who mentions
//! it" — and it is deliberately conservative: a symbol with no `end_line` is
//! better than a wrong one, because the reference scan uses ranges to decide
//! which region a mention belongs to.
//!
//! Known limits, stated rather than hidden: braces inside string literals and
//! comments are counted, so a pathological file can close a frame early; macro
//! bodies are opaque; and a declaration whose name sits on a different line from
//! its keyword is not recognised. The production `memnoth-codegraph` crate owns
//! real parsing — this scanner is what the CLI can do without it.

use std::sync::LazyLock;

use regex::Regex;

/// One declaration found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedSymbol {
    /// Declared name, without qualifiers.
    pub name: String,
    /// `fn`, `struct`, `class`, `impl`, … — the language's own word where it has one.
    pub kind: &'static str,
    /// 1-based line of the declaration.
    pub line: usize,
    /// 1-based line the declaration closes on (best effort; never before `line`).
    pub end_line: usize,
    /// Enclosing declaration's name, when one is open at this line.
    pub container: Option<String>,
    /// Whether the declaration is part of the file's public surface: `pub` in
    /// Rust, `export` in the script languages, a top-level name where the
    /// language has no keyword for it. Reports that answer "is this dead?"
    /// need this, because for TypeScript every local `const` otherwise looked
    /// like an unused export.
    pub exported: bool,
}

/// Language label for a workspace-relative path.
///
/// Unknown extensions return `"other"`: they still count as files in the graph,
/// they simply contribute no symbols, and the status report says so by name.
pub(crate) fn language_for(relative_path: &str) -> &'static str {
    let extension = relative_path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" | "pyi" => "python",
        "go" => "go",
        "java" => "java",
        "cs" => "csharp",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "c" | "h" => "c",
        "rb" => "ruby",
        "php" => "php",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "scala" | "sc" => "scala",
        _ => "other",
    }
}

/// Whether this build can read declarations out of a language.
///
/// `language_for` labels a file even when nothing here can parse it (`json`,
/// `css`, `markdown` are all `other`). A caller that is about to treat a file's
/// text as evidence has to ask this rather than compare the label to `"other"`,
/// because `other` is also what an unrecognised extension gets.
pub(crate) fn has_scanner(language: &str) -> bool {
    matches!(
        language,
        "rust"
            | "typescript"
            | "tsx"
            | "javascript"
            | "jsx"
            | "python"
            | "go"
            | "java"
            | "csharp"
            | "kotlin"
            | "swift"
            | "scala"
            | "cpp"
            | "c"
            | "php"
            | "ruby"
    )
}

/// Extract every declaration this scanner understands from `text`.
pub(crate) fn extract(language: &str, text: &str) -> Vec<ExtractedSymbol> {
    match language {
        "rust" => brace_extract(text, detect_rust, language),
        "typescript" | "tsx" | "javascript" | "jsx" => brace_extract(text, detect_script, language),
        "python" => extract_python(text),
        "go" => brace_extract(text, detect_go, language),
        "java" | "csharp" | "kotlin" | "swift" | "scala" | "cpp" | "c" | "php" => {
            brace_extract(text, detect_typed, language)
        }
        "ruby" => extract_ruby(text),
        _ => Vec::new(),
    }
}

/// Whether a declaration is part of its file's public surface.
///
/// Only the languages that spell it out get keyword detection. Python, Go and
/// Ruby have no such keyword, so their module-level names are the surface and a
/// nested one is not — the same rule that makes `dead-code` mean something there.
fn is_exported(language: &str, line: &str, container: Option<&str>) -> bool {
    let trimmed = line.trim_start();
    match language {
        "rust" => trimmed.starts_with("pub"),
        "typescript" | "tsx" | "javascript" | "jsx" => trimmed.starts_with("export"),
        "java" | "csharp" | "kotlin" | "swift" | "scala" | "cpp" | "c" | "php" => {
            trimmed.starts_with("public") || trimmed.starts_with("export")
        }
        _ => container.is_none(),
    }
}

/// A brace-delimited declaration detector: one line in, one symbol out.
type Detect = fn(&str, i32) -> Option<(String, &'static str)>;

/// Walk a brace language, pairing each declaration with the line its brace closes on.
fn brace_extract(text: &str, detect: Detect, language: &str) -> Vec<ExtractedSymbol> {
    struct Frame {
        index: usize,
        parent_depth: i32,
        name: String,
    }

    let mut symbols: Vec<ExtractedSymbol> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut depth: i32 = 0;
    let mut last_line = 0usize;

    for (offset, raw) in text.lines().enumerate() {
        let line_number = offset + 1;
        last_line = line_number;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let opens = raw.matches('{').count() as i32;
        let closes = raw.matches('}').count() as i32;
        let before = depth;
        let container = frames.last().map(|frame| frame.name.clone());

        if let Some((name, kind)) = detect(trimmed, before) {
            let index = symbols.len();
            symbols.push(ExtractedSymbol {
                name: name.clone(),
                kind,
                line: line_number,
                end_line: line_number,
                exported: is_exported(language, raw, container.as_deref()),
                container: container.clone(),
            });
            if opens > 0 {
                frames.push(Frame {
                    index,
                    parent_depth: before,
                    name,
                });
            }
        }

        depth += opens - closes;

        // Close every frame whose body ended on this line. `>=` (not `>`) is what
        // lets a one-line `fn f() {}` close on the line it opened.
        while let Some(frame) = frames.last() {
            let closes_here =
                depth <= frame.parent_depth && line_number >= symbols[frame.index].line;
            if !closes_here {
                break;
            }
            let frame = frames.pop().expect("checked immediately above");
            symbols[frame.index].end_line = line_number;
        }
    }

    // A file that ends with an unclosed frame (truncated, or an unbalanced brace
    // inside a string) still reports the symbol; the range simply ends with the
    // file rather than pointing past it.
    for frame in frames {
        symbols[frame.index].end_line = last_line.max(symbols[frame.index].line);
    }
    symbols
}

struct Patterns {
    rust_fn: Regex,
    rust_struct: Regex,
    rust_enum: Regex,
    rust_trait: Regex,
    rust_mod: Regex,
    rust_type: Regex,
    rust_union: Regex,
    rust_const: Regex,
    rust_static: Regex,
    rust_macro: Regex,
    script_fn: Regex,
    script_class: Regex,
    script_interface: Regex,
    script_enum: Regex,
    script_type: Regex,
    script_namespace: Regex,
    script_assign: Regex,
    script_method: Regex,
    go_fn: Regex,
    go_type: Regex,
    typed_class: Regex,
    typed_interface: Regex,
    typed_enum: Regex,
    typed_struct: Regex,
    typed_record: Regex,
    typed_protocol: Regex,
    typed_trait: Regex,
    typed_namespace: Regex,
    typed_fun: Regex,
    typed_func: Regex,
    typed_function: Regex,
}

static P: LazyLock<Patterns> = LazyLock::new(|| {
    let re = |pattern: &str| Regex::new(pattern).expect("extractor patterns are valid");
    Patterns {
        rust_fn: re(
            r#"^(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?(?:const\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"#,
        ),
        rust_struct: re(r"^(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)"),
        rust_enum: re(r"^(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)"),
        rust_trait: re(r"^(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)"),
        rust_mod: re(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)"),
        rust_type: re(r"^(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)"),
        rust_union: re(r"^(?:pub(?:\([^)]*\))?\s+)?union\s+([A-Za-z_][A-Za-z0-9_]*)"),
        rust_const: re(r"^(?:pub(?:\([^)]*\))?\s+)?const\s+([A-Za-z_][A-Za-z0-9_]*)\s*:"),
        rust_static: re(
            r"^(?:pub(?:\([^)]*\))?\s+)?static\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:",
        ),
        rust_macro: re(r"^macro_rules!\s*([A-Za-z_][A-Za-z0-9_]*)"),
        script_fn: re(r"^(?:async\s+)?function\s*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)"),
        script_class: re(r"^class\s+([A-Za-z_$][A-Za-z0-9_$]*)"),
        script_interface: re(r"^interface\s+([A-Za-z_$][A-Za-z0-9_$]*)"),
        script_enum: re(r"^enum\s+([A-Za-z_$][A-Za-z0-9_$]*)"),
        script_type: re(r"^type\s+([A-Za-z_$][A-Za-z0-9_$]*)"),
        script_namespace: re(r"^namespace\s+([A-Za-z_$][A-Za-z0-9_$]*)"),
        script_assign: re(
            r"^(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?::[^=]+)?=\s*(.*)$",
        ),
        script_method: re(
            r"^(?:(?:public|private|protected|static|async|readonly|get|set|override|abstract)\s+)*([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
        ),
        go_fn: re(r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)"),
        go_type: re(r"^type\s+([A-Za-z_][A-Za-z0-9_]*)\s+([A-Za-z]+)"),
        typed_class: re(
            r"^(?:(?:final|abstract|public|private|internal|protected|sealed|partial|static|open|data|readonly)\s+)*class\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_interface: re(
            r"^(?:(?:public|private|internal|protected|sealed|abstract|partial)\s+)*interface\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_enum: re(
            r"^(?:(?:public|private|internal|protected)\s+)*enum\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_struct: re(
            r"^(?:(?:public|private|internal|protected)\s+)*struct\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_record: re(
            r"^(?:(?:public|private|internal|protected)\s+)*record\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_protocol: re(
            r"^(?:(?:public|private|internal|protected)\s+)*protocol\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_trait: re(
            r"^(?:(?:public|private|internal|protected)\s+)*trait\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_namespace: re(
            r"^(?:(?:public|private|internal|protected)\s+)*namespace\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_fun: re(
            r"^(?:(?:public|private|internal|protected|static|override|final|open|suspend|inline)\s+)*fun\s+(?:<[^>]*>\s*)?([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_func: re(
            r"^(?:(?:public|private|internal|protected|static|override|mutating|class)\s+)*func\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
        typed_function: re(
            r"^(?:(?:public|private|protected|static|abstract)\s+)*function\s+([A-Za-z_][A-Za-z0-9_]*)",
        ),
    }
});

fn detect_rust(line: &str, _depth: i32) -> Option<(String, &'static str)> {
    let patterns: [(&Regex, &'static str); 10] = [
        (&P.rust_fn, "fn"),
        (&P.rust_struct, "struct"),
        (&P.rust_enum, "enum"),
        (&P.rust_trait, "trait"),
        (&P.rust_mod, "mod"),
        (&P.rust_type, "type"),
        (&P.rust_union, "union"),
        (&P.rust_const, "const"),
        (&P.rust_static, "static"),
        (&P.rust_macro, "macro"),
    ];
    for (pattern, kind) in patterns {
        if let Some(caps) = pattern.captures(line) {
            return Some((caps[1].to_string(), kind));
        }
    }
    if line.starts_with("impl") || line.starts_with("unsafe impl") {
        let rest = line
            .trim_start_matches("unsafe ")
            .trim_start_matches("impl");
        return impl_target(rest).map(|name| (name, "impl"));
    }
    None
}

/// The type an `impl` block belongs to.
///
/// `impl Display for MemoryEngine` is about `MemoryEngine`, not `Display`: the
/// traversal question is "what is in this type", so the type wins. Generics and
/// where-clauses are stripped rather than parsed.
fn impl_target(rest: &str) -> Option<String> {
    let rest = skip_generics(rest.trim());
    let rest = rest.split(" where ").next().unwrap_or(rest);
    let target = match rest.rsplit_once(" for ") {
        Some((_, after)) => after,
        None => rest,
    };
    let target = target.trim();
    let name: String = target
        .chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == ':')
        .collect();
    let name = name.rsplit("::").next().unwrap_or(&name).trim().to_string();
    if name.is_empty() || !name.starts_with(|ch: char| ch.is_alphabetic() || ch == '_') {
        return None;
    }
    Some(name)
}

fn skip_generics(text: &str) -> &str {
    if !text.starts_with('<') {
        return text;
    }
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return text[index + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    text
}

fn detect_script(line: &str, depth: i32) -> Option<(String, &'static str)> {
    let mut body = line;
    for prefix in ["export ", "default ", "declare ", "abstract "] {
        if let Some(rest) = body.strip_prefix(prefix) {
            body = rest.trim_start();
        }
    }

    if let Some(caps) = P.script_fn.captures(body) {
        return Some((caps[1].to_string(), "function"));
    }
    let patterns: [(&Regex, &'static str); 5] = [
        (&P.script_class, "class"),
        (&P.script_interface, "interface"),
        (&P.script_enum, "enum"),
        (&P.script_type, "type"),
        (&P.script_namespace, "namespace"),
    ];
    for (pattern, kind) in patterns {
        if let Some(caps) = pattern.captures(body) {
            return Some((caps[1].to_string(), kind));
        }
    }
    if let Some(caps) = P.script_assign.captures(body) {
        let initializer = caps[2].trim_start();
        let is_function = initializer.starts_with("async function")
            || initializer.starts_with("function")
            || initializer.contains("=>");
        return Some((
            caps[1].to_string(),
            if is_function { "function" } else { "const" },
        ));
    }
    // A method is only recognised inside a body, where indentation plus a trailing
    // `{` is the shape every formatter produces. Keywords are excluded so
    // `if (x) {` cannot be stored as a declaration, and the shape check below
    // excludes calls whose argument happens to end in `{` — `useEffect(() => {`
    // was being stored as a method called `useEffect` in every component.
    if depth > 0
        && line.ends_with('{')
        && let Some(caps) = P.script_method.captures(line)
    {
        let name = caps[1].to_string();
        let from_name = &line[caps.get(1).map(|m| m.start()).unwrap_or(0)..];
        if !KEYWORDS.contains(&name.as_str()) && is_method_shape(from_name) {
            return Some((name, "method"));
        }
    }
    None
}

/// Whether a line that starts at a method's name has a *declaration* shape:
/// `name(args) {`, optionally through a return type (`name(a): R {`).
///
/// A call with a function argument also ends in `{` — `useEffect(() => {` — but
/// after its argument list comes `=>`, and a declaration never has an operator
/// between the parameter list and the body.
fn is_method_shape(from_name: &str) -> bool {
    let after_name =
        from_name.trim_start_matches(|ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$');
    let Some(after_open) = after_name.trim_start().strip_prefix('(') else {
        return false;
    };
    let mut depth = 1usize;
    let mut end = None;
    for (index, ch) in after_open.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return false;
    };
    let after_params = after_open[end + 1..].trim();
    after_params.ends_with('{')
        && !after_params.contains('=')
        && !after_params.contains(';')
        && !after_params.contains(')')
}

fn detect_go(line: &str, _depth: i32) -> Option<(String, &'static str)> {
    if let Some(caps) = P.go_fn.captures(line) {
        return Some((caps[1].to_string(), "func"));
    }
    if let Some(caps) = P.go_type.captures(line) {
        let kind = match &caps[2] {
            "struct" => "struct",
            "interface" => "interface",
            _ => "type",
        };
        return Some((caps[1].to_string(), kind));
    }
    None
}

fn detect_typed(line: &str, _depth: i32) -> Option<(String, &'static str)> {
    let patterns: [(&Regex, &'static str); 11] = [
        (&P.typed_class, "class"),
        (&P.typed_interface, "interface"),
        (&P.typed_enum, "enum"),
        (&P.typed_struct, "struct"),
        (&P.typed_record, "record"),
        (&P.typed_protocol, "protocol"),
        (&P.typed_trait, "trait"),
        (&P.typed_namespace, "namespace"),
        (&P.typed_fun, "fun"),
        (&P.typed_func, "func"),
        (&P.typed_function, "function"),
    ];
    for (pattern, kind) in patterns {
        if let Some(caps) = pattern.captures(line) {
            return Some((caps[1].to_string(), kind));
        }
    }
    None
}

fn extract_python(text: &str) -> Vec<ExtractedSymbol> {
    static DEF: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid")
    });
    static CLASS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^class\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid"));

    let lines: Vec<&str> = text.lines().collect();
    let mut symbols: Vec<ExtractedSymbol> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();

    for (offset, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = indentation(raw);
        while let Some((level, _)) = stack.last() {
            if *level >= indent {
                stack.pop();
            } else {
                break;
            }
        }
        let declaration = if let Some(caps) = DEF.captures(trimmed) {
            Some((caps[1].to_string(), "def"))
        } else {
            CLASS
                .captures(trimmed)
                .map(|caps| (caps[1].to_string(), "class"))
        };
        let Some((name, kind)) = declaration else {
            continue;
        };

        // Python has no closing brace: the body ends at the last line indented
        // further than the declaration.
        let mut end = offset + 1;
        for (next_offset, next) in lines.iter().enumerate().skip(offset + 1) {
            if next.trim().is_empty() {
                continue;
            }
            if indentation(next) <= indent {
                break;
            }
            end = next_offset + 1;
        }

        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind,
            line: offset + 1,
            end_line: end,
            exported: stack.is_empty(),
            container: stack.last().map(|(_, name)| name.clone()),
        });
        stack.push((indent, name));
    }
    symbols
}

fn extract_ruby(text: &str) -> Vec<ExtractedSymbol> {
    static DEF: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^def\s+(?:self\.)?([A-Za-z_][A-Za-z0-9_]*[?!]?)").expect("valid")
    });
    static CLASS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^class\s+([A-Za-z_][A-Za-z0-9_:]*)").expect("valid"));
    static MODULE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^module\s+([A-Za-z_][A-Za-z0-9_:]*)").expect("valid"));

    let mut symbols = Vec::new();
    let mut container: Vec<(usize, String)> = Vec::new();
    for (offset, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = indentation(raw);
        while let Some((level, _)) = container.last() {
            if *level >= indent {
                container.pop();
            } else {
                break;
            }
        }
        let declaration = if let Some(caps) = DEF.captures(trimmed) {
            Some((caps[1].to_string(), "def"))
        } else if let Some(caps) = CLASS.captures(trimmed) {
            Some((caps[1].to_string(), "class"))
        } else {
            MODULE
                .captures(trimmed)
                .map(|caps| (caps[1].to_string(), "module"))
        };
        let Some((name, kind)) = declaration else {
            continue;
        };
        symbols.push(ExtractedSymbol {
            name: name.clone(),
            kind,
            line: offset + 1,
            // Ruby closes with `end`; tracking that without parsing strings and
            // blocks would guess. One line is honest; the reference scan still
            // finds mentions.
            end_line: offset + 1,
            exported: container.is_empty(),
            container: container.last().map(|(_, name)| name.clone()),
        });
        container.push((indent, name));
    }
    symbols
}

fn indentation(line: &str) -> usize {
    line.chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
}

/// Words that look like calls but are control flow. Excluded from method
/// detection and from the reference scan so `if`, `for` and `return` cannot be
/// counted as references to a symbol that happens to share their spelling.
pub(crate) const KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "def",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "func",
    "function",
    "if",
    "impl",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "new",
    "nil",
    "none",
    "null",
    "of",
    "package",
    "private",
    "protected",
    "pub",
    "public",
    "raise",
    "ref",
    "return",
    "self",
    "static",
    "struct",
    "super",
    "switch",
    "this",
    "throw",
    "trait",
    "true",
    "try",
    "type",
    "typeof",
    "unsafe",
    "use",
    "var",
    "void",
    "where",
    "while",
    "with",
    "yield",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_declarations_are_found_with_ranges_and_containers() {
        let source = "pub struct MemoryEngine;\n\nimpl MemoryEngine {\n    pub fn open(path: &Path) -> Result<Self> {\n        todo!()\n    }\n}\n";
        let symbols = extract("rust", source);
        let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
        assert!(names.contains(&"MemoryEngine"), "{names:?}");
        assert!(names.contains(&"open"), "{names:?}");

        let open = symbols.iter().find(|symbol| symbol.name == "open").unwrap();
        assert_eq!(open.kind, "fn");
        assert_eq!(open.line, 4);
        assert_eq!(open.end_line, 6);
        assert_eq!(open.container.as_deref(), Some("MemoryEngine"));
    }

    #[test]
    fn an_impl_target_is_the_type_not_the_trait() {
        let symbols = extract("rust", "impl Display for MemoryEngine {\n}\n");
        assert_eq!(symbols[0].name, "MemoryEngine");
        assert_eq!(symbols[0].kind, "impl");
    }

    #[test]
    fn script_declarations_and_arrow_functions_are_recognised() {
        let source = "export function run() {\n}\nexport const load = async () => {\n};\nclass Server {\n  handle(req) {\n  }\n}\n";
        let symbols = extract("typescript", source);
        let names: Vec<(&str, &str)> = symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.kind))
            .collect();
        assert!(names.contains(&("run", "function")), "{names:?}");
        assert!(names.contains(&("load", "function")), "{names:?}");
        assert!(names.contains(&("Server", "class")), "{names:?}");
        assert!(names.contains(&("handle", "method")), "{names:?}");
    }

    #[test]
    fn export_default_function_is_not_lost_to_the_prefix_strip() {
        let symbols = extract("javascript", "export default function boot() {\n}\n");
        assert_eq!(symbols[0].name, "boot");
    }

    #[test]
    fn python_bodies_end_where_the_indentation_does() {
        let source = "class Store:\n    def load(self, key):\n        return key\n\n    def other(self):\n        pass\n";
        let symbols = extract("python", source);
        let load = symbols.iter().find(|symbol| symbol.name == "load").unwrap();
        assert_eq!(load.line, 2);
        assert_eq!(load.end_line, 3);
        assert_eq!(load.container.as_deref(), Some("Store"));
        assert_eq!(symbols.iter().filter(|s| s.name == "other").count(), 1);
    }

    #[test]
    fn a_call_with_a_function_argument_is_not_a_method() {
        let source = "function Hero() {\n  useEffect(() => {\n    go();\n  }, []);\n}\n";
        let symbols = extract("tsx", source);
        assert!(
            !symbols.iter().any(|symbol| symbol.name == "useEffect"),
            "{symbols:?}"
        );
        assert!(symbols.iter().any(|symbol| symbol.name == "Hero"));
    }

    #[test]
    fn a_method_with_a_return_type_is_still_a_method() {
        let symbols = extract("typescript", "class A {\n  load(key): Row {\n  }\n}\n");
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "load" && symbol.kind == "method"),
            "{symbols:?}"
        );
    }

    #[test]
    fn control_flow_is_not_a_method() {
        let symbols = extract("javascript", "function f(x) {\n  if (x) {\n  }\n}\n");
        assert!(
            !symbols.iter().any(|symbol| symbol.name == "if"),
            "{symbols:?}"
        );
    }

    #[test]
    fn unknown_extensions_are_named_not_guessed() {
        assert_eq!(language_for("README.md"), "other");
        assert_eq!(language_for("src/main.rs"), "rust");
        assert_eq!(language_for("a/b.TSX"), "tsx");
    }
}

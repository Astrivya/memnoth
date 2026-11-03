//! Argument shapes for each search mode.
//!
//! Plain data — the tool layer builds these programmatically, never from a
//! command line, so there are no CLI derives here. Field-for-field compatible
//! with what the consumers construct.

#[derive(Debug, Clone)]
pub struct GrepArgs {
    /// Exact query to search for.
    pub query: String,
    /// Treat the query as a regular expression.
    pub regex: bool,
    /// Restrict to a known file type.
    pub file_type: Option<String>,
    /// Emit JSON output.
    pub json: bool,
    /// Print only matching file paths.
    pub paths_only: bool,
    /// Include hidden files.
    pub hidden: bool,
    /// Ignore .gitignore and related ignore files.
    pub no_ignore: bool,
    /// Optional root path to search instead of the current directory.
    pub path: Option<String>,
    /// Restrict candidate files by glob.
    pub glob: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FindArgs {
    /// File/path-oriented query terms.
    pub query_parts: Vec<String>,
    /// Restrict to a known file type.
    pub file_type: Option<String>,
    /// Emit JSON output.
    pub json: bool,
    /// Print only matching file paths.
    pub paths_only: bool,
    /// Print score information in human-readable output.
    pub debug_score: bool,
    /// Max files to return.
    pub max_files: usize,
    /// Include hidden files.
    pub hidden: bool,
    /// Ignore .gitignore and related ignore files.
    pub no_ignore: bool,
    /// Optional root path to search instead of the current directory.
    pub path: Option<String>,
    /// Restrict candidate files by glob.
    pub glob: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OutlineArgs {
    /// File path to outline.
    pub file: String,
    /// Emit JSON output.
    pub json: bool,
    /// Maximum structure items to print. Defaults to all detected items.
    pub max_items: Option<usize>,
    /// Optional root path to resolve relative file paths against.
    pub path: Option<String>,
    /// Optional harness context JSON file.
    pub context_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SmartArgs {
    /// Structured smart query DSL terms.
    pub terms: Vec<String>,
    /// Emit JSON output.
    pub json: bool,
    /// Max files to return.
    pub max_files: usize,
    /// Max regions to return per query.
    pub max_regions: usize,
    /// Preferred region expansion mode.
    pub full_region: FullRegionMode,
    /// Print parser/planner details.
    pub debug_plan: bool,
    /// Print score information in human-readable output.
    pub debug_score: bool,
    /// Print only matching file paths.
    pub paths_only: bool,
    /// Optional root path to search instead of the current directory.
    pub path: Option<String>,
    /// Restrict to a known file type.
    pub file_type: Option<String>,
    /// Restrict candidate files by glob.
    pub glob: Option<String>,
    /// Include hidden files.
    pub hidden: bool,
    /// Ignore .gitignore and related ignore files.
    pub no_ignore: bool,
    /// Optional harness context JSON file.
    pub context_json: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FullRegionMode {
    #[default]
    Auto,
    Always,
    Never,
}

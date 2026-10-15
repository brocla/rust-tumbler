//! Argument structs, one per tool shape. Each derives both `Deserialize` (the
//! parser) and `JsonSchema` (what agents are shown), and its doc comments are
//! the property descriptions in that schema. Bounds go in `#[schemars(...)]`
//! attributes, which [`super::tools::check_args`] enforces.

use schemars::JsonSchema;
use serde::Deserialize;

/// No arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocOpenArgs {
    /// The PDF file to open (relative to --root when one is set).
    #[schemars(length(min = 1))]
    pub path: String,
    /// The file's user or owner password, if it is password-protected.
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocCloseArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// Close even if there are unsaved changes, dropping them. Default false.
    #[serde(default)]
    pub discard_changes: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocSaveArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// Save as this file instead (relative to --root when one is set). Omit to
    /// overwrite the file the document was opened from.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageRenderArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The page to render, 1-based.
    #[schemars(range(min = 1))]
    pub page: u32,
    /// Resolution in dots per inch. Default 96.
    #[serde(default)]
    #[schemars(range(min = 1, max = 600))]
    pub dpi: Option<f64>,
    /// Write the PNG to this file (relative to --root when one is set) instead
    /// of returning the image.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub out: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextExtractArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// 1-based page numbers to extract. Default: every page.
    #[serde(default)]
    #[schemars(length(min = 1), inner(range(min = 1)))]
    pub pages: Option<Vec<u32>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextFindArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The text to find, or a regular expression when regex is true.
    #[schemars(length(min = 1))]
    pub query: String,
    /// Match upper and lower case exactly. Default false.
    #[serde(default)]
    pub match_case: bool,
    /// Match only whole words. Default false.
    #[serde(default)]
    pub whole_word: bool,
    /// Treat query as a regular expression (Rust regex syntax). Default false.
    #[serde(default)]
    pub regex: bool,
}

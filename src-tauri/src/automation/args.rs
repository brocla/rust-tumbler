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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocExportTextArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The .txt file to write (relative to --root when one is set). Replaced if
    /// it exists.
    #[schemars(length(min = 1))]
    pub path: String,
    /// Run OCR on pages without a text layer (slow). Default false: such pages
    /// use OCR text from earlier in this session, if any.
    #[serde(default)]
    pub use_ocr: bool,
}

// ── Pages ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageDeleteArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// 1-based page numbers to delete. At least one page must remain.
    #[schemars(length(min = 1), inner(range(min = 1)))]
    pub pages: Vec<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageRotateArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// Clockwise rotation in degrees, a multiple of 90; negative turns
    /// counterclockwise. Added to each page's current rotation.
    pub degrees: i32,
    /// 1-based page numbers to rotate. Default: every page.
    #[serde(default)]
    #[schemars(length(min = 1), inner(range(min = 1)))]
    pub pages: Option<Vec<u32>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageReorderArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The new order: every current 1-based page number exactly once, in the
    /// order they should appear. [3, 1, 2] moves page 3 to the front.
    #[schemars(length(min = 1), inner(range(min = 1)))]
    pub order: Vec<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageInsertFileArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The PDF whose pages to insert (relative to --root when one is set). It
    /// must not be password-protected.
    #[schemars(length(min = 1))]
    pub path: String,
    /// Insert after this 1-based page; 0 inserts before the first page.
    /// Default: after the last page.
    #[serde(default)]
    pub after: Option<u32>,
}

/// A run of pages, 1-based and inclusive.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageRange {
    /// First page of the range, 1-based.
    #[schemars(range(min = 1))]
    pub first: u32,
    /// Last page of the range, 1-based and inclusive.
    #[schemars(range(min = 1))]
    pub last: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocSplitArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The existing folder to write the new files into (relative to --root when
    /// one is set).
    #[schemars(length(min = 1))]
    pub out_dir: String,
    /// Pages per file. Default 1. Not with ranges.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub every: Option<u32>,
    /// One output file per range, instead of every.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub ranges: Option<Vec<PageRange>>,
}

// ── Metadata and passwords ───────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocSetInfoArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// New Title. Omit to keep the current one; "" clears it.
    #[serde(default)]
    pub title: Option<String>,
    /// New Author. Omit to keep the current one; "" clears it.
    #[serde(default)]
    pub author: Option<String>,
    /// New Subject. Omit to keep the current one; "" clears it.
    #[serde(default)]
    pub subject: Option<String>,
    /// New Keywords. Omit to keep the current ones; "" clears them.
    #[serde(default)]
    pub keywords: Option<String>,
    /// New Creator (the authoring application). Omit to keep the current one.
    #[serde(default)]
    pub creator: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocProtectArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The password needed to open the saved file (also its owner password).
    #[schemars(length(min = 1))]
    pub password: String,
}

// ── OCR ──────────────────────────────────────────────────────────────────────

fn yes() -> bool {
    true
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OcrRecognizeArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// 1-based page numbers to recognize. Default: every page.
    #[serde(default)]
    #[schemars(length(min = 1), inner(range(min = 1)))]
    pub pages: Option<Vec<u32>>,
    /// Also recognize pages that already have a text layer, and prefer the OCR
    /// text over it in this session. For scans whose existing text is junk.
    /// Default false.
    #[serde(default)]
    pub force: bool,
    /// Embed the recognized text as an invisible text layer in pages that have
    /// none, so it is saved with the file. False only recognizes, for
    /// text_find and text_extract in this session. Default true.
    #[serde(default = "yes")]
    pub embed: bool,
}

// ── Compression ──────────────────────────────────────────────────────────────

/// One step of the compression pipeline.
#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum OptimizeStep {
    /// Re-deflate content streams (lossless).
    RecompressStreams,
    /// Remove objects nothing refers to (lossless).
    PruneUnused,
    /// Drop empty streams (lossless).
    DeleteZeroLength,
    /// Remove XMP metadata, page thumbnails, JavaScript and embedded files.
    StripExtras,
    /// Downsample images above dpi and re-encode them as JPEG (lossy).
    RecompressImages,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocOptimizeArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// Steps to run; they always run in pipeline order. Default: the four
    /// lossless ones (recompress_streams, prune_unused, delete_zero_length,
    /// strip_extras). recompress_images must be asked for.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub steps: Option<Vec<OptimizeStep>>,
    /// recompress_images: downsample images displayed above this resolution.
    /// Default 150.
    #[serde(default)]
    #[schemars(range(min = 50, max = 300))]
    pub dpi: Option<f32>,
    /// recompress_images: JPEG quality. Default 80.
    #[serde(default)]
    #[schemars(range(min = 50, max = 95))]
    pub quality: Option<u8>,
    /// recompress_images: also re-encode JPEG images already at or below dpi,
    /// keeping their size, when that saves enough. Default false.
    #[serde(default)]
    pub reencode_at_target: bool,
}

// ── Linearize, Expand Margins ────────────────────────────────────────────────

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocLinearizeArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// The file to write (relative to --root when one is set). Replaced if it
    /// exists; it must not be the document's own file.
    #[schemars(length(min = 1))]
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarginsArgs {
    /// Document id, from doc_open or doc_list.
    #[schemars(range(min = 1))]
    pub doc: u64,
    /// Space to leave between the content and the page edge, in points (72 per
    /// inch). Default 18 (a quarter inch).
    #[serde(default)]
    #[schemars(range(min = 0, max = 36))]
    pub padding: Option<f32>,
}

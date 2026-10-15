//! The tool table: name, description, argument schema and handler for every
//! tool.
//!
//! A tool is declared with [`tool`], which takes the argument type and the
//! handler together: the schema is generated from the same struct the handler
//! receives, so what an agent is told and what the parser accepts cannot drift
//! apart. Bounds in the schema (`minimum`, `minItems`, ...) are enforced by
//! [`check_args`] before the handler runs.

use super::args::*;
use super::handlers::{document, render, standards, text};
use super::{Automation, Content, ToolError};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::sync::LazyLock;

type Handler = Box<dyn Fn(&mut Automation, &Value) -> Result<Vec<Content>, ToolError> + Send + Sync>;

pub struct ToolDef {
    /// `[a-z_]+`: MCP clients reject other characters.
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// JSON Schema (draft 2020-12) for the arguments object.
    pub input_schema: Value,
    /// Changes no document and writes no file.
    pub read_only: bool,
    /// May remove content or overwrite a file.
    pub destructive: bool,
    /// The `#[tauri::command]`s this tool automates. A coverage test checks
    /// every GUI command is named here or deliberately excluded.
    pub commands: &'static [&'static str],
    run: Handler,
}

impl ToolDef {
    pub(crate) fn run(&self, auto: &mut Automation, args: &Value) -> Result<Vec<Content>, ToolError> {
        check_args(self, args)?;
        (self.run)(auto, args)
    }

    fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    fn commands(mut self, commands: &'static [&'static str]) -> Self {
        self.commands = commands;
        self
    }
}

impl std::fmt::Debug for ToolDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDef").field("name", &self.name).finish_non_exhaustive()
    }
}

/// Declares a tool whose arguments are `A`.
fn tool<A>(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    handler: fn(&mut Automation, A) -> Result<Vec<Content>, ToolError>,
) -> ToolDef
where
    A: DeserializeOwned + JsonSchema + 'static,
{
    ToolDef {
        name,
        title,
        description,
        input_schema: schema_for::<A>(),
        read_only: false,
        destructive: false,
        commands: &[],
        run: Box::new(move |auto, args| {
            let parsed = serde_json::from_value::<A>(args.clone())
                .map_err(|e| ToolError::InvalidArgs(format!("{name}: {e}")))?;
            handler(auto, parsed)
        }),
    }
}

/// The schema for `A`, self-contained (subschemas inlined, no `$ref`) and with
/// optional arguments typed plainly rather than as `T | null`, which some MCP
/// clients reject.
fn schema_for<A: JsonSchema>() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let mut schema = generator.into_root_schema_for::<A>().to_value();
    tidy_schema(&mut schema);
    if let Some(top) = schema.as_object_mut() {
        // The tool's own title and description say this better.
        for key in ["$schema", "title", "description"] {
            top.remove(key);
        }
        // An argument-less struct comes out without `properties`.
        top.entry("properties").or_insert_with(|| Value::Object(Map::new()));
    }
    schema
}

/// Tidies what schemars emits for an `Option` argument and a wrapped doc
/// comment: `"type": ["integer", "null"]` becomes `"type": "integer"` (absence
/// from `required` already makes it optional), a `"default": null` goes, and a
/// description's line breaks become spaces.
fn tidy_schema(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get_mut("type") {
                types.retain(|t| t != "null");
                if let [only] = types.as_slice() {
                    let only = only.clone();
                    map.insert("type".into(), only);
                }
            }
            if map.get("default").is_some_and(Value::is_null) {
                map.remove("default");
            }
            if let Some(Value::String(d)) = map.get_mut("description") {
                *d = d.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            map.values_mut().for_each(tidy_schema);
        }
        Value::Array(items) => items.iter_mut().for_each(tidy_schema),
        _ => {}
    }
}

static TOOLS: LazyLock<Vec<ToolDef>> = LazyLock::new(table);

/// Every tool, in the order `tools` lists them.
pub fn tools() -> &'static [ToolDef] {
    &TOOLS
}

pub(crate) fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

fn table() -> Vec<ToolDef> {
    vec![
        // ── Documents and session ───────────────────────────────────────────
        tool::<DocOpenArgs>(
            "doc_open",
            "Open a PDF",
            "Open a PDF file and return its document id (used by every other tool), page count, \
             page sizes in points as displayed, and whether it is encrypted or linearized. A \
             password-protected file needs its password. A file can be open only once; opening \
             it again fails and names the existing id. Nothing is written.",
            document::doc_open,
        )
        .commands(&["open_document"]),
        tool::<NoArgs>(
            "doc_list",
            "List open documents",
            "List the open documents with their ids, paths, page counts and whether they have \
             unsaved changes.",
            document::doc_list,
        )
        .read_only(),
        tool::<DocArgs>(
            "doc_info",
            "Inspect a document",
            "A summary of one open document: path, page sizes, metadata (title, author, dates \
             and so on), declared PDF/A, PDF/X, PDF/E and PDF/UA conformance, the signature count \
             and overall status, whether it has a form, and which pages have no text layer. \
             pages_without_text lists pages that need OCR before their text can be found or \
             extracted; pages_ocr_only lists pages that have OCR text from this session but \
             nothing in the file, so it is lost on save unless ocr_recognize embeds it.",
            document::doc_info,
        )
        .read_only()
        .commands(&["get_metadata", "page_text_coverage"]),
        tool::<DocCloseArgs>(
            "doc_close",
            "Close a document",
            "Close an open document and forget it. Fails if it has unsaved changes, unless \
             discard_changes is true, which drops them. There is no undo: closing without saving \
             is how to abandon edits.",
            document::doc_close,
        )
        .commands(&["close_document"]),
        tool::<DocSaveArgs>(
            "doc_save",
            "Save a document",
            "Write the document, with all its in-memory edits, to disk. Without path it \
             overwrites the file it was opened from. With path it writes a new file (Save As) \
             and the document then belongs to that file. The write is atomic. A password-\
             protected document is encrypted again (AES-256). Saving stamps the modification \
             date. Saving over a file that is open as another document fails.",
            document::doc_save,
        )
        .destructive()
        .commands(&["save_document", "save_document_as"]),
        // ── Rendering and text ──────────────────────────────────────────────
        tool::<PageRenderArgs>(
            "page_render",
            "Render a page",
            "Render one page to a PNG image, 96 dpi by default and at most 600, with annotations \
             and form values drawn. Without out the image is returned; with out it is written \
             to that file and its path and size are returned. The document is not changed.",
            render::page_render,
        )
        .read_only()
        .commands(&["render_page"]),
        tool::<TextExtractArgs>(
            "text_extract",
            "Extract text",
            "The plain text of each page, in the order it is defined in the file. A page \
             without a text layer uses OCR text recognized earlier in this session, if any; \
             source says which (text, ocr, or none). This tool does not run OCR itself: use \
             ocr_recognize first for scanned pages (see doc_info's pages_without_text).",
            text::text_extract,
        )
        .read_only()
        .commands(&["extract_page_text"]),
        tool::<TextFindArgs>(
            "text_find",
            "Find text",
            "Search every page for a string or regular expression and return each match's \
             rectangles, in points from the top-left of the page as displayed (rotation \
             applied). Case-insensitive unless match_case. Pages without a text layer are \
             searched through this session's OCR text, if any.",
            text::text_find,
        )
        .read_only()
        .commands(&["search_document"]),
        // ── Signatures and standards ────────────────────────────────────────
        tool::<DocArgs>(
            "sign_list",
            "Verify signatures",
            "List the document's digital signatures and whether each still covers the current \
             bytes (an edit after signing breaks it). Verification only: this does not check \
             certificate trust and cannot sign.",
            standards::sign_list,
        )
        .read_only()
        .commands(&["get_signature_info"]),
        tool::<DocArgs>(
            "doc_conformance",
            "Declared standards",
            "The ISO sub-formats the document declares in its XMP metadata (PDF/A, PDF/X, PDF/E, \
             PDF/UA), as labels such as \"PDF/A-2b\". A declaration is a claim by the producer; \
             this does not check that the file meets it.",
            standards::doc_conformance,
        )
        .read_only()
        .commands(&["get_conformance"]),
    ]
}

/// Checks `args` against `def`'s schema: an object; no unknown or missing
/// arguments; each top-level argument of the declared JSON type and within
/// its bounds. Run before parsing, so errors name the argument rather than a
/// serde position.
pub(crate) fn check_args(def: &ToolDef, args: &Value) -> Result<(), ToolError> {
    let invalid = |m: String| ToolError::InvalidArgs(format!("{}: {m}", def.name));
    let obj = args.as_object().ok_or_else(|| invalid("arguments must be a JSON object".into()))?;
    let props = def.input_schema["properties"].as_object().cloned().unwrap_or_default();
    if let Some(unknown) = obj.keys().find(|k| !props.contains_key(*k)) {
        let known: Vec<&str> = props.keys().map(String::as_str).collect();
        return Err(invalid(format!("unknown argument `{unknown}` (expected: {})", known.join(", "))));
    }
    for required in def.input_schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if obj.get(required).is_none_or(Value::is_null) {
            return Err(invalid(format!("missing argument `{required}`")));
        }
    }
    for (name, value) in obj {
        if !value.is_null() {
            check_value(&props[name], value).map_err(|m| invalid(format!("argument `{name}` {m}")))?;
        }
    }
    Ok(())
}

/// One value against its (inlined) schema: type, numeric bounds, lengths, and
/// the same for array items.
fn check_value(schema: &Value, value: &Value) -> Result<(), String> {
    let ty = schema["type"].as_str().unwrap_or("");
    let type_ok = match ty {
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    };
    if !type_ok {
        return Err(format!("must be {}", article(ty)));
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = schema["minimum"].as_f64().filter(|min| n < *min) {
            return Err(format!("must be at least {min}"));
        }
        if let Some(max) = schema["maximum"].as_f64().filter(|max| n > *max) {
            return Err(format!("must be at most {max}"));
        }
    }
    if let Some(s) = value.as_str() {
        if schema["minLength"].as_u64().is_some_and(|min| (s.chars().count() as u64) < min) {
            return Err("must not be empty".into());
        }
    }
    if let Some(items) = value.as_array() {
        if let Some(min) = schema["minItems"].as_u64().filter(|min| (items.len() as u64) < *min) {
            return Err(format!("needs at least {min} item(s)"));
        }
        if let Some(max) = schema["maxItems"].as_u64().filter(|max| (items.len() as u64) > *max) {
            return Err(format!("takes at most {max} item(s)"));
        }
        for item in items {
            check_value(&schema["items"], item).map_err(|m| format!("item {item} {m}"))?;
        }
    }
    Ok(())
}

fn article(ty: &str) -> String {
    match ty {
        "integer" | "array" | "object" => format!("an {ty}"),
        _ => format!("a {ty}"),
    }
}

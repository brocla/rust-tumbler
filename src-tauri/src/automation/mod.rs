//! Tumbler's engine as a table of tools, for `tumbler-cli` and its MCP server
//! (doc/cli-plan.html).
//!
//! Every tool has a name, a description, a JSON Schema for its arguments and a
//! handler; [`Automation::call`] runs one by name with JSON arguments. The CLI
//! (`run`, the friendly subcommands) and the MCP server both dispatch through
//! it, so a tool added to the table appears in both.
//!
//! Conventions shared by every tool:
//! - Pages are **1-based**.
//! - Rectangles are in PDF points, top-left origin, measured on the page as
//!   displayed (render space), the same space the GUI's overlays use.
//! - Edits apply to the in-memory document; only `doc_save` writes it.

mod args;
pub mod cli;
mod handlers;
pub mod mcp;
mod paths;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_cli;
#[cfg(test)]
mod tests_edit;
#[cfg(test)]
mod tests_mcp;
mod tools;

pub use tools::{tools, ToolDef};

use crate::state::{lock_mutex, AppState, DocEntry};
use pdfium_render::prelude::Pdfium;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Progress for a long tool: `(done, total)`, such as pages OCR'd so far. The
/// MCP server turns it into `notifications/progress`; the CLI prints it.
pub type Progress = Arc<dyn Fn(u32, u32) + Send + Sync>;

/// How many documents a session holds open at once. Each keeps its bytes, a
/// pdfium document and a page cache in memory, so a long MCP session that
/// never closes anything would otherwise grow without bound.
const MAX_OPEN_DOCUMENTS: usize = 32;

/// A session: the open documents, and the folder tools are confined to.
///
/// Document ids are small integers (1, 2, ...) issued by `doc_open` and never
/// reused within a session. `AppState` keys documents by string, so id 1 is
/// stored under `"1"`.
pub struct Automation {
    pub(crate) state: AppState,
    root: Option<PathBuf>,
    next_doc: u64,
    /// For the call in progress (see [`Automation::call_with`]).
    progress: Progress,
    cancel: Arc<AtomicBool>,
}

impl Automation {
    /// A session over `state`. Tests build one with fake OCR and linearizer
    /// (`AppState::with_ocr_engine`, `with_linearizer`).
    pub(crate) fn new(state: AppState) -> Self {
        Self { state, root: None, next_doc: 1, progress: no_progress(), cancel: Arc::default() }
    }

    /// The production session: the Windows OCR engine and qpdf.
    pub fn with_pdfium(pdfium: &'static Pdfium) -> Self {
        Self::new(AppState::new(pdfium, None))
    }

    /// Confines every path a tool reads or writes to `root`, an existing
    /// folder. Relative paths are then taken relative to it.
    pub fn with_root(mut self, root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = dunce::canonicalize(root.as_ref())?;
        if !root.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the root must be a folder"));
        }
        self.root = Some(root);
        Ok(self)
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Runs the tool `name` with JSON `args`, an object (`null` means none).
    pub fn call(&mut self, name: &str, args: &Value) -> Result<Vec<Content>, ToolError> {
        self.call_with(name, args, no_progress(), Arc::default())
    }

    /// [`Self::call`], reporting progress for the long tools (OCR,
    /// compression, Expand Margins, text export) and stopping when `cancel` is
    /// set. A cancelled edit changes nothing and fails with "cancelled".
    pub fn call_with(
        &mut self,
        name: &str,
        args: &Value,
        progress: Progress,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Content>, ToolError> {
        let empty = Value::Object(Map::new());
        let args = if args.is_null() { &empty } else { args };
        let def = tools::find(name).ok_or_else(|| ToolError::UnknownTool(name.to_string()))?;
        self.progress = progress;
        self.cancel = cancel;
        let result = def.run(self, args);
        self.progress = no_progress();
        self.cancel = Arc::default();
        result
    }

    /// Writes `bytes` that a front door produces on the session's behalf,
    /// such as the CLI saving a page_render image: confined to the root like a
    /// tool's own output, and atomic. Returns where the file went.
    pub fn write_output(&self, path: &str, bytes: &[u8]) -> Result<PathBuf, ToolError> {
        let target = paths::output_path(self.root(), path)?;
        paths::write_atomic(&target, bytes)?;
        Ok(target)
    }

    pub(crate) fn progress(&self) -> Progress {
        self.progress.clone()
    }

    pub(crate) fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// The `AppState` key of an open document.
    pub(crate) fn key(&self, doc: u64) -> Result<String, ToolError> {
        let key = doc.to_string();
        match self.state.get_document(&key) {
            Ok(_) => Ok(key),
            Err(_) => Err(failed(format!("no open document {doc} (doc_list shows the open ones)"))),
        }
    }

    pub(crate) fn entry(&self, doc: u64) -> Result<Arc<Mutex<DocEntry>>, ToolError> {
        let key = self.key(doc)?;
        Ok(self.state.get_document(&key)?)
    }

    pub(crate) fn page_count(&self, doc: u64) -> Result<u32, ToolError> {
        let entry = self.entry(doc)?;
        let entry = lock_mutex(&entry)?;
        Ok(entry.document.pages().len() as u32)
    }

    /// `page` (1-based) checked against the document's page count.
    pub(crate) fn check_page(&self, doc: u64, page: u32) -> Result<u32, ToolError> {
        let count = self.page_count(doc)?;
        if page == 0 || page > count {
            return Err(ToolError::InvalidArgs(format!(
                "page {page} is out of range: document {doc} has {count} page{}",
                if count == 1 { "" } else { "s" }
            )));
        }
        Ok(page)
    }

    /// Issues the next document id. Ids are never reused within a session.
    fn issue_doc_id(&mut self) -> Result<u64, ToolError> {
        if self.state.document_ids()?.len() >= MAX_OPEN_DOCUMENTS {
            return Err(failed(format!(
                "{MAX_OPEN_DOCUMENTS} documents are already open: close one with doc_close first"
            )));
        }
        let id = self.next_doc;
        self.next_doc += 1;
        Ok(id)
    }

    /// The id of the open document stored at `path`, if any.
    fn doc_at_path(&self, path: &str) -> Result<Option<u64>, ToolError> {
        for key in self.state.document_ids()? {
            let entry = self.state.get_document(&key)?;
            if lock_mutex(&entry)?.file_path == path {
                return Ok(key.parse().ok());
            }
        }
        Ok(None)
    }

    /// Every open document's id, in the order they were opened.
    fn doc_ids(&self) -> Result<Vec<u64>, ToolError> {
        let mut ids: Vec<u64> = self.state.document_ids()?.iter().filter_map(|k| k.parse().ok()).collect();
        ids.sort_unstable();
        Ok(ids)
    }
}

fn no_progress() -> Progress {
    Arc::new(|_, _| {})
}

/// One piece of a tool's result.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// Structured data (MCP: a text block with the JSON, plus `structuredContent`).
    Json(Value),
    /// A PNG image.
    Png { data: Vec<u8>, width: u32, height: u32 },
}

/// Why a tool call failed.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolError {
    /// No tool has this name.
    UnknownTool(String),
    /// The arguments don't match the tool's schema.
    InvalidArgs(String),
    /// The tool ran and failed. The message is for the caller (often an agent)
    /// to read.
    Failed(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::UnknownTool(t) => write!(f, "unknown tool {t:?}"),
            ToolError::InvalidArgs(m) | ToolError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ToolError {}

pub(crate) fn failed(message: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(message.to_string())
}

impl From<crate::error::AppError> for ToolError {
    fn from(e: crate::error::AppError) -> Self {
        failed(e)
    }
}

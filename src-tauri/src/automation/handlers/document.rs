//! doc_open, doc_list, doc_info, doc_close, doc_save.

use super::{json, snake_json, ToolResult};
use crate::automation::args::{DocArgs, DocCloseArgs, DocOpenArgs, DocSaveArgs, NoArgs};
use crate::automation::{failed, paths, Automation, ToolError};
use crate::commands::{conformance, forms, metadata, ocr, save, signature, text};
use crate::error::AppError;
use crate::state::{lock_mutex, DocEntry};
use serde_json::{json, Value};

pub(crate) fn doc_open(auto: &mut Automation, args: DocOpenArgs) -> ToolResult {
    let path = paths::input_path(auto.root(), &args.path)?;
    let path = path.to_string_lossy().into_owned();
    // One document per file, as the GUI enforces on open: Save and the
    // open-elsewhere checks compare stored paths, which are canonical.
    if let Some(existing) = auto.doc_at_path(&path)? {
        return Err(failed(format!("{path} is already open as document {existing}")));
    }
    let entry = DocEntry::load(auto.state.pdfium, &path, args.password.as_deref()).map_err(|e| match e {
        AppError::PasswordRequired => failed(format!("{path} is password-protected: pass its password")),
        AppError::WrongPassword => failed(format!("wrong password for {path}")),
        other => failed(other),
    })?;
    // Only once the file has opened, so a failed open uses up no id.
    let id = auto.issue_doc_id()?;
    auto.state.insert_document(id.to_string(), entry)?;
    let mut summary = summary(auto, id)?;
    summary["pages"] = page_sizes(auto, id)?;
    json(summary)
}

pub(crate) fn doc_list(auto: &mut Automation, _: NoArgs) -> ToolResult {
    let documents = auto.doc_ids()?.into_iter().map(|id| summary(auto, id)).collect::<Result<Vec<_>, _>>()?;
    json(json!({ "documents": documents }))
}

pub(crate) fn doc_info(auto: &mut Automation, args: DocArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let mut info = summary(auto, args.doc)?;
    info["pages"] = page_sizes(auto, args.doc)?;
    info["metadata"] = snake_json(&metadata::get_metadata_impl(&auto.state, key.clone())?)?;
    info["conformance"] = json!(conformance::get_conformance_impl(&auto.state, key.clone())?.declared);
    let signatures = signature::get_signature_info_impl(&auto.state, key.clone())?;
    info["signatures"] = json!({ "count": signatures.count, "status": snake_json(&signatures.status)? });
    info["has_form"] = json!(forms::document_has_form_impl(&auto.state, key.clone())?);
    let (without_text, ocr_only) = text_coverage(auto, args.doc)?;
    info["pages_without_text"] = json!(without_text);
    info["pages_ocr_only"] = json!(ocr_only);
    json(info)
}

pub(crate) fn doc_close(auto: &mut Automation, args: DocCloseArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    // In the GUI this check is a prompt (confirmCloseDirtyTab); headless, the
    // caller must say so explicitly.
    let entry = auto.entry(args.doc)?;
    let dirty = lock_mutex(&entry)?.dirty;
    if dirty && !args.discard_changes {
        return Err(failed(format!(
            "document {} has unsaved changes: save it with doc_save, or pass discard_changes: true",
            args.doc
        )));
    }
    // What the GUI's close_document does.
    auto.state.clear_ocr_cache_for_doc(&key);
    auto.state.remove_document(&key)?;
    json(json!({ "closed": args.doc }))
}

pub(crate) fn doc_save(auto: &mut Automation, args: DocSaveArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let path = match &args.path {
        None => {
            save::save_document_impl(&auto.state, &key)?;
            let entry = auto.entry(args.doc)?;
            let path = lock_mutex(&entry)?.file_path.clone();
            path
        }
        Some(path) => {
            let target = paths::output_path(auto.root(), path)?;
            save::save_document_as_impl(&auto.state, &key, &target.to_string_lossy())?
        }
    };
    let bytes = std::fs::metadata(&path).map_err(|e| failed(format!("{path}: {e}")))?.len();
    json(json!({ "doc": args.doc, "path": path, "bytes": bytes, "dirty": false }))
}

/// The fields every document listing shares.
fn summary(auto: &Automation, doc: u64) -> Result<Value, ToolError> {
    let entry = auto.entry(doc)?;
    let entry = lock_mutex(&entry)?;
    Ok(json!({
        "doc": doc,
        "path": entry.file_path,
        "page_count": entry.document.pages().len(),
        "dirty": entry.dirty,
        "encrypted": entry.protection.is_encrypted(),
        "linearized": entry.linearized,
    }))
}

/// Page sizes in points, as displayed (rotation applied), read from the page
/// tree without loading pages, as `open_document` does.
fn page_sizes(auto: &Automation, doc: u64) -> Result<Value, ToolError> {
    let entry = auto.entry(doc)?;
    let entry = lock_mutex(&entry)?;
    let sizes = entry
        .document
        .pages()
        .page_sizes()
        .map_err(|e| AppError::pdfium("Failed to read page dimensions", e))?;
    Ok(sizes.iter().map(|r| json!({ "width": r.width().value, "height": r.height().value })).collect())
}

/// Pages with no native text layer, split by whether this session's OCR has
/// covered them: `(need OCR, OCR only)`. The same test as `page_text_coverage`,
/// which reports only the counts.
fn text_coverage(auto: &Automation, doc: u64) -> Result<(Vec<u32>, Vec<u32>), ToolError> {
    let key = auto.key(doc)?;
    let cache = auto.state.ocr_cache_handle();
    let entry = auto.entry(doc)?;
    let entry = lock_mutex(&entry)?;
    let (mut without_text, mut ocr_only) = (Vec::new(), Vec::new());
    for (i, page) in entry.document.pages().iter().enumerate() {
        let page_num = i as u32 + 1;
        let native = page.text().map(|t| text::page_text_in_document_order(&t)).unwrap_or_default();
        if !native.trim().is_empty() {
            continue;
        }
        if ocr::cache_get(&cache, &key, page_num).is_some() {
            ocr_only.push(page_num);
        } else {
            without_text.push(page_num);
        }
    }
    Ok((without_text, ocr_only))
}

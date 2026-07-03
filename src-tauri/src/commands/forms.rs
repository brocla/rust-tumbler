//! AcroForm reading + inline editing (issue #2).
//!
//! Discovery and value writes both go through **lopdf on the document buffer**
//! (issue #31 buffer model) — never the file on disk. pdfium renders the
//! AcroForm (it ignores `/XFA`), so filling the AcroForm is what Tumbler
//! displays. On the first value write we drop `/XFA` from the AcroForm, which
//! downgrades a *hybrid* AcroForm+XFA form (the norm for government forms, e.g.
//! IRS f8946) to a pure AcroForm so Acrobat — which otherwise prefers the stale
//! XFA `/datasets` — agrees with what Tumbler wrote.

use crate::commands::text::TextRect;
use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::{Emitter, State};

// AcroForm field flags (PDF 32000-1). Value is 1 << (bit - 1).
const FF_READ_ONLY: i64 = 1 << 0; // bit 1
const FF_MULTILINE: i64 = 1 << 12; // Tx, bit 13
const FF_PUSHBUTTON: i64 = 1 << 16; // Btn, bit 17
const FF_RADIO: i64 = 1 << 15; // Btn, bit 16

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    MultilineText,
    Checkbox,
    Radio,
    Dropdown,
    Unknown,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    /// Stable key = the field's fully-qualified name (`parent.child`). Used by
    /// the frontend and passed straight back to `set_form_field_value`.
    pub id: String,
    /// Leaf name (the field's own `/T`), for display.
    pub name: String,
    pub field_type: FieldType,
    /// Text fields: the current string. Checkbox/radio: the current on-state
    /// name (e.g. `Yes`, `Red`) or `Off`. Dropdown: the selected option.
    pub value: String,
    /// The on-state this specific control turns the field to. Checkbox: its
    /// `/AP` on-state (e.g. `Yes`). Radio: this button's export value — a radio
    /// group emits one `FormField` per button, all sharing `id`, and the button
    /// is selected when `value == export_value`. Empty for text/dropdown.
    pub export_value: String,
    /// Top-left-origin rectangle, matching the convention `TextLayer` consumes.
    pub rect: TextRect,
    pub page: u32,
    /// Dropdown options (empty for other field types).
    pub options: Vec<String>,
    pub read_only: bool,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_form_fields(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
) -> Result<Vec<FormField>, String> {
    get_form_fields_impl(&state, doc_id, page).map_err(String::from)
}

#[tauri::command]
pub fn set_form_field_value(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    field_id: String,
    value: String,
) -> Result<(), String> {
    set_form_field_value_impl(&state, doc_id.clone(), field_id, value).map_err(String::from)?;
    let _ = app.emit(
        "document-dirty-changed",
        crate::commands::save::DirtyChangedPayload { doc_id, dirty: true },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn get_form_fields_impl(
    state: &AppState,
    doc_id: String,
    page: u32,
) -> Result<Vec<FormField>, AppError> {
    let buffer = {
        let entry = state.get_document(&doc_id)?;
        let entry = lock_mutex(&entry)?;
        entry.buffer.clone()
    };
    let doc = Document::load_mem(&buffer)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for form fields", e))?;

    let acroform = match acroform_dict(&doc) {
        Some(d) => d,
        None => return Ok(Vec::new()), // no form at all
    };

    // XFA policy: a *hybrid* form (has a usable /Fields tree) is fillable via
    // its AcroForm; only a dynamic/XFA-only form with no /Fields is unsupported.
    let fields = acroform
        .get(b"Fields")
        .ok()
        .and_then(|o| deref(&doc, o).as_array().ok())
        .cloned()
        .unwrap_or_default();
    if has_xfa(&doc, &acroform) && fields.is_empty() {
        return Err(AppError::Other(
            "This is an XFA (dynamic) form, which Tumbler can't fill.".into(),
        ));
    }

    let widget_page = widget_page_map(&doc);

    let mut out = Vec::new();
    for field_ref in &fields {
        if let Ok(id) = field_ref.as_reference() {
            collect_field(&doc, id, "", None, &widget_page, &mut out);
        }
    }
    out.retain(|f| f.page == page);
    Ok(out)
}

/// Recursively walk one field, emitting `FormField`s for terminal fields on
/// any page. Non-terminal (intermediate) fields have `/Kids` that are
/// themselves fields (they carry their own `/T`); a terminal field's kids, if
/// any, are widget annotations (no `/T`) — e.g. the buttons of a radio group.
fn collect_field(
    doc: &Document,
    field_id: ObjectId,
    parent_fq: &str,
    inherited_ft: Option<&[u8]>,
    widget_page: &HashMap<ObjectId, u32>,
    out: &mut Vec<FormField>,
) {
    let Ok(dict) = doc.get_dictionary(field_id) else {
        return;
    };

    let leaf = dict
        .get(b"T")
        .ok()
        .and_then(|o| deref(doc, o).as_str().ok())
        .map(decode_pdf_string);
    let fq = match (&leaf, parent_fq.is_empty()) {
        (Some(l), true) => l.clone(),
        (Some(l), false) => format!("{parent_fq}.{l}"),
        (None, _) => parent_fq.to_string(),
    };

    let own_ft = dict
        .get(b"FT")
        .ok()
        .and_then(|o| deref(doc, o).as_name().ok().map(|n| n.to_vec()));
    let ft: Option<Vec<u8>> = own_ft.or_else(|| inherited_ft.map(|n| n.to_vec()));

    // Distinguish intermediate fields from terminal ones: recurse only when a
    // kid is itself a field (has /T).
    let kids: Vec<ObjectId> = dict
        .get(b"Kids")
        .ok()
        .and_then(|o| deref(doc, o).as_array().ok())
        .map(|a| a.iter().filter_map(|k| k.as_reference().ok()).collect())
        .unwrap_or_default();
    let kid_is_field = |kid: &ObjectId| {
        doc.get_dictionary(*kid)
            .map(|d| d.has(b"T"))
            .unwrap_or(false)
    };

    if kids.iter().any(kid_is_field) {
        for kid in kids {
            collect_field(doc, kid, &fq, ft.as_deref(), widget_page, out);
        }
        return;
    }

    // Terminal field.
    let read_only = flags(dict) & FF_READ_ONLY != 0;
    let leaf_name = leaf.unwrap_or_else(|| fq.clone());
    let ff = flags(dict);

    // Radio group: one FormField per button widget, each at its own /Rect with
    // its own export value, all sharing the group's fq id and current value.
    if ft.as_deref() == Some(b"Btn") && ff & FF_RADIO != 0 {
        let value = btn_value(doc, dict);
        for kid in &kids {
            let Ok(kd) = doc.get_dictionary(*kid) else { continue };
            let export = ap_on_states(doc, kd).into_iter().next().unwrap_or_default();
            let Some(page) = place(doc, *kid, widget_page) else { continue };
            out.push(FormField {
                id: fq.clone(),
                name: leaf_name.clone(),
                field_type: FieldType::Radio,
                value: value.clone(),
                export_value: export,
                rect: widget_rect(doc, *kid),
                page,
                options: Vec::new(),
                read_only,
            });
        }
        return;
    }

    // Everything else: a single widget (the field itself when merged, else its
    // first kid).
    let widget_id = if dict.has(b"Rect") {
        field_id
    } else {
        kids.first().copied().unwrap_or(field_id)
    };
    let Some(page) = place(doc, widget_id, widget_page) else { return };

    let (field_type, value, export_value, options) = classify(doc, dict, ft.as_deref());
    if field_type == FieldType::Unknown {
        return; // e.g. pushbuttons carry no data — nothing to fill
    }

    out.push(FormField {
        id: fq,
        name: leaf_name,
        field_type,
        value,
        export_value,
        rect: widget_rect(doc, widget_id),
        page,
        options,
        read_only,
    });
}

/// The 1-based page for a widget, via the page `/Annots` map or the `/P`
/// fallback. `None` (page 0) means we couldn't place it — skip rather than guess.
fn place(doc: &Document, widget_id: ObjectId, widget_page: &HashMap<ObjectId, u32>) -> Option<u32> {
    widget_page
        .get(&widget_id)
        .copied()
        .or_else(|| page_from_p(doc, widget_id, widget_page))
        .filter(|p| *p != 0)
}

fn btn_value(doc: &Document, dict: &Dictionary) -> String {
    dict.get(b"V")
        .ok()
        .and_then(|o| deref(doc, o).as_name().ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_else(|| "Off".into())
}

/// Determine type, current value, export value, and dropdown options for a
/// non-radio terminal field.
fn classify(
    doc: &Document,
    dict: &Dictionary,
    ft: Option<&[u8]>,
) -> (FieldType, String, String, Vec<String>) {
    let ff = flags(dict);
    match ft {
        Some(b"Tx") => {
            let ty = if ff & FF_MULTILINE != 0 {
                FieldType::MultilineText
            } else {
                FieldType::Text
            };
            (ty, text_value(doc, dict), String::new(), Vec::new())
        }
        Some(b"Ch") => {
            let opts = dict
                .get(b"Opt")
                .ok()
                .and_then(|o| deref(doc, o).as_array().ok())
                .map(|a| a.iter().map(|o| opt_label(doc, o)).collect())
                .unwrap_or_default();
            (FieldType::Dropdown, text_value(doc, dict), String::new(), opts)
        }
        Some(b"Btn") => {
            if ff & FF_PUSHBUTTON != 0 {
                return (FieldType::Unknown, String::new(), String::new(), Vec::new());
            }
            // Checkbox: its on-state is the single non-Off /AP /N key.
            let export = ap_on_states(doc, dict).into_iter().next().unwrap_or_else(|| "Yes".into());
            (FieldType::Checkbox, btn_value(doc, dict), export, Vec::new())
        }
        _ => (FieldType::Unknown, String::new(), String::new(), Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Writing a value
// ---------------------------------------------------------------------------

fn set_form_field_value_impl(
    state: &AppState,
    doc_id: String,
    field_id: String,
    value: String,
) -> Result<(), AppError> {
    let buffer = {
        let entry = state.get_document(&doc_id)?;
        let entry = lock_mutex(&entry)?;
        entry.buffer.clone()
    };
    let mut doc = Document::load_mem(&buffer)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for form update", e))?;

    let acroform = acroform_dict(&doc)
        .ok_or_else(|| AppError::Other("Document has no form fields".into()))?;
    let top: Vec<Object> = acroform
        .get(b"Fields")
        .ok()
        .and_then(|o| deref(&doc, o).as_array().ok())
        .cloned()
        .unwrap_or_default();

    let target = top
        .iter()
        .filter_map(|f| f.as_reference().ok())
        .find_map(|id| find_field_by_fq(&doc, id, "", &field_id))
        .ok_or_else(|| AppError::Other(format!("Form field not found: {field_id}")))?;

    apply_value(&mut doc, target, &value)?;

    // Tell viewers to regenerate appearances for the changed value, and drop
    // /XFA so a hybrid form no longer shows stale /datasets in Acrobat.
    set_need_appearances(&mut doc);
    strip_xfa(&mut doc);

    // `save_to` rewrites every object under one fresh cross-reference table, so
    // the trailer's `/Prev` (and hybrid `/XRefStm`) pointers into the *original*
    // file are now stale — lopdf itself rejects them on the next `load_mem`
    // ("invalid start value in Prev field"). Drop them so our own output
    // round-trips through a subsequent edit. (Real-world forms like f8946 are
    // incrementally-updated and carry `/Prev`; freshly-authored PDFs don't.)
    doc.trailer.remove(b"Prev");
    doc.trailer.remove(b"XRefStm");

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .map_err(|e| AppError::io("Failed to serialize PDF after form update", e))?;
    state.set_buffer_and_refresh(&doc_id, bytes)?;
    Ok(())
}

/// Locate the terminal field whose fully-qualified name equals `want`.
fn find_field_by_fq(
    doc: &Document,
    field_id: ObjectId,
    parent_fq: &str,
    want: &str,
) -> Option<ObjectId> {
    let dict = doc.get_dictionary(field_id).ok()?;
    let leaf = dict
        .get(b"T")
        .ok()
        .and_then(|o| deref(doc, o).as_str().ok())
        .map(decode_pdf_string);
    let fq = match (&leaf, parent_fq.is_empty()) {
        (Some(l), true) => l.clone(),
        (Some(l), false) => format!("{parent_fq}.{l}"),
        (None, _) => parent_fq.to_string(),
    };

    let kids: Vec<ObjectId> = dict
        .get(b"Kids")
        .ok()
        .and_then(|o| deref(doc, o).as_array().ok())
        .map(|a| a.iter().filter_map(|k| k.as_reference().ok()).collect())
        .unwrap_or_default();
    let has_subfields = kids.iter().any(|k| {
        doc.get_dictionary(*k)
            .map(|d| d.has(b"T"))
            .unwrap_or(false)
    });

    if has_subfields {
        return kids
            .iter()
            .find_map(|k| find_field_by_fq(doc, *k, &fq, want));
    }
    (fq == want).then_some(field_id)
}

/// Write `value` into the terminal field `target`, and — for checkbox/radio —
/// the matching `/AS` appearance state onto the relevant widget(s).
fn apply_value(doc: &mut Document, target: ObjectId, value: &str) -> Result<(), AppError> {
    let ft = inherited_ft(doc, target);
    match ft.as_deref() {
        Some(b"Btn") => {
            let name = Object::Name(value.as_bytes().to_vec());
            let kids: Vec<ObjectId> = doc
                .get_dictionary(target)
                .ok()
                .and_then(|d| d.get(b"Kids").ok().and_then(|o| o.as_array().ok()))
                .map(|a| a.iter().filter_map(|k| k.as_reference().ok()).collect())
                .unwrap_or_default();

            if kids.is_empty() {
                // Merged checkbox widget: set both /V and /AS on the field.
                if let Ok(d) = doc.get_dictionary_mut(target) {
                    d.set("V", name.clone());
                    d.set("AS", name);
                }
            } else {
                // Radio group: /V on the parent, /AS on each kid (the chosen
                // export value, else /Off).
                if let Ok(d) = doc.get_dictionary_mut(target) {
                    d.set("V", name);
                }
                for kid in kids {
                    let on = match doc.get_dictionary(kid) {
                        Ok(d) => ap_on_states(doc, d).iter().any(|s| s == value),
                        Err(_) => false,
                    };
                    if let Ok(d) = doc.get_dictionary_mut(kid) {
                        let state = if on { value } else { "Off" };
                        d.set("AS", Object::Name(state.as_bytes().to_vec()));
                    }
                }
            }
        }
        _ => {
            // Text and choice fields store a text string.
            if let Ok(d) = doc.get_dictionary_mut(target) {
                d.set("V", pdf_text_string(value));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Follow references one hop; on failure return the original object.
fn deref<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    match obj {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(obj),
        _ => obj,
    }
}

fn acroform_dict(doc: &Document) -> Option<Dictionary> {
    let catalog = doc.catalog().ok()?;
    catalog
        .get(b"AcroForm")
        .ok()
        .map(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .cloned()
}

fn has_xfa(doc: &Document, acroform: &Dictionary) -> bool {
    match acroform.get(b"XFA") {
        Ok(o) => !matches!(deref(doc, o), Object::Null),
        Err(_) => false,
    }
}

fn flags(dict: &Dictionary) -> i64 {
    dict.get(b"Ff").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0)
}

fn inherited_ft(doc: &Document, mut id: ObjectId) -> Option<Vec<u8>> {
    loop {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(ft) = dict.get(b"FT").map(|o| deref(doc, o)).and_then(|o| o.as_name()) {
            return Some(ft.to_vec());
        }
        id = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok())?;
    }
}

fn text_value(doc: &Document, dict: &Dictionary) -> String {
    dict.get(b"V")
        .ok()
        .map(|o| deref(doc, o))
        .and_then(|o| o.as_str().ok())
        .map(decode_pdf_string)
        .unwrap_or_default()
}

/// A `/Opt` entry is either a string, or a `[export, display]` pair — use the
/// display (human-readable) label.
fn opt_label(doc: &Document, obj: &Object) -> String {
    match deref(doc, obj) {
        Object::Array(pair) => pair
            .get(1)
            .or_else(|| pair.first())
            .and_then(|o| deref(doc, o).as_str().ok())
            .map(decode_pdf_string)
            .unwrap_or_default(),
        o => o.as_str().map(decode_pdf_string).unwrap_or_default(),
    }
}

/// On-state names from a widget's `/AP /N` appearance dictionary (all keys
/// except `Off`).
fn ap_on_states(doc: &Document, dict: &Dictionary) -> Vec<String> {
    dict.get(b"AP")
        .ok()
        .map(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|ap| ap.get(b"N").ok().map(|o| deref(doc, o)))
        .and_then(|n| n.as_dict().ok())
        .map(|n| {
            n.iter()
                .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                .filter(|k| k != "Off")
                .collect()
        })
        .unwrap_or_default()
}

/// Build a top-left-origin `TextRect` (the convention `TextLayer` consumes)
/// from a widget's `/Rect`, using the page's MediaBox for height and origin.
fn widget_rect(doc: &Document, widget_id: ObjectId) -> TextRect {
    let dict = match doc.get_dictionary(widget_id) {
        Ok(d) => d,
        Err(_) => return TextRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
    };
    let rect: Vec<f32> = dict
        .get(b"Rect")
        .ok()
        .and_then(|o| deref(doc, o).as_array().ok())
        .map(|a| a.iter().map(as_f32).collect())
        .unwrap_or_default();
    if rect.len() != 4 {
        return TextRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };
    }
    let (x0, y0, x1, y1) = (rect[0], rect[1], rect[2], rect[3]);
    let (left, top) = (x0.min(x1), y0.max(y1));

    let (mb_left, mb_bottom, mb_height) = page_metrics(doc, widget_id);
    TextRect {
        x: left - mb_left,
        y: mb_height - (top - mb_bottom),
        width: (x1 - x0).abs(),
        height: (y1 - y0).abs(),
    }
}

/// (MediaBox left, MediaBox bottom, MediaBox height) for the page a widget
/// lives on, resolving the inherited MediaBox up the page tree.
fn page_metrics(doc: &Document, widget_id: ObjectId) -> (f32, f32, f32) {
    let page_id = doc
        .get_dictionary(widget_id)
        .ok()
        .and_then(|d| d.get(b"P").ok().and_then(|o| o.as_reference().ok()));
    let mut cur = page_id;
    while let Some(id) = cur {
        if let Ok(d) = doc.get_dictionary(id) {
            if let Some(mb) = d
                .get(b"MediaBox")
                .ok()
                .and_then(|o| deref(doc, o).as_array().ok())
            {
                let v: Vec<f32> = mb.iter().map(as_f32).collect();
                if v.len() == 4 {
                    return (v[0], v[1], v[3] - v[1]);
                }
            }
            cur = d.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
        } else {
            break;
        }
    }
    (0.0, 0.0, 792.0)
}

/// Map every widget-annotation object id to its 1-based page number, by
/// walking each page's `/Annots`.
fn widget_page_map(doc: &Document) -> HashMap<ObjectId, u32> {
    let mut map = HashMap::new();
    for (pnum, pid) in doc.get_pages() {
        if let Ok(page) = doc.get_dictionary(pid) {
            if let Some(annots) = page
                .get(b"Annots")
                .ok()
                .and_then(|o| deref(doc, o).as_array().ok())
            {
                for a in annots {
                    if let Ok(id) = a.as_reference() {
                        map.insert(id, pnum);
                    }
                }
            }
        }
    }
    map
}

/// Fallback page lookup: resolve a widget's `/P` reference to a page number.
fn page_from_p(
    doc: &Document,
    widget_id: ObjectId,
    widget_page: &HashMap<ObjectId, u32>,
) -> Option<u32> {
    let p = doc
        .get_dictionary(widget_id)
        .ok()?
        .get(b"P")
        .ok()?
        .as_reference()
        .ok()?;
    doc.get_pages()
        .iter()
        .find(|(_, pid)| **pid == p)
        .map(|(n, _)| *n)
        .or_else(|| widget_page.get(&widget_id).copied())
}

fn set_need_appearances(doc: &mut Document) {
    if let Some(id) = acroform_id(doc) {
        if let Ok(d) = doc.get_dictionary_mut(id) {
            d.set("NeedAppearances", Object::Boolean(true));
        }
    }
}

fn strip_xfa(doc: &mut Document) {
    if let Some(id) = acroform_id(doc) {
        if let Ok(d) = doc.get_dictionary_mut(id) {
            d.remove(b"XFA");
        }
    }
}

/// The object id of the AcroForm dictionary, if it is an indirect object
/// (it is in every real document; a direct dict can't be mutated in place).
fn acroform_id(doc: &Document) -> Option<ObjectId> {
    doc.catalog()
        .ok()?
        .get(b"AcroForm")
        .ok()?
        .as_reference()
        .ok()
}

fn as_f32(o: &Object) -> f32 {
    match o {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
    }
}

/// Decode a PDF text string: UTF-16BE if it carries a BOM, else treat the bytes
/// as Latin-1 / PDFDocEncoding (close enough for the ASCII range).
fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Encode a string as a PDF text string (ASCII literal, else UTF-16BE+BOM).
fn pdf_text_string(s: &str) -> Object {
    if s.is_ascii() {
        Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
    } else {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in s.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        Object::String(bytes, StringFormat::Literal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;
    use lopdf::dictionary;

    fn forms_fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/forms/acroform_basic.pdf")
    }

    fn state_with_bytes(bytes: Vec<u8>, path: &str) -> AppState {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let document = pdfium
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .expect("pdfium load");
        state
            .insert_document(
                "doc-1".into(),
                DocEntry {
                    document,
                    file_path: path.into(),
                    buffer: bytes,
                    dirty: false,
                },
            )
            .expect("insert");
        state
    }

    /// A minimal in-memory PDF whose AcroForm carries the given /Fields and,
    /// optionally, an /XFA entry. Used to exercise the hybrid-vs-XFA-only path.
    fn build_form_pdf(fields: Vec<Object>, with_xfa: bool) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let page_id = doc.new_object_id();
        let content = doc.add_object(lopdf::Stream::new(dictionary! {}, b"BT ET".to_vec()));
        doc.set_object(
            page_id,
            dictionary! {
                "Type" => "Page",
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Contents" => content,
            },
        );
        let pages_id = doc.add_object(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        });
        if let Ok(p) = doc.get_dictionary_mut(page_id) {
            p.set("Parent", pages_id);
        }
        let mut af = dictionary! { "Fields" => fields };
        if with_xfa {
            af.set("XFA", Object::String(b"<xfa/>".to_vec(), StringFormat::Literal));
        }
        let acroform_id = doc.add_object(af);
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog", "Pages" => pages_id, "AcroForm" => acroform_id,
        });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize");
        out
    }

    #[test]
    fn discovers_every_field_type_on_the_fixture() {
        let bytes = std::fs::read(forms_fixture()).expect("read fixture");
        let state = state_with_bytes(bytes, "mem.pdf");

        let fields = get_form_fields_impl(&state, "doc-1".into(), 1).expect("get fields");

        let by_id = |id: &str| fields.iter().find(|f| f.id == id);
        assert_eq!(by_id("fullName").unwrap().field_type, FieldType::Text);
        assert_eq!(
            by_id("comments").unwrap().field_type,
            FieldType::MultilineText
        );
        let subscribe = by_id("subscribe").unwrap();
        assert_eq!(subscribe.field_type, FieldType::Checkbox);
        assert_eq!(subscribe.export_value, "Yes");

        let country = by_id("country").expect("country field");
        assert_eq!(country.field_type, FieldType::Dropdown);
        assert_eq!(country.value, "USA");
        assert_eq!(country.options, vec!["USA", "Canada", "Mexico"]);

        // The radio group emits one FormField per button, sharing id "color",
        // each carrying its own export value.
        let mut radios: Vec<_> = fields
            .iter()
            .filter(|f| f.id == "color")
            .map(|f| f.export_value.clone())
            .collect();
        radios.sort();
        assert_eq!(radios, vec!["Blue", "Red"]);
        assert!(fields.iter().filter(|f| f.id == "color").all(|f| f.field_type == FieldType::Radio));
    }

    #[test]
    fn field_rect_is_top_left_origin() {
        let bytes = std::fs::read(forms_fixture()).expect("read fixture");
        let state = state_with_bytes(bytes, "mem.pdf");
        let fields = get_form_fields_impl(&state, "doc-1".into(), 1).expect("fields");
        let full_name = fields.iter().find(|f| f.id == "fullName").unwrap();
        // /Rect [50 700 300 720] on a 792-high page → top-left y = 792 - 720 = 72.
        assert!((full_name.rect.x - 50.0).abs() < 0.5);
        assert!((full_name.rect.y - 72.0).abs() < 0.5);
        assert!((full_name.rect.width - 250.0).abs() < 0.5);
        assert!((full_name.rect.height - 20.0).abs() < 0.5);
    }

    #[test]
    fn xfa_only_form_is_rejected_but_hybrid_is_accepted() {
        // XFA present, no /Fields → dynamic XFA, unsupported.
        let xfa_only = build_form_pdf(Vec::new(), true);
        let state = state_with_bytes(xfa_only, "xfa.pdf");
        let err = get_form_fields_impl(&state, "doc-1".into(), 1).unwrap_err();
        assert!(err.to_string().contains("XFA"), "got: {err}");

        // XFA present *and* a usable /Fields tree → hybrid, fillable.
        let bytes = std::fs::read(forms_fixture()).expect("read fixture");
        let mut doc = Document::load_mem(&bytes).unwrap();
        // Splice an /XFA onto the otherwise-pure fixture to simulate a hybrid.
        let af_id = acroform_id(&doc).unwrap();
        doc.get_dictionary_mut(af_id)
            .unwrap()
            .set("XFA", Object::String(b"<xfa/>".to_vec(), StringFormat::Literal));
        let mut hybrid = Vec::new();
        doc.save_to(&mut hybrid).unwrap();

        let state = state_with_bytes(hybrid, "hybrid.pdf");
        let fields = get_form_fields_impl(&state, "doc-1".into(), 1).expect("hybrid accepted");
        assert!(!fields.is_empty());
    }

    #[test]
    fn set_text_value_persists_in_buffer_goes_dirty_and_strips_xfa() {
        let _guard = crate::test_pdfium_guard();
        // Start from a hybrid (fixture + spliced /XFA) so we can assert the
        // strip, and write to a real temp file so we can assert disk is untouched.
        let base = std::fs::read(forms_fixture()).expect("read fixture");
        let mut doc = Document::load_mem(&base).unwrap();
        let af_id = acroform_id(&doc).unwrap();
        doc.get_dictionary_mut(af_id)
            .unwrap()
            .set("XFA", Object::String(b"<xfa/>".to_vec(), StringFormat::Literal));
        let mut hybrid = Vec::new();
        doc.save_to(&mut hybrid).unwrap();

        let tmp = std::env::temp_dir().join("tumbler_set_form_test.pdf");
        std::fs::write(&tmp, &hybrid).expect("write tmp");
        let path = tmp.to_string_lossy().into_owned();
        let state = state_with_bytes(hybrid, &path);
        let disk_before = std::fs::read(&tmp).expect("read disk");

        set_form_field_value_impl(&state, "doc-1".into(), "fullName".into(), "Ada Lovelace".into())
            .expect("set value");

        // Read back from the buffer via a fresh discovery.
        let fields = get_form_fields_impl(&state, "doc-1".into(), 1).expect("fields");
        let full = fields.iter().find(|f| f.id == "fullName").unwrap();
        assert_eq!(full.value, "Ada Lovelace");

        let entry = state.get_document("doc-1").unwrap();
        let entry = lock_mutex(&entry).unwrap();
        assert!(entry.dirty, "form edit must mark the doc dirty");
        // XFA was stripped by the write.
        let edited = Document::load_mem(&entry.buffer).unwrap();
        let af = acroform_dict(&edited).unwrap();
        assert!(af.get(b"XFA").is_err(), "XFA should be removed after an edit");
        drop(entry);

        assert_eq!(
            std::fs::read(&tmp).expect("read disk"),
            disk_before,
            "form edit must not touch the file until an explicit save"
        );
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn set_checkbox_and_radio_update_value() {
        let _guard = crate::test_pdfium_guard();
        let bytes = std::fs::read(forms_fixture()).expect("read fixture");
        let state = state_with_bytes(bytes, "mem.pdf");

        set_form_field_value_impl(&state, "doc-1".into(), "subscribe".into(), "Yes".into())
            .expect("set checkbox");
        set_form_field_value_impl(&state, "doc-1".into(), "color".into(), "Red".into())
            .expect("set radio");

        let fields = get_form_fields_impl(&state, "doc-1".into(), 1).expect("fields");
        assert_eq!(fields.iter().find(|f| f.id == "subscribe").unwrap().value, "Yes");
        assert_eq!(fields.iter().find(|f| f.id == "color").unwrap().value, "Red");
    }

    /// The real-world hybrid AcroForm+XFA fixture (an IRS form) must be
    /// discoverable — its widgets aren't listed in page `/Annots`, so this
    /// exercises the `/P`-reference page-association fallback.
    #[test]
    fn f8946_hybrid_form_is_discovered() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/forms/f8946.pdf");
        let bytes = std::fs::read(&path).expect("read f8946");
        let state = state_with_bytes(bytes, "f8946.pdf");
        let total: usize = (1..=3)
            .map(|p| get_form_fields_impl(&state, "doc-1".into(), p).expect("fields").len())
            .sum();
        assert!(total > 0, "f8946 hybrid form should yield fields");
    }

    /// Regression: real-world forms (f8946) are incrementally updated and carry
    /// a `/Prev` cross-reference chain. After one edit, lopdf's re-serialized
    /// output must still be parseable by the *next* edit — it wasn't until we
    /// dropped the stale `/Prev`/`/XRefStm` on save.
    #[test]
    fn consecutive_edits_survive_reparse_on_pdf_with_prev_xref() {
        let _guard = crate::test_pdfium_guard();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/forms/f8946.pdf");
        let bytes = std::fs::read(&path).expect("read f8946");
        let state = state_with_bytes(bytes, "f8946.pdf");

        let fid = "topmostSubform[0].Page1[0].phoneNumber[0]";
        set_form_field_value_impl(&state, "doc-1".into(), fid.into(), "555-1234".into())
            .expect("first edit");
        // The second edit reloads the buffer the first produced — the point of
        // failure before the fix.
        set_form_field_value_impl(&state, "doc-1".into(), fid.into(), "555-9999".into())
            .expect("second edit must reparse the once-saved buffer");

        let fields = get_form_fields_impl(&state, "doc-1".into(), 1).expect("fields");
        assert_eq!(fields.iter().find(|f| f.id == fid).unwrap().value, "555-9999");
    }

    #[test]
    fn decode_pdf_string_handles_utf16be_and_ascii() {
        assert_eq!(decode_pdf_string(b"hello"), "hello");
        let utf16 = [0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]; // "Hi"
        assert_eq!(decode_pdf_string(&utf16), "Hi");
    }
}

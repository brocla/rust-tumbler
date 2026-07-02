//! "Save Searchable Copy" — the persisted OCR-layer tier (issue #4).
//!
//! Where "Make Searchable" (see [`crate::commands::ocr`]) recognizes text into a
//! session-only cache, this writes those words into a **new** PDF as an
//! invisible text layer (the "OCR sandwich"): for every previously text-less
//! (scanned) page, the recognized words are grouped into lines (reusing the
//! ephemeral overlay's `ocr_words_to_lines`) and each line is appended to the
//! page's content stream as one run in **text render mode 3 (invisible)** — so
//! a reader's selection/search highlight stays smooth across the line, matching
//! "Make Searchable". The bytes are never painted —
//! they exist purely so the file is searchable, selectable, and copyable in any
//! PDF reader. Afterward Tumbler's own `search_document` / `extract_page_text`
//! need no special-casing: pdfium just sees real text operators.
//!
//! This is always "Save As": the source file is never modified. The write goes
//! to a temp file in the destination directory, then an atomic rename.
//!
//! Coordinate note: `OcrWord.rect` is already in PDF user space (points, origin
//! bottom-left) for the common case of a MediaBox at `[0 0 w h]` with no
//! `/Rotate`. Pages with a shifted origin or a rotation are *detected and
//! skipped* in this first cut rather than mis-positioned (see
//! [`geometry_is_simple`]); the happy path is authored correctly.

use crate::commands::ocr::{
    cache_get, ocr_page_into_cache, ocr_words_to_lines, OcrCache, OcrEngine, OcrProgress, OcrWord,
};
use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

/// Resource name for the invisible-text font. Prefixed to avoid colliding with
/// any font the page already defines.
const FONT_NAME: &str = "TumblerOCR";

/// Loose-bounds metrics that pdfium reports for the non-embedded standard
/// Helvetica we use, as fractions of the font size: `ASCENT` is how far the
/// text-extraction box rises above the baseline, `DESCENT` how far it drops
/// below. We size and place each line from these so its extraction box (and
/// thus the selection/search highlight, which is derived from it) coincides
/// with the OCR box — landing on the scanned text instead of a fraction of a
/// line too low. Pinned empirically by `layer_box_matches_ocr_box`.
const HELVETICA_ASCENT_RATIO: f32 = 0.905;
const HELVETICA_DESCENT_RATIO: f32 = 0.211;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSearchableResult {
    /// Pages that received an invisible OCR text layer.
    pub pages_written: u32,
    /// Text-less pages that were OCR'd but left un-searchable because their
    /// geometry (a `/Rotate` or a shifted MediaBox origin) isn't yet supported
    /// by the layer author. Surfaced so the user is told, rather than silently
    /// seeing a lower count. (Distinct from a scanned page on which OCR simply
    /// recognized no encodable text — a rare case not separately counted here.)
    pub pages_skipped_unsupported_geometry: u32,
    pub cancelled: bool,
}

// ── Content-stream authoring (pure) ─────────────────────────────────────────

/// Advance width of a WinAnsi byte in the standard Helvetica font, in 1000ths
/// of an em (Adobe AFM values). Used to compute a line's true natural width so
/// the horizontal scaling stretches it to exactly the OCR box — the backend
/// equivalent of the frontend text layer's canvas width measurement. Bytes
/// outside the table (rare WinAnsi punctuation) fall back to a mid-range 556.
fn helvetica_width_1000(byte: u8) -> u16 {
    match byte {
        b' ' => 278, b'!' => 278, b'"' => 355, b'#' => 556, b'$' => 556, b'%' => 889,
        b'&' => 667, b'\'' => 191, b'(' => 333, b')' => 333, b'*' => 389, b'+' => 584,
        b',' => 278, b'-' => 333, b'.' => 278, b'/' => 278,
        b'0'..=b'9' => 556,
        b':' => 278, b';' => 278, b'<' => 584, b'=' => 584, b'>' => 584, b'?' => 556,
        b'@' => 1015,
        b'A' => 667, b'B' => 667, b'C' => 722, b'D' => 722, b'E' => 667, b'F' => 611,
        b'G' => 778, b'H' => 722, b'I' => 278, b'J' => 500, b'K' => 667, b'L' => 556,
        b'M' => 833, b'N' => 722, b'O' => 778, b'P' => 667, b'Q' => 778, b'R' => 722,
        b'S' => 667, b'T' => 611, b'U' => 722, b'V' => 667, b'W' => 944, b'X' => 667,
        b'Y' => 667, b'Z' => 611,
        b'[' => 278, b'\\' => 278, b']' => 278, b'^' => 469, b'_' => 556, b'`' => 333,
        b'a' => 556, b'b' => 556, b'c' => 500, b'd' => 556, b'e' => 556, b'f' => 278,
        b'g' => 556, b'h' => 556, b'i' => 222, b'j' => 222, b'k' => 500, b'l' => 222,
        b'm' => 833, b'n' => 556, b'o' => 556, b'p' => 556, b'q' => 556, b'r' => 333,
        b's' => 500, b't' => 278, b'u' => 556, b'v' => 500, b'w' => 722, b'x' => 500,
        b'y' => 500, b'z' => 500,
        b'{' => 334, b'|' => 260, b'}' => 334, b'~' => 584,
        // Common WinAnsi upper range: accented Latin share their base letter's
        // width; the frequent punctuation is given its AFM value.
        0x91 | 0x92 => 222,          // ' '  quoteleft/right
        0x93 | 0x94 => 333,          // " "  quotedbl left/right
        0x95 => 350,                 // •    bullet
        0x96 => 556,                 // –    endash
        0x97 => 1000,                // —    emdash
        0x85 => 1000,                // …    ellipsis
        0xA0 => 278,                 // nbsp
        0xC0..=0xC5 => 667,          // À-Å
        0xC6 => 1000,                // Æ
        0xC7 => 722,                 // Ç
        0xC8..=0xCB => 667,          // È-Ë
        0xCC..=0xCF => 278,          // Ì-Ï
        0xD1 => 722,                 // Ñ
        0xD2..=0xD6 => 778,          // Ò-Ö
        0xD9..=0xDC => 722,          // Ù-Ü
        0xDD => 667,                 // Ý
        0xDF => 611,                 // ß
        0xE0..=0xE5 => 556,          // à-å
        0xE6 => 889,                 // æ
        0xE7 => 500,                 // ç
        0xE8..=0xEB => 556,          // è-ë
        0xEC..=0xEF => 278,          // ì-ï
        0xF1 => 556,                 // ñ
        0xF2..=0xF6 => 556,          // ò-ö
        0xF9..=0xFC => 556,          // ù-ü
        0xFD | 0xFF => 500,          // ý ÿ
        _ => 556,
    }
}

/// A line's natural (unscaled) width in points for the standard Helvetica font.
fn helvetica_natural_width(encoded: &[u8], font_size: f32) -> f32 {
    let sum: u32 = encoded.iter().map(|&b| helvetica_width_1000(b) as u32).sum();
    font_size * sum as f32 / 1000.0
}

/// Horizontal scaling percent (`Tz`) that stretches the (invisible) glyphs to
/// span the OCR box width exactly. The natural width comes from Helvetica's real
/// advance-width table, so the persisted run's on-page extent matches the OCR
/// box — which is what makes the selection/search highlight reach the end of the
/// line (a crude average-width estimate left it short).
fn horizontal_scale_percent(encoded: &[u8], font_size: f32, box_width: f32) -> f32 {
    let natural_width = helvetica_natural_width(encoded, font_size);
    if natural_width <= 0.0 || box_width <= 0.0 {
        return 100.0;
    }
    (box_width / natural_width * 100.0).clamp(1.0, 1000.0)
}

/// Maps a Unicode scalar to its WinAnsi (cp1252) byte, or `None` if it can't be
/// represented. ASCII and Latin-1 map directly; the cp1252-only punctuation in
/// `0x80..=0x9F` (smart quotes, dashes, ellipsis, …) is mapped explicitly.
/// Characters outside WinAnsi (CJK, most non-Latin scripts) are dropped — the
/// documented limitation of the standard-font (Option A) approach.
fn win_ansi_byte(c: char) -> Option<u8> {
    let cp = c as u32;
    match cp {
        0x20..=0x7E | 0xA0..=0xFF => Some(cp as u8),
        0x20AC => Some(0x80),
        0x201A => Some(0x82),
        0x0192 => Some(0x83),
        0x201E => Some(0x84),
        0x2026 => Some(0x85),
        0x2020 => Some(0x86),
        0x2021 => Some(0x87),
        0x02C6 => Some(0x88),
        0x2030 => Some(0x89),
        0x0160 => Some(0x8A),
        0x2039 => Some(0x8B),
        0x0152 => Some(0x8C),
        0x017D => Some(0x8E),
        0x2018 => Some(0x91),
        0x2019 => Some(0x92),
        0x201C => Some(0x93),
        0x201D => Some(0x94),
        0x2022 => Some(0x95),
        0x2013 => Some(0x96),
        0x2014 => Some(0x97),
        0x02DC => Some(0x98),
        0x2122 => Some(0x99),
        0x0161 => Some(0x9A),
        0x203A => Some(0x9B),
        0x0153 => Some(0x9C),
        0x017E => Some(0x9E),
        0x0178 => Some(0x9F),
        _ => None,
    }
}

/// Encodes text for the standard WinAnsi font, dropping unrepresentable chars.
fn encode_for_font(text: &str) -> Vec<u8> {
    text.chars().filter_map(win_ansi_byte).collect()
}

/// Builds the invisible-text content stream for one page's worth of OCR words.
///
/// Words are grouped into visual **lines** with the same [`ocr_words_to_lines`]
/// pass the ephemeral "Make Searchable" overlay uses, and each line is written
/// as **one continuous run** — a single `BT … ET` block in render mode 3, with
/// one font size and one horizontal-scale (`Tz`) stretching the whole line to
/// its box width. Emitting per line (not per word) is what keeps a reader's
/// selection and search highlight smooth across the line, with uniform spacing,
/// instead of jumping between independently-scaled per-word runs. Each `BT…ET`
/// block isolates *text* state; isolation from the page's *graphics* state
/// (a leftover CTM or clip) is handled where this stream is appended — see
/// [`append_content_stream`], which wraps the existing content in `q`/`Q`.
///
/// Font size and baseline are derived from the line box and Helvetica's loose
/// metrics so the run's text-extraction box coincides with the OCR box: with
/// `fs = height / (ascent + descent)` the box height matches, and placing the
/// baseline at `box_bottom + descent·fs` makes the box bottom sit on the OCR
/// box bottom (the descent hangs down to exactly the box bottom, not below it).
///
/// Returns `Ok(vec![])` when no line has representable text (e.g. a pure-CJK
/// page) — a legitimate "nothing to write". An encoding failure is returned as
/// `Err` rather than collapsed into an empty stream, so the caller can't mistake
/// a real error for an empty page and silently drop the layer.
pub fn build_invisible_text_stream(words: &[OcrWord], font_name: &str) -> Result<Vec<u8>, AppError> {
    let mut ops: Vec<Operation> = Vec::new();
    for line in ocr_words_to_lines(words) {
        let encoded = encode_for_font(&line.text);
        if encoded.is_empty() {
            continue; // nothing representable (e.g. a pure-CJK line)
        }
        let box_height = line.rect.height.max(1.0);
        let font_size = box_height / (HELVETICA_ASCENT_RATIO + HELVETICA_DESCENT_RATIO);
        let baseline_y = line.rect.y + HELVETICA_DESCENT_RATIO * font_size;
        let h_scale = horizontal_scale_percent(&encoded, font_size, line.rect.width);

        ops.push(Operation::new("BT", vec![]));
        ops.push(Operation::new(
            "Tf",
            vec![Object::Name(font_name.as_bytes().to_vec()), Object::Real(font_size)],
        ));
        ops.push(Operation::new("Tr", vec![Object::Integer(3)])); // invisible
        ops.push(Operation::new("Tz", vec![Object::Real(h_scale)]));
        ops.push(Operation::new(
            "Td",
            vec![Object::Real(line.rect.x), Object::Real(baseline_y)],
        ));
        ops.push(Operation::new(
            "Tj",
            vec![Object::String(encoded, StringFormat::Literal)],
        ));
        ops.push(Operation::new("ET", vec![]));
    }
    Content { operations: ops }
        .encode()
        .map_err(|e| AppError::lopdf("Failed to encode OCR text content", e))
}

// ── Page geometry ───────────────────────────────────────────────────────────

/// Whether a page's coordinate space matches the one `OcrWord.rect` assumes:
/// MediaBox origin at (0,0) and no rotation. Rotated/offset pages are skipped
/// in this cut so their layer is never mis-placed.
fn geometry_is_simple(origin_x: f32, origin_y: f32, rotate: i64) -> bool {
    origin_x.abs() < 0.5 && origin_y.abs() < 0.5 && rotate.rem_euclid(360) == 0
}

/// Resolves a possibly-inherited page attribute, following `/Parent` up the page
/// tree and dereferencing an indirect value. Returns an owned clone.
fn inherited_value(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    for _ in 0..64 {
        let dict = doc.get_object(current).ok()?.as_dict().ok()?;
        if let Ok(value) = dict.get(key) {
            return Some(match value {
                Object::Reference(r) => doc.get_object(*r).ok()?.clone(),
                other => other.clone(),
            });
        }
        current = dict.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn object_as_f32(obj: &Object) -> f32 {
    match obj {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
    }
}

/// Reads a page's effective (MediaBox origin, /Rotate) for the simple-geometry
/// check. Missing values default to origin (0,0) and rotation 0.
fn page_geometry(doc: &Document, page_id: ObjectId) -> (f32, f32, i64) {
    let (origin_x, origin_y) = match inherited_value(doc, page_id, b"MediaBox") {
        Some(Object::Array(a)) if a.len() >= 2 => (object_as_f32(&a[0]), object_as_f32(&a[1])),
        _ => (0.0, 0.0),
    };
    let rotate = match inherited_value(doc, page_id, b"Rotate") {
        Some(Object::Integer(i)) => i,
        _ => 0,
    };
    (origin_x, origin_y, rotate)
}

/// Builds the page's Resources dictionary with our OCR font added, preserving
/// every existing resource (images, other fonts) whether the page owns its
/// `/Resources` or inherits them. Returned owned so it can be set after the
/// mutable page borrow begins.
fn merged_resources_with_font(
    doc: &Document,
    page_id: ObjectId,
    font_name: &str,
    font_id: ObjectId,
) -> Dictionary {
    let mut resources = match inherited_value(doc, page_id, b"Resources") {
        Some(Object::Dictionary(d)) => d,
        _ => Dictionary::new(),
    };
    let mut fonts = match resources.get(b"Font") {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(r)) => doc
            .get_object(*r)
            .ok()
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default(),
        _ => Dictionary::new(),
    };
    fonts.set(font_name, Object::Reference(font_id));
    resources.set("Font", Object::Dictionary(fonts));
    resources
}

/// The stream references in a page's `/Contents`, normalized across its possible
/// shapes (single `Reference`, `Array` of references, or missing → empty).
fn contents_refs(doc: &Document, page_id: ObjectId) -> Vec<ObjectId> {
    let Some(page) = doc.get_object(page_id).ok().and_then(|o| o.as_dict().ok()) else {
        return Vec::new();
    };
    match page.get(b"Contents") {
        Ok(Object::Reference(r)) => vec![*r],
        Ok(Object::Array(a)) => a.iter().filter_map(|o| o.as_reference().ok()).collect(),
        _ => Vec::new(),
    }
}

/// Appends our invisible-text stream to a page's `/Contents`, bracketing the
/// existing content in a `q … Q` pair first.
///
/// PDF concatenates the content streams into one, so without the wrap our text
/// would inherit any graphics state the page's content left in effect — a
/// leftover CTM (`cm`) or an open clip, both common in real scans, would shift
/// or clip the layer. The `q` saves the page-default state at the top; the `Q`
/// restores it after the existing content; our text then runs from a clean
/// default CTM. (A single wrap can't neutralize *pathological* content that sets
/// a top-level `cm` and then an unbalanced `q` with no `Q`; that's rare and
/// matches what ocrmypdf/pikepdf do.)
///
/// Guard/text stream objects are added by the caller (needs `&mut Document`) and
/// passed in as ids so this only edits the page dictionary. When the page has no
/// existing content there's nothing to reset, so `/Contents` is set to our
/// stream alone and the guard ids are ignored.
fn append_content_stream(
    page: &mut Dictionary,
    existing: &[ObjectId],
    save_id: ObjectId,
    restore_id: ObjectId,
    text_id: ObjectId,
) {
    if existing.is_empty() {
        page.set("Contents", Object::Reference(text_id));
        return;
    }
    let mut refs = Vec::with_capacity(existing.len() + 3);
    refs.push(Object::Reference(save_id));
    refs.extend(existing.iter().map(|id| Object::Reference(*id)));
    refs.push(Object::Reference(restore_id));
    refs.push(Object::Reference(text_id));
    page.set("Contents", Object::Array(refs));
}

// ── Command ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn save_searchable_copy(
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
) -> Result<SaveSearchableResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let engine = state.ocr_engine.clone();
    let cache = state.ocr_cache_handle();
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_ocr_job(cancel.clone());

    // Same shared `ocr-progress` channel the progress overlay already listens on.
    let emit = move |page, total| {
        let _ = window.emit("ocr-progress", OcrProgress { page, total });
    };

    let result = tauri::async_runtime::spawn_blocking(move || {
        save_searchable_copy_impl(emit, entry, doc_id, dest_path, engine, cache, cancel)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_ocr_job();
    result?.map_err(String::from)
}

#[allow(clippy::too_many_arguments)]
fn save_searchable_copy_impl(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    dest_path: String,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
) -> Result<SaveSearchableResult, AppError> {
    let (file_path, page_count) = {
        let entry = lock_mutex(&entry)?;
        (entry.file_path.clone(), entry.document.pages().len() as u32)
    };

    // Phase A (pdfium): ensure every text-less page is OCR'd into the cache, and
    // remember which pages are text-less — those are the only ones that get a
    // layer (native-text pages are already searchable; adding text would
    // duplicate and confuse selection).
    let mut textless_pages: Vec<u32> = Vec::new();
    for i in 0..page_count {
        let page_num = i + 1;

        if cancel.load(Ordering::Relaxed) {
            return Ok(SaveSearchableResult {
                pages_written: 0,
                pages_skipped_unsupported_geometry: 0,
                cancelled: true,
            });
        }
        emit_progress(page_num, page_count);

        let native_empty = {
            let entry = lock_mutex(&entry)?;
            let page = entry
                .document
                .pages()
                .get(i as i32)
                .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
            page.text()
                .map(|t| t.all())
                .unwrap_or_default()
                .trim()
                .is_empty()
        };
        if native_empty {
            ocr_page_into_cache(&entry, &doc_id, page_num, &engine, &cache)?;
            textless_pages.push(page_num);
        }
    }

    // Phase B (lopdf): author the invisible text into a fresh copy read from
    // disk. pdfium's handle is never used for writing. Only parse/rewrite the
    // PDF when at least one page actually needs a layer — see the write step.
    let mut pages_written = 0u32;
    let mut pages_skipped_unsupported_geometry = 0u32;
    let mut doc: Option<Document> = None;

    if !textless_pages.is_empty() {
        let mut d = Document::load(&file_path)
            .map_err(|e| AppError::lopdf("Failed to open PDF for searchable copy", e))?;
        let pages = d.get_pages();
        // Add the shared font object lazily — only when a page first needs it.
        let mut font_id: Option<ObjectId> = None;

        for page_num in textless_pages {
            let Some(words) = cache_get(&cache, &doc_id, page_num) else {
                continue;
            };
            let Some(&page_id) = pages.get(&page_num) else {
                continue;
            };

            // Skip pages whose coordinate space doesn't match what OcrWord.rect
            // assumes; better no layer than a mis-placed one. Count them so the
            // user is told these pages were left un-searchable.
            let (ox, oy, rotate) = page_geometry(&d, page_id);
            if !geometry_is_simple(ox, oy, rotate) {
                pages_skipped_unsupported_geometry += 1;
                continue;
            }

            let stream_bytes = build_invisible_text_stream(&words, FONT_NAME)?;
            if stream_bytes.is_empty() {
                continue; // genuinely no representable text — not an error
            }

            let fid = *font_id.get_or_insert_with(|| {
                d.add_object(dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Helvetica",
                    "Encoding" => "WinAnsiEncoding",
                })
            });
            let resources = merged_resources_with_font(&d, page_id, FONT_NAME, fid);

            // Everything that needs `&mut Document` is created before the page
            // borrow: our text stream, and (per page) the `q`/`Q` guard streams
            // that reset the graphics state around the existing content so our
            // layer isn't shifted/clipped by a leftover CTM or clip.
            let existing = contents_refs(&d, page_id);
            let stream_id = d.add_object(Object::Stream(Stream::new(Dictionary::new(), stream_bytes)));
            let (save_id, restore_id) = if existing.is_empty() {
                (stream_id, stream_id) // unused when there's no content to wrap
            } else {
                (
                    d.add_object(Object::Stream(Stream::new(Dictionary::new(), b"q\n".to_vec()))),
                    d.add_object(Object::Stream(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()))),
                )
            };

            {
                let page = d
                    .get_object_mut(page_id)
                    .map_err(|e| AppError::lopdf(format!("Failed to get page {page_num}"), e))?
                    .as_dict_mut()
                    .map_err(|e| AppError::lopdf(format!("Page {page_num} is not a dictionary"), e))?;
                append_content_stream(page, &existing, save_id, restore_id, stream_id);
                page.set("Resources", Object::Dictionary(resources));
            }
            pages_written += 1;
        }
        doc = Some(d);
    }

    // Write to a temp file in the destination dir, then atomic rename, so a
    // crash or disk-full can't leave a truncated file at dest_path. When we
    // authored a layer, serialize the modified document; otherwise copy the
    // source bytes verbatim — an unchanged copy should be byte-identical, since
    // lopdf re-serialization would reorder objects and could drop structures it
    // doesn't model.
    let tmp_path = format!("{dest_path}.tmp-{}", uuid::Uuid::new_v4());
    let write_result = match doc.as_mut() {
        Some(d) if pages_written > 0 => d
            .save(&tmp_path)
            .map(|_| ())
            .map_err(|e| AppError::io("Failed to save searchable copy", e)),
        _ => std::fs::copy(&file_path, &tmp_path)
            .map(|_| ())
            .map_err(|e| AppError::io("Failed to copy source PDF", e)),
    };
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    std::fs::rename(&tmp_path, &dest_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::io("Failed to move searchable copy into place", e)
    })?;

    Ok(SaveSearchableResult {
        pages_written,
        pages_skipped_unsupported_geometry,
        cancelled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::text::TextRect;
    use crate::state::DocEntry;
    use std::sync::atomic::AtomicBool;

    /// OCR engine that returns fixed pixel-space words (mapped to PDF points by
    /// `ocr_page_into_cache`, exactly like the real engine).
    struct FakeOcrEngine {
        words: Vec<OcrWord>,
    }
    impl OcrEngine for FakeOcrEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            Ok(self.words.clone())
        }
    }

    /// A word already in PDF user space (origin bottom-left), as the cache holds.
    fn pt_word(text: &str, x: f32, y: f32, w: f32, h: f32) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect { x, y, width: w, height: h },
        }
    }

    /// A pixel-space word as an OCR engine reports it (top-left origin).
    fn px_word(text: &str) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect { x: 40.0, y: 40.0, width: 120.0, height: 40.0 },
        }
    }

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("{}-{}", uuid::Uuid::new_v4(), name))
            .to_string_lossy()
            .into_owned()
    }

    /// Hand-writes a minimal one-page (200×200) PDF to `path` whose page content
    /// stream is `content`, so both pdfium and lopdf can load it. Passing a
    /// non-trivial content stream lets a test seed a *dirty graphics state* — a
    /// leftover CTM, an open clip, or an unbalanced `q` — to prove the appended
    /// OCR layer isn't affected by it. Empty content = a plain scanned-page
    /// stand-in.
    fn write_pdf_with_content(path: &str, content: &[u8]) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let contents_id = doc.add_object(Stream::new(Dictionary::new(), content.to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents_id,
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(200), Object::Integer(200),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).expect("write pdf");
    }

    /// A blank (no-content) one-page PDF — a plain scanned-page stand-in.
    fn write_blank_pdf(path: &str) {
        write_pdf_with_content(path, b"");
    }

    /// Saves a searchable copy of a one-page PDF whose page content is
    /// `page_content`, seeded with a single OCR `word`, then returns the unioned
    /// loose bounds `(left, bottom, right, top)` pdfium reports for the authored
    /// layer. Used to assert the layer lands on the OCR box regardless of the
    /// page's pre-existing graphics state. The caller must hold
    /// `test_pdfium_guard()`.
    fn saved_layer_loose_bounds(page_content: &[u8], word: OcrWord) -> (f32, f32, f32, f32) {
        let pdfium = crate::test_pdfium();
        let src = temp_path("src.pdf");
        write_pdf_with_content(&src, page_content);
        let dest = temp_path("out.pdf");

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        // Seed the cache directly so the rect is exactly `word.rect`.
        state.set_ocr_words("doc1", 1, vec![word]);
        let document = pdfium.load_pdf_from_file(&src, None).expect("load src");
        state
            .insert_document("doc1".to_string(), DocEntry { document, file_path: src.clone() })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save");

        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("reopen");
        let page = reopened.pages().get(0).expect("page");
        let text = page.text().expect("text");
        let (mut left, mut bottom, mut right, mut top) =
            (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for ch in text.chars().iter() {
            if let Ok(b) = ch.loose_bounds() {
                left = left.min(b.left().value);
                bottom = bottom.min(b.bottom().value);
                right = right.max(b.right().value);
                top = top.max(b.top().value);
            }
        }
        drop(reopened);
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
        (left, bottom, right, top)
    }

    // ── Pure builder / helpers ──────────────────────────────────────────────

    #[test]
    fn builds_invisible_text_ops() {
        let bytes = build_invisible_text_stream(&[pt_word("Hello", 10.0, 100.0, 60.0, 12.0)], FONT_NAME)
            .expect("encode");
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("3 Tr"), "missing invisible render mode: {s}");
        assert!(s.contains("BT") && s.contains("ET"), "missing BT/ET: {s}");
        assert!(s.contains("Hello"), "missing word text: {s}");
        assert!(s.contains("/TumblerOCR"), "missing font ref: {s}");
    }

    #[test]
    fn empty_words_produce_empty_stream() {
        assert!(build_invisible_text_stream(&[], FONT_NAME).expect("encode").is_empty());
    }

    /// Words sharing a baseline become a single continuous line run (one BT…ET,
    /// text joined with spaces), not one run per word — this is what preserves
    /// the smooth, uniform highlighting of "Make Searchable".
    #[test]
    fn words_on_one_line_form_a_single_run() {
        let words = vec![
            pt_word("Hello", 10.0, 100.0, 30.0, 12.0),
            pt_word("World", 50.0, 100.0, 30.0, 12.0),
        ];
        let bytes = build_invisible_text_stream(&words, FONT_NAME).expect("encode");
        let content = Content::decode(&bytes).expect("decode content");
        let runs = content
            .operations
            .iter()
            .filter(|op| op.operator == "BT")
            .count();
        assert_eq!(runs, 1, "two words on one line should be one run, got {runs}");
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("Hello World"), "line text should be joined: {s}");
    }

    #[test]
    fn unrepresentable_only_word_is_skipped() {
        // A pure-CJK token has no WinAnsi bytes, so it contributes nothing.
        let bytes = build_invisible_text_stream(&[pt_word("日本語", 0.0, 0.0, 30.0, 10.0)], FONT_NAME)
            .expect("encode");
        assert!(bytes.is_empty(), "CJK-only word should be dropped under WinAnsi");
    }

    #[test]
    fn geometry_simple_only_for_unrotated_origin_zero() {
        assert!(geometry_is_simple(0.0, 0.0, 0));
        assert!(geometry_is_simple(0.0, 0.0, 360));
        assert!(!geometry_is_simple(10.0, 0.0, 0), "offset origin is not simple");
        assert!(!geometry_is_simple(0.0, 0.0, 90), "rotation is not simple");
        assert!(!geometry_is_simple(0.0, 0.0, 270));
    }

    // ── B9: Helvetica width table / horizontal scaling ──────────────────────

    /// Spot-check the AFM advance widths for common glyphs, so a typo in the
    /// table is caught directly rather than only via an integration string. The
    /// last two pin the behavior of bytes outside the explicit arms (0x7F → the
    /// 556 fallback; 0xFF → its table entry).
    #[test]
    fn helvetica_widths_match_afm() {
        assert_eq!(helvetica_width_1000(b' '), 278);
        assert_eq!(helvetica_width_1000(b'i'), 222);
        assert_eq!(helvetica_width_1000(b'M'), 833);
        assert_eq!(helvetica_width_1000(b'W'), 944);
        assert_eq!(helvetica_width_1000(b'0'), 556);
        assert_eq!(helvetica_width_1000(b'.'), 278);
        assert_eq!(helvetica_width_1000(0xFF), 500); // ÿ
        assert_eq!(helvetica_width_1000(0x7F), 556); // DEL → fallback
    }

    /// The natural width is the summed advances scaled by the font size:
    /// "Test" = T611 + e556 + s500 + t278 = 1945 units → 19.45 pt at fs 10.
    #[test]
    fn helvetica_natural_width_sums_advances() {
        let w = helvetica_natural_width(b"Test", 10.0);
        assert!((w - 19.45).abs() < 1e-4, "unexpected natural width: {w}");
        // Spaces are counted too: "A A" = 667 + 278 + 667 = 1612 → 16.12 pt.
        let w2 = helvetica_natural_width(b"A A", 10.0);
        assert!((w2 - 16.12).abs() < 1e-4, "spaces not counted: {w2}");
    }

    /// `Tz` scales the run so its natural width fills the box. "II" is 2×278 =
    /// 556 units → 5.56 pt at fs 10; a 11.12 pt box needs 200%.
    #[test]
    fn horizontal_scale_fits_box_width() {
        let tz = horizontal_scale_percent(b"II", 10.0, 11.12);
        assert!((tz - 200.0).abs() < 0.5, "unexpected Tz: {tz}");
        // Degenerate inputs fall back to 100% rather than dividing by zero.
        assert_eq!(horizontal_scale_percent(b"II", 10.0, 0.0), 100.0);
        assert_eq!(horizontal_scale_percent(b"", 10.0, 50.0), 100.0);
        // A wildly oversized box is clamped to the 1000% ceiling.
        assert_eq!(horizontal_scale_percent(b"I", 10.0, 1.0e6), 1000.0);
    }

    // ── B8: /Contents normalization and the q/Q wrap shapes ─────────────────

    #[test]
    fn contents_refs_handles_reference_array_and_missing() {
        let mut doc = Document::with_version("1.5");
        let s1 = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        let s2 = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));

        // Single reference.
        let single = doc.add_object(dictionary! { "Type" => "Page", "Contents" => s1 });
        assert_eq!(contents_refs(&doc, single), vec![s1]);

        // Array of references (order preserved).
        let array = doc.add_object(dictionary! {
            "Type" => "Page",
            "Contents" => vec![Object::Reference(s1), Object::Reference(s2)],
        });
        assert_eq!(contents_refs(&doc, array), vec![s1, s2]);

        // No /Contents key at all.
        let missing = doc.add_object(dictionary! { "Type" => "Page" });
        assert!(contents_refs(&doc, missing).is_empty());
    }

    #[test]
    fn append_content_stream_sets_lone_reference_when_page_had_no_content() {
        let mut page = Dictionary::new();
        append_content_stream(&mut page, &[], (5, 0), (6, 0), (7, 0));
        match page.get(b"Contents") {
            Ok(Object::Reference(r)) => assert_eq!(*r, (7, 0)),
            other => panic!("expected a lone text reference, got {other:?}"),
        }
    }

    #[test]
    fn append_content_stream_brackets_existing_content_with_q_q() {
        let mut page = Dictionary::new();
        // existing = [1, 2]; guards save=5 restore=6; text=7.
        append_content_stream(&mut page, &[(1, 0), (2, 0)], (5, 0), (6, 0), (7, 0));
        match page.get(b"Contents") {
            Ok(Object::Array(a)) => {
                let ids: Vec<ObjectId> =
                    a.iter().map(|o| o.as_reference().expect("ref")).collect();
                // q, then existing content, then Q, then our text — so our text
                // runs from the page-default graphics state.
                assert_eq!(ids, vec![(5, 0), (1, 0), (2, 0), (6, 0), (7, 0)]);
            }
            other => panic!("expected a wrapped array, got {other:?}"),
        }
    }

    // ── B7: resource merge preserves the page's existing resources ──────────

    fn font_id(doc: &mut Document, base: &str) -> ObjectId {
        doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => base,
        })
    }

    /// A page owning `/Resources` with an image XObject and a font: after the
    /// merge, both survive and our OCR font is added alongside — proving we
    /// don't blank the page's own resources (which would drop the scanned image).
    #[test]
    fn merged_resources_preserves_owned_xobject_and_font() {
        let mut doc = Document::with_version("1.5");
        let img = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        let existing_font = font_id(&mut doc, "Courier");
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im0" => Object::Reference(img) },
                "Font" => dictionary! { "F1" => Object::Reference(existing_font) },
            },
        });
        let our_font = font_id(&mut doc, "Helvetica");

        let res = merged_resources_with_font(&doc, page_id, FONT_NAME, our_font);

        let xobject = res.get(b"XObject").unwrap().as_dict().unwrap();
        assert_eq!(xobject.get(b"Im0").unwrap().as_reference().unwrap(), img);
        let fonts = res.get(b"Font").unwrap().as_dict().unwrap();
        assert_eq!(fonts.get(b"F1").unwrap().as_reference().unwrap(), existing_font);
        assert_eq!(fonts.get(FONT_NAME.as_bytes()).unwrap().as_reference().unwrap(), our_font);
    }

    /// When `/Resources` is inherited from the parent `/Pages` node (page has
    /// none of its own), the merge still resolves and preserves them.
    #[test]
    fn merged_resources_uses_inherited_resources() {
        let mut doc = Document::with_version("1.5");
        let img = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
                "Resources" => dictionary! {
                    "XObject" => dictionary! { "Im0" => Object::Reference(img) },
                },
            }),
        );
        let our_font = font_id(&mut doc, "Helvetica");

        let res = merged_resources_with_font(&doc, page_id, FONT_NAME, our_font);

        let xobject = res.get(b"XObject").unwrap().as_dict().unwrap();
        assert_eq!(xobject.get(b"Im0").unwrap().as_reference().unwrap(), img);
        let fonts = res.get(b"Font").unwrap().as_dict().unwrap();
        assert_eq!(fonts.get(FONT_NAME.as_bytes()).unwrap().as_reference().unwrap(), our_font);
    }

    /// `/Font` given as an indirect reference (not an inline dict) is resolved,
    /// cloned, and extended with our font.
    #[test]
    fn merged_resources_handles_font_subdict_by_reference() {
        let mut doc = Document::with_version("1.5");
        let existing_font = font_id(&mut doc, "Courier");
        let font_subdict =
            doc.add_object(Object::Dictionary(dictionary! { "F1" => Object::Reference(existing_font) }));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Resources" => dictionary! { "Font" => Object::Reference(font_subdict) },
        });
        let our_font = font_id(&mut doc, "Helvetica");

        let res = merged_resources_with_font(&doc, page_id, FONT_NAME, our_font);

        let fonts = res.get(b"Font").unwrap().as_dict().unwrap();
        assert_eq!(fonts.get(b"F1").unwrap().as_reference().unwrap(), existing_font);
        assert_eq!(fonts.get(FONT_NAME.as_bytes()).unwrap().as_reference().unwrap(), our_font);
    }

    // ── The decisive round-trip: searchable in any reader ───────────────────

    /// A blank (scanned-style) page + cached OCR words → save copy → reopen the
    /// output with pdfium → pdfium's **native** text API returns the words. This
    /// proves the layer is real text operators, not a Tumbler-only overlay.
    #[test]
    fn saved_copy_is_natively_searchable() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let dest = temp_path("out.pdf");

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![px_word("Scanned")] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());

        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { document, file_path: src.clone() },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save searchable");

        assert_eq!(result.pages_written, 1);
        assert!(!result.cancelled);

        // Reopen the saved copy and read text through pdfium's native API.
        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("reopen copy");
        let text = reopened
            .pages()
            .get(0)
            .expect("page 0")
            .text()
            .expect("text")
            .all();
        assert!(
            text.contains("Scanned"),
            "native pdfium text should contain the OCR word, got: {text:?}"
        );

        drop(reopened);
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// The saved layer's text-extraction (loose) box — which drives the
    /// selection/search highlight — must coincide with the OCR box, so the
    /// highlight sits on the scanned text rather than a fraction of a line low.
    /// Seeded rect: bottom y=100, height=20 → OCR box spans y 100..120.
    #[test]
    fn layer_box_matches_ocr_box() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let dest = temp_path("out.pdf");

        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        // Seed the cache directly so the rect is exactly the one above (no
        // pixel→point mapping in the way).
        state.set_ocr_words("doc1", 1, vec![word]);
        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document("doc1".to_string(), DocEntry { document, file_path: src.clone() })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save");

        // Union the loose bounds of every character in the run.
        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("reopen");
        let page = reopened.pages().get(0).expect("page");
        let text = page.text().expect("text");
        let mut bottom = f32::INFINITY;
        let mut top = f32::NEG_INFINITY;
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        let mut fonts = std::collections::HashSet::new();
        for ch in text.chars().iter() {
            fonts.insert(ch.font_name());
            if let Ok(b) = ch.loose_bounds() {
                bottom = bottom.min(b.bottom().value);
                top = top.max(b.top().value);
                left = left.min(b.left().value);
                right = right.max(b.right().value);
            }
        }

        // Font is exactly the standard Helvetica we declared (metrics assumed).
        assert!(fonts.contains("Helvetica"), "unexpected font(s): {fonts:?}");
        // Highlight box aligns with the OCR box within a fraction of a point.
        assert!(
            (bottom - 100.0).abs() < 1.5,
            "layer bottom {bottom} should align to OCR box bottom 100"
        );
        assert!(
            (top - 120.0).abs() < 1.5,
            "layer top {top} should align to OCR box top 120"
        );
        // And the run spans the full OCR box width (x 30..150) — the highlight
        // reaches the end of the line rather than falling short.
        assert!((left - 30.0).abs() < 4.0, "layer left {left} should align to OCR box left 30");
        assert!((right - 150.0).abs() < 4.0, "layer right {right} should reach OCR box right 150");

        drop(reopened);
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    // ── B1: appended layer must not inherit the page's leftover graphics state ─
    //
    // These pages leave a dirty CTM in effect at the end of their content (a
    // bare `cm`, and an unbalanced `q`+`cm` with no `Q`), which real scans do.
    // The appended OCR run is concatenated after that content, so today it
    // inherits the transform and the extraction box drifts/scales away from the
    // OCR box. Both assert the layer lands on the OCR box (x 30..150, y
    // 100..120) and therefore FAIL until the append wraps existing content in
    // `q`/`Q`. (A leftover clip would likewise clip the *rendered* highlight;
    // the same q/Q wrap fixes it, but clipping doesn't affect pdfium's text
    // extraction, so it can't be surfaced through these bounds-based checks.)

    /// A leftover translation CTM (bare `cm`, never restored) must not shift the
    /// appended invisible text. FAILS until B1 is fixed.
    #[test]
    fn layer_ignores_leftover_translate_ctm() {
        let _guard = crate::test_pdfium_guard();
        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };
        // Translate the CTM by (+100, +40) and never restore it.
        let (left, bottom, right, top) = saved_layer_loose_bounds(b"1 0 0 1 100 40 cm\n", word);

        assert!((left - 30.0).abs() < 4.0, "left {left} drifted (leftover CTM not reset)");
        assert!((right - 150.0).abs() < 4.0, "right {right} drifted (leftover CTM not reset)");
        assert!((bottom - 100.0).abs() < 1.5, "bottom {bottom} drifted (leftover CTM not reset)");
        assert!((top - 120.0).abs() < 1.5, "top {top} drifted (leftover CTM not reset)");
    }

    /// An unbalanced `q` that applies a 2× scale and is never popped must not
    /// scale/shift the appended invisible text. FAILS until B1 is fixed.
    #[test]
    fn layer_ignores_unbalanced_q_scale_ctm() {
        let _guard = crate::test_pdfium_guard();
        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };
        // Push graphics state and scale 2×, with no matching `Q`.
        let (left, bottom, right, top) = saved_layer_loose_bounds(b"q 2 0 0 2 0 0 cm\n", word);

        assert!((left - 30.0).abs() < 4.0, "left {left} scaled/drifted (leftover state not reset)");
        assert!((right - 150.0).abs() < 4.0, "right {right} scaled/drifted (leftover state not reset)");
        assert!((bottom - 100.0).abs() < 1.5, "bottom {bottom} scaled/drifted (leftover state not reset)");
        assert!((top - 120.0).abs() < 1.5, "top {top} scaled/drifted (leftover state not reset)");
    }

    /// A page whose `/Contents` is already an Array (multiple content streams,
    /// the second leaving a translated CTM) is wrapped correctly: our layer is
    /// appended after a `Q` that resets the state, so it lands on the OCR box.
    /// Exercises the Array branch of `append_content_stream`.
    #[test]
    fn layer_wraps_array_contents_and_ignores_ctm() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        // Build a page whose Contents is an ARRAY of two streams; the second
        // leaves a +80,+30 translation in effect.
        let src = temp_path("src.pdf");
        {
            let mut doc = Document::with_version("1.5");
            let pages_id = doc.new_object_id();
            let c0 = doc.add_object(Stream::new(Dictionary::new(), b"% first stream\n".to_vec()));
            let c1 = doc.add_object(Stream::new(Dictionary::new(), b"1 0 0 1 80 30 cm\n".to_vec()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => vec![Object::Reference(c0), Object::Reference(c1)],
                "MediaBox" => vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(200), Object::Integer(200),
                ],
            });
            doc.objects.insert(
                pages_id,
                Object::Dictionary(dictionary! {
                    "Type" => "Pages",
                    "Kids" => vec![Object::Reference(page_id)],
                    "Count" => Object::Integer(1),
                }),
            );
            let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
            doc.trailer.set("Root", catalog_id);
            doc.save(&src).expect("write array-contents pdf");
        }
        let dest = temp_path("out.pdf");

        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        state.set_ocr_words("doc1", 1, vec![word]);
        let document = pdfium.load_pdf_from_file(&src, None).expect("load src");
        state
            .insert_document("doc1".to_string(), DocEntry { document, file_path: src.clone() })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save");
        assert_eq!(result.pages_written, 1);

        let reopened = pdfium.load_pdf_from_file(&dest, None).expect("reopen");
        let page = reopened.pages().get(0).expect("page");
        let text = page.text().expect("text");
        let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);
        for ch in text.chars().iter() {
            if let Ok(b) = ch.loose_bounds() {
                left = left.min(b.left().value);
                right = right.max(b.right().value);
            }
        }
        assert!((left - 30.0).abs() < 4.0, "left {left} — array contents not wrapped");
        assert!((right - 150.0).abs() < 4.0, "right {right} — array contents not wrapped");

        drop(reopened);
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// A document with one plain page and one offset-origin page: both are
    /// text-less and OCR'd in Phase A, but the offset page fails the geometry
    /// guard in Phase B. The result must report it (pages_skipped) rather than
    /// silently drop it, so the UI can tell the user. Offset origin is used
    /// instead of /Rotate because it trips the same guard without needing
    /// pdfium to render a rotated page.
    #[test]
    fn offset_page_is_counted_as_skipped() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        {
            let mut doc = Document::with_version("1.5");
            let pages_id = doc.new_object_id();
            let empty = || Stream::new(Dictionary::new(), Vec::new());
            let c0 = doc.add_object(empty());
            let c1 = doc.add_object(empty());
            // Page 1: normal origin (0,0) → gets a layer.
            let p0 = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => c0,
                "MediaBox" => vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(200), Object::Integer(200),
                ],
            });
            // Page 2: shifted origin (50,50) → skipped by the geometry guard.
            let p1 = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => c1,
                "MediaBox" => vec![
                    Object::Integer(50), Object::Integer(50),
                    Object::Integer(250), Object::Integer(250),
                ],
            });
            doc.objects.insert(
                pages_id,
                Object::Dictionary(dictionary! {
                    "Type" => "Pages",
                    "Kids" => vec![Object::Reference(p0), Object::Reference(p1)],
                    "Count" => Object::Integer(2),
                }),
            );
            let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
            doc.trailer.set("Root", catalog_id);
            doc.save(&src).expect("write two-page pdf");
        }
        let dest = temp_path("out.pdf");

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![px_word("Scanned")] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        let document = pdfium.load_pdf_from_file(&src, None).expect("load src");
        state
            .insert_document("doc1".to_string(), DocEntry { document, file_path: src.clone() })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save");

        assert_eq!(result.pages_written, 1, "the plain page should get a layer");
        assert_eq!(
            result.pages_skipped_unsupported_geometry, 1,
            "the offset page should be counted as skipped"
        );

        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// A page with a native text layer must not receive a duplicate OCR layer.
    /// The fixture's single page has real text ("Test Fixture"), so no page is
    /// text-less and nothing is written.
    #[test]
    fn pages_with_native_text_are_not_modified() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);

        let src = crate::fixture_path();
        let document = pdfium
            .load_pdf_from_file(src.to_str().unwrap(), None)
            .expect("load fixture");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry {
                    document,
                    file_path: src.to_string_lossy().into_owned(),
                },
            )
            .expect("insert");

        let dest = temp_path("native.pdf");
        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save searchable");

        assert_eq!(result.pages_written, 0, "native-text page must get no layer");
        // With nothing to author, the copy must be byte-identical to the source
        // (a faithful copy, not a lopdf re-serialization).
        let src_bytes = std::fs::read(&src).expect("read source");
        let dest_bytes = std::fs::read(&dest).expect("read dest");
        assert_eq!(src_bytes, dest_bytes, "unchanged copy should be byte-identical");
        std::fs::remove_file(&dest).ok();
    }

    /// "Save As" only: the source file's bytes are untouched even though the
    /// destination gains a text layer.
    #[test]
    fn source_file_is_unchanged() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let before = std::fs::read(&src).expect("read src");
        let dest = temp_path("out.pdf");

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![px_word("Scanned")] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { document, file_path: src.clone() },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("save searchable");
        assert_eq!(result.pages_written, 1);

        let after = std::fs::read(&src).expect("read src again");
        assert_eq!(before, after, "source file must not be modified");

        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// A pre-set cancel token stops before any file is written.
    #[test]
    fn cancellation_writes_no_file() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let dest = temp_path("out.pdf");
        std::fs::remove_file(&dest).ok();

        let state = AppState::new(pdfium, None);
        let document = pdfium.load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { document, file_path: src.clone() },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let result = save_searchable_copy_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            dest.clone(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("save searchable");

        assert!(result.cancelled);
        assert_eq!(result.pages_written, 0);
        assert!(!std::path::Path::new(&dest).exists(), "cancelled run must not write");

        std::fs::remove_file(&src).ok();
    }
}

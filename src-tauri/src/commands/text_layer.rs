//! "Add Text Layer" — the persisted OCR-layer tier (issue #4).
//!
//! Where "Make Searchable" (see [`crate::commands::ocr`]) recognizes text into a
//! session-only cache, this embeds those words into the document as an
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
//! Like every other edit (issue #31), the layer is applied to the in-memory
//! buffer and the document is marked dirty; the user commits it to disk with
//! an ordinary Save / Save As. Nothing here touches the file.
//!
//! Coordinate note: `OcrWord.rect` is in points with a bottom-left origin, but
//! measured from the corner of the box pdfium *rendered* (the CropBox), because
//! that is the bitmap the OCR engine read. A content stream is authored in user
//! space. The two coincide only when the render box sits at `[0 0 w h]`; on a
//! deskewed scan every page carries a small non-zero origin, so authoring adds
//! the render box's origin to each word (issue #129). `/Rotate` needs more than
//! a translation — the glyphs must turn, not just their boxes — so rotated pages
//! are still *detected and skipped* rather than mis-positioned.

use crate::commands::ocr::{
    cache_get, ocr_page_into_cache, ocr_words_to_line_groups, OcrCache, OcrEngine, OcrProgress,
    OcrWord,
};
use crate::commands::page_space::PageSpace;
use crate::commands::text::page_text_in_document_order;
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
pub struct AddTextLayerResult {
    /// Pages that received an invisible OCR text layer.
    pub pages_written: u32,
    /// Text-less pages that were OCR'd but left un-searchable because the
    /// layer author can't yet place text on a rotated page. Surfaced so the
    /// user is told, rather than silently seeing a lower count. (Distinct from
    /// a scanned page on which OCR simply recognized no encodable text — a rare
    /// case not separately counted here.)
    pub pages_skipped_unsupported_geometry: u32,
    pub cancelled: bool,
}

// ── Content-stream authoring (pure) ─────────────────────────────────────────

/// Advance width of a WinAnsi byte in the standard Helvetica font, in 1000ths
/// of an em (Adobe AFM values). Used to compute a line's true natural width so
/// the horizontal scaling stretches it to exactly the OCR box — the backend
/// equivalent of the frontend text layer's canvas width measurement. Bytes
/// outside the table (rare WinAnsi punctuation) fall back to a mid-range 556.
pub(crate) fn helvetica_width_1000(byte: u8) -> u16 {
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
pub(crate) fn encode_for_font(text: &str) -> Vec<u8> {
    text.chars().filter_map(win_ansi_byte).collect()
}

/// Test-only convenience: [`build_invisible_text_stream_runs`] at line
/// granularity, which is what every production caller outside redaction asks
/// for.
///
/// Compiled only for tests — `commands` is a private module, so a wrapper
/// nothing in the crate calls is genuinely unreachable, and rustc reports it
/// as dead rather than taking `pub` at face value.
#[cfg(test)]
pub fn build_invisible_text_stream(words: &[OcrWord], font_name: &str) -> Result<Vec<u8>, AppError> {
    build_invisible_text_stream_runs(words, font_name, false)
}

/// Builds the invisible-text content stream for one page's worth of OCR words.
///
/// Words are grouped into visual **lines** with the same
/// [`ocr_words_to_line_groups`] pass the ephemeral "Make Searchable" overlay
/// uses, and each line becomes one `BT … ET` text object in render mode 3 —
/// but every word inside it is positioned at **its own box**, with its own
/// horizontal scale (`Tz`) and an absolute `Tm`.
///
/// Placing words individually is not a refinement, it is the difference
/// between a layer that lands on the ink and one that doesn't. A line-wide
/// run positions only its two ends: the text between them is laid out by
/// stretching a single-space-joined string uniformly, so on justified text —
/// where the real word gaps vary — a mid-line word's glyphs drift from the
/// ink they belong to (measured at ~47pt on a 200pt page by
/// `mid_line_words_land_on_their_own_boxes`). The error is purely horizontal,
/// because the vertical metrics below come from the line box and are right
/// either way, which makes it easy to mistake for a page-offset problem.
///
/// Keeping the whole line in one `BT … ET` is what preserves the smooth
/// selection and search highlighting of "Make Searchable": readers group
/// selection by text object, so a line stays one flowing span even though its
/// words are individually placed.
///
/// Font size and baseline are derived from the **line's** union box and
/// Helvetica's loose metrics so the run's text-extraction box coincides with
/// the OCR box: with `fs = height / (ascent + descent)` the box height
/// matches, and placing the baseline at `box_bottom + descent·fs` makes the
/// box bottom sit on the OCR box bottom (the descent hangs down to exactly the
/// box bottom, not below it). Taking them per line rather than per word keeps
/// one baseline across the line where the engine reported slightly different
/// word heights.
///
/// Each `BT…ET` block isolates *text* state; isolation from the page's
/// *graphics* state (a leftover CTM or clip) is handled where this stream is
/// appended — see [`append_content_stream`], which wraps the existing content
/// in `q`/`Q`.
///
/// Returns `Ok(vec![])` when no line has representable text (e.g. a pure-CJK
/// page) — a legitimate "nothing to write". An encoding failure is returned as
/// `Err` rather than collapsed into an empty stream, so the caller can't mistake
/// a real error for an empty page and silently drop the layer.
///
/// With `per_word` set, every word additionally becomes its own text object
/// rather than sharing the line's. Redaction (issue #1) uses this for its
/// re-OCR of flattened pages: per-word *placement* already keeps glyphs out of
/// a burned mid-line gap, and isolating the text objects too means nothing
/// about that guarantee depends on how a reader groups a shared object. The
/// cost — selection that steps per word instead of flowing per line — stays
/// confined to redacted pages.
pub(crate) fn build_invisible_text_stream_runs(
    words: &[OcrWord],
    font_name: &str,
    per_word: bool,
) -> Result<Vec<u8>, AppError> {
    // Group into visual lines but keep each word's own box: the line supplies
    // the shared vertical metrics, each word supplies its own horizontal
    // placement. `per_word` makes every word its own group, so it also gets
    // its own text object.
    let groups: Vec<Vec<&OcrWord>> = if per_word {
        words.iter().map(|w| vec![w]).collect()
    } else {
        ocr_words_to_line_groups(words)
    };

    let mut ops: Vec<Operation> = Vec::new();
    for group in groups {
        // Vertical metrics from the line's union box, so every word on the
        // line shares one baseline and one size even where the OCR engine
        // reported slightly different heights per word.
        let bottom = group.iter().map(|w| w.rect.y).fold(f32::INFINITY, f32::min);
        let top = group
            .iter()
            .map(|w| w.rect.y + w.rect.height)
            .fold(f32::NEG_INFINITY, f32::max);
        let box_height = (top - bottom).max(1.0);
        let font_size = box_height / (HELVETICA_ASCENT_RATIO + HELVETICA_DESCENT_RATIO);
        let baseline_y = bottom + HELVETICA_DESCENT_RATIO * font_size;

        // Opened lazily: a group whose every word is unrepresentable (a
        // pure-CJK line) must emit no text object at all, not an empty one.
        let mut opened = false;
        for word in group {
            let encoded = encode_for_font(&word.text);
            if encoded.is_empty() {
                continue;
            }
            if !opened {
                ops.push(Operation::new("BT", vec![]));
                ops.push(Operation::new(
                    "Tf",
                    vec![Object::Name(font_name.as_bytes().to_vec()), Object::Real(font_size)],
                ));
                ops.push(Operation::new("Tr", vec![Object::Integer(3)])); // invisible
                opened = true;
            }
            // Each word is stretched to its *own* box and positioned at its
            // own origin with an absolute `Tm` (not a relative `Td`, which
            // would accumulate across the line).
            let h_scale = horizontal_scale_percent(&encoded, font_size, word.rect.width);
            ops.push(Operation::new("Tz", vec![Object::Real(h_scale)]));
            ops.push(Operation::new(
                "Tm",
                vec![
                    Object::Real(1.0),
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(1.0),
                    Object::Real(word.rect.x),
                    Object::Real(baseline_y),
                ],
            ));
            ops.push(Operation::new(
                "Tj",
                vec![Object::String(encoded, StringFormat::Literal)],
            ));
        }
        if opened {
            ops.push(Operation::new("ET", vec![]));
        }
    }
    Content { operations: ops }
        .encode()
        .map_err(|e| AppError::lopdf("Failed to encode OCR text content", e))
}

// ── Page geometry ───────────────────────────────────────────────────────────

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

/// Builds the page's Resources dictionary with our OCR font added, preserving
/// every existing resource (images, other fonts) whether the page owns its
/// `/Resources` or inherits them. Returned owned so it can be set after the
/// mutable page borrow begins.
pub(crate) fn merged_resources_with_font(
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
pub(crate) fn contents_refs(doc: &Document, page_id: ObjectId) -> Vec<ObjectId> {
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
pub(crate) fn append_content_stream(
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
pub async fn add_text_layer(
    app: tauri::AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<AddTextLayerResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let engine = state.ocr_engine.clone();
    let cache = state.ocr_cache_handle();
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_ocr_job(cancel.clone());

    // Same shared `ocr-progress` channel the progress overlay already listens on.
    let emit = move |page, total| {
        let _ = window.emit("ocr-progress", OcrProgress { page, total });
    };

    let outcome = tauri::async_runtime::spawn_blocking(move || {
        add_text_layer_impl(emit, entry, doc_id.clone(), engine, cache, cancel)
            .map(|r| (doc_id, r))
    })
    .await
    .map_err(|e| e.to_string());

    state.take_ocr_job();
    let (doc_id, (result, edited_bytes)) = outcome?.map_err(String::from)?;

    // A layer was authored: it becomes the buffer (dirty until the user saves).
    // `set_buffer_and_refresh` also drops the doc's OCR cache — correct, since
    // those pages now carry native text.
    if let Some(bytes) = edited_bytes {
        state.set_buffer_and_refresh(&doc_id, bytes).map_err(String::from)?;
        let _ = app.emit(
            "document-dirty-changed",
            crate::commands::save::dirty_changed_payload(&state, doc_id, true),
        );
    }
    Ok(result)
}

/// Runs Phase A (OCR text-less pages into the cache) and Phase B (author the
/// invisible layer), returning the result plus the edited document bytes —
/// `None` when no page needed a layer (or the run was cancelled), in which
/// case the document is left untouched.
fn add_text_layer_impl(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
) -> Result<(AddTextLayerResult, Option<Vec<u8>>), AppError> {
    add_text_layer_impl_filtered(emit_progress, entry, doc_id, engine, cache, cancel, None, false)
}

/// [`add_text_layer_impl`] with an optional page filter and run granularity:
/// when `only_pages` is `Some`, pages outside the set are neither OCR'd nor
/// given a layer, and `per_word_runs` selects per-word authoring (see
/// [`build_invisible_text_stream_runs`]). Redaction (issue #1) uses both — it
/// re-OCRs just the pages it flattened, with runs that cannot span a burned
/// gap.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_text_layer_impl_filtered(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
    only_pages: Option<&std::collections::HashSet<u32>>,
    per_word_runs: bool,
) -> Result<(AddTextLayerResult, Option<Vec<u8>>), AppError> {
    // The buffer is the authoritative bytes (it carries any unsaved edits), so
    // the layer is authored into it, not into the file on disk.
    let (source_bytes, page_count) = {
        let entry = lock_mutex(&entry)?;
        (entry.buffer.clone(), entry.document.pages().len() as u32)
    };

    // Phase A (pdfium): ensure every text-less page is OCR'd into the cache, and
    // remember which pages are text-less — those are the only ones that get a
    // layer (native-text pages are already searchable; adding text would
    // duplicate and confuse selection).
    let mut textless_pages: Vec<u32> = Vec::new();
    for i in 0..page_count {
        let page_num = i + 1;
        if only_pages.is_some_and(|s| !s.contains(&page_num)) {
            continue;
        }

        if cancel.load(Ordering::Relaxed) {
            return Ok((
                AddTextLayerResult {
                    pages_written: 0,
                    pages_skipped_unsupported_geometry: 0,
                    cancelled: true,
                },
                None,
            ));
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
                .map(|t| page_text_in_document_order(&t))
                .unwrap_or_default()
                .trim()
                .is_empty()
        };
        if native_empty {
            ocr_page_into_cache(&entry, &doc_id, page_num, &engine, &cache)?;
            textless_pages.push(page_num);
        }
    }

    // Phase B (lopdf): author the invisible text into a fresh copy parsed from
    // the buffer. pdfium's handle is never used for writing. Only parse/rewrite
    // the PDF when at least one page actually needs a layer — see the write step.
    let mut pages_written = 0u32;
    let mut pages_skipped_unsupported_geometry = 0u32;
    let mut doc: Option<Document> = None;

    if !textless_pages.is_empty() {
        let mut d = Document::load_mem(&source_bytes)
            .map_err(|e| AppError::lopdf("Failed to parse PDF for searchable copy", e))?;
        let pages = d.get_pages();
        // Add the shared font object lazily — only when a page first needs it.
        let mut font_id: Option<ObjectId> = None;

        for page_num in textless_pages {
            let Some(mut words) = cache_get(&cache, &doc_id, page_num) else {
                continue;
            };
            let Some(&page_id) = pages.get(&page_num) else {
                continue;
            };

            // Rotation still can't be authored: unlike a flattened polyline,
            // rotated text isn't handled by mapping its corners — the glyphs
            // have to turn too, which needs a text matrix rather than the bare
            // `Td` below. Better no layer than a mis-placed one; count it so
            // the user is told the page was left un-searchable.
            let space = PageSpace::of(&d, page_id);
            if space.rotate() != 0 {
                pages_skipped_unsupported_geometry += 1;
                continue;
            }

            // `OcrWord.rect` is measured from the corner of the box pdfium
            // *rendered* (the CropBox), while a content stream is authored in
            // user space. On a page whose box origin isn't (0,0) — every page
            // of a deskewed scan — the two differ by exactly that origin, so
            // shift the words onto it (issue #129). Both halves of this claim
            // are pinned by
            // `pdfium_reports_text_in_user_space_but_renders_the_cropbox`.
            let [ox, oy] = space.origin();
            if ox != 0.0 || oy != 0.0 {
                for w in &mut words {
                    w.rect.x += ox;
                    w.rect.y += oy;
                }
            }

            let stream_bytes = build_invisible_text_stream_runs(&words, FONT_NAME, per_word_runs)?;
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

    // Serialize the modified document only when a layer was actually authored;
    // otherwise return None so the caller leaves the buffer untouched (a lopdf
    // re-serialization for no reason would reorder objects and could drop
    // structures it doesn't model).
    let edited_bytes = match doc.as_mut() {
        Some(d) if pages_written > 0 => {
            let mut out = Vec::new();
            d.save_to(&mut out)
                .map_err(|e| AppError::io("Failed to serialize text layer", e))?;
            Some(out)
        }
        _ => None,
    };

    Ok((
        AddTextLayerResult {
            pages_written,
            pages_skipped_unsupported_geometry,
            cancelled: false,
        },
        edited_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::text::TextRect;
    use pdfium_render::prelude::{PdfSearchDirection, PdfSearchOptions};
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

    /// Adds a text layer to a one-page PDF whose page content is
    /// `page_content`, seeded with a single OCR `word`, then returns the unioned
    /// loose bounds `(left, bottom, right, top)` pdfium reports for the authored
    /// layer. Used to assert the layer lands on the OCR box regardless of the
    /// page's pre-existing graphics state. The caller must hold
    /// `test_pdfium_guard()`.
    fn saved_layer_loose_bounds(page_content: &[u8], word: OcrWord) -> (f32, f32, f32, f32) {
        let pdfium = crate::test_pdfium();
        let src = temp_path("src.pdf");
        write_pdf_with_content(&src, page_content);

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());
        // Seed the cache directly so the rect is exactly `word.rect`.
        state.set_ocr_words("doc1", 1, vec![word]);
        let document = pdfium.get().load_pdf_from_file(&src, None).expect("load src");
        state
            .insert_document("doc1".to_string(), DocEntry { page_cache: Vec::new(), document, file_path: src.clone(), buffer: std::fs::read(&src).expect("read src"), dirty: false, protection: crate::state::Protection::Plaintext, linearized: false })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let (_, bytes) = add_text_layer_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");

        let reopened = pdfium.get()
            .load_pdf_from_byte_vec(bytes.expect("edited bytes"), None)
            .expect("reopen");
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
        (left, bottom, right, top)
    }

    /// Pins the coordinate space pdfium's text extraction reports on a page
    /// whose box origin is *not* `(0,0)` — the assumption the offset fix rests
    /// on (issue #129).
    ///
    /// Every other placement test here uses a `[0 0 w h]` MediaBox, where user
    /// space and render space coincide, so nothing else can tell them apart.
    /// Measured here:
    ///
    /// - `loose_bounds()` reports **user space**, unshifted by the MediaBox or
    ///   CropBox origin — a run authored at user-space `(100, 200)` reads back
    ///   at `x = 100` on both pages below.
    /// - `page.width()/height()` — what `bitmap_rect_to_pdf_points` divides by
    ///   to place OCR words — tracks the **CropBox** (200x400 vs 180x370).
    ///
    /// So an OCR word's rect is relative to the **CropBox** corner while the
    /// content stream it is written into is user space, and the correction
    /// between them is the CropBox origin. That is why authoring goes through
    /// `PageSpace` (which prefers CropBox) rather than reading the MediaBox.
    ///
    /// Non-square on purpose (200x400): a square page hides width/height
    /// mix-ups.
    #[test]
    fn pdfium_reports_text_in_user_space_but_renders_the_cropbox() {
        let pdfium = crate::test_pdfium();

        // MediaBox [50 60 250 460] -> 200x400, origin (50, 60); the optional
        // CropBox gives a *different* origin so the two can't be confused.
        let build = |crop: Option<[f32; 4]>| {
            let mut doc = Document::with_version("1.5");
            let pages_id = doc.new_object_id();
            let font = doc.add_object(dictionary! {
                "Type" => "Font", "Subtype" => "Type1",
                "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
            });
            // A *visible* run (no `3 Tr`) at user-space (100, 200), size 24.
            let content = doc.add_object(Stream::new(
                Dictionary::new(),
                b"BT /F1 24 Tf 100 200 Td (Probe) Tj ET\n".to_vec(),
            ));
            let mut page_dict = dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content,
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => Object::Reference(font) },
                },
                "MediaBox" => vec![
                    Object::Real(50.0), Object::Real(60.0),
                    Object::Real(250.0), Object::Real(460.0),
                ],
            };
            if let Some([x0, y0, x1, y1]) = crop {
                page_dict.set("CropBox", vec![
                    Object::Real(x0), Object::Real(y0),
                    Object::Real(x1), Object::Real(y1),
                ]);
            }
            let page_id = doc.add_object(Object::Dictionary(page_dict));
            doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }));
            let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
            doc.trailer.set("Root", catalog_id);
            let mut out = Vec::new();
            doc.save_to(&mut out).expect("serialize probe pdf");
            out
        };

        // Returns (page_width, page_height, text_left, text_bottom).
        let measure = |bytes: Vec<u8>| {
            let doc = pdfium.get()
                .load_pdf_from_byte_vec(bytes, None)
                .expect("load probe");
            let page = doc.pages().get(0).expect("page");
            let (pw, ph) = (page.width().value, page.height().value);
            let text = page.text().expect("text");
            let (mut left, mut bottom) = (f32::INFINITY, f32::INFINITY);
            for ch in text.chars().iter() {
                if let Ok(b) = ch.loose_bounds() {
                    left = left.min(b.left().value);
                    bottom = bottom.min(b.bottom().value);
                }
            }
            (pw, ph, left, bottom)
        };

        let (pw, ph, left, bottom) = measure(build(None));
        assert!((pw - 200.0).abs() < 0.5 && (ph - 400.0).abs() < 0.5, "page size {pw}x{ph}");
        assert!(
            (left - 100.0).abs() < 0.5,
            "text left {left}: extraction is not user space (MediaBox origin leaked in)"
        );
        // Baseline 200 less Helvetica's descent at size 24 (24 * 0.211).
        assert!((bottom - 194.936).abs() < 0.5, "text bottom {bottom}");

        // With a CropBox, the *rendered* page shrinks to it — so OCR word rects
        // are measured against 180x370 from the CropBox corner — while the
        // extracted text stays at the same user-space x.
        let (pw, ph, left, bottom) = measure(build(Some([70.0, 90.0, 250.0, 460.0])));
        assert!(
            (pw - 180.0).abs() < 0.5 && (ph - 370.0).abs() < 0.5,
            "page size {pw}x{ph}: pdfium should render the CropBox"
        );
        assert!(
            (left - 100.0).abs() < 0.5,
            "text left {left}: extraction is not user space (CropBox origin leaked in)"
        );
        assert!((bottom - 194.936).abs() < 0.5, "text bottom {bottom}");
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

    /// Words sharing a baseline go into a single `BT…ET` text object — that is
    /// what preserves the smooth, uniform selection highlighting of "Make
    /// Searchable" — while each word is shown separately so it can be placed
    /// at its own box.
    #[test]
    fn words_on_one_line_share_one_text_object_but_are_shown_separately() {
        let words = vec![
            pt_word("Hello", 10.0, 100.0, 30.0, 12.0),
            pt_word("World", 50.0, 100.0, 30.0, 12.0),
        ];
        let bytes = build_invisible_text_stream(&words, FONT_NAME).expect("encode");
        let content = Content::decode(&bytes).expect("decode content");
        let count = |op_name: &str| {
            content.operations.iter().filter(|op| op.operator == op_name).count()
        };
        assert_eq!(count("BT"), 1, "two words on one line should be one text object");
        assert_eq!(count("ET"), 1);
        // One show + one absolute placement per word.
        assert_eq!(count("Tj"), 2, "each word is shown separately");
        assert_eq!(count("Tm"), 2, "each word gets its own absolute placement");

        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("Hello") && s.contains("World"), "missing words: {s}");
    }

    /// Showing words separately must not cost the reader its word breaks: the
    /// joined string is no longer written into the file, so extraction has to
    /// recover the space from the gap between the two placements. pdfium does,
    /// and this pins it — a layer that copies out as "HelloWorld" would be a
    /// quiet regression in every paste.
    #[test]
    fn separately_placed_words_still_extract_with_a_space() {
        let pdfium = crate::test_pdfium();
        let bytes = boxed_page_bytes([0.0, 0.0, 200.0, 400.0], None, 0, b"");
        let words = vec![
            OcrWord {
                text: "Hello".to_string(),
                rect: TextRect { x: 10.0, y: 300.0, width: 30.0, height: 12.0 },
            },
            OcrWord {
                text: "World".to_string(),
                rect: TextRect { x: 150.0, y: 300.0, width: 30.0, height: 12.0 },
            },
        ];
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: words.clone() });
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());
        state.set_ocr_words("doc1", 1, words);
        let document = pdfium
            .get()
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .expect("load");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry {
                    page_cache: Vec::new(),
                    document,
                    file_path: String::new(),
                    buffer: bytes,
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");
        let (_, out) = add_text_layer_impl(
            |_, _| {},
            state.get_document("doc1").expect("get"),
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");

        let doc = pdfium
            .get()
            .load_pdf_from_byte_vec(out.expect("edited bytes"), None)
            .expect("reopen");
        let page = doc.pages().get(0).expect("page");
        let text = page.text().expect("text");
        assert_eq!(
            crate::commands::text::page_text_in_document_order(&text),
            "Hello World"
        );
    }

    #[test]
    fn unrepresentable_only_word_is_skipped() {
        // A pure-CJK token has no WinAnsi bytes, so it contributes nothing.
        let bytes = build_invisible_text_stream(&[pt_word("日本語", 0.0, 0.0, 30.0, 10.0)], FONT_NAME)
            .expect("encode");
        assert!(bytes.is_empty(), "CJK-only word should be dropped under WinAnsi");
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

    /// A blank (scanned-style) page + cached OCR words → add layer → reopen the
    /// edited bytes with pdfium → pdfium's **native** text API returns the
    /// words. This proves the layer is real text operators, not a Tumbler-only
    /// overlay.
    #[test]
    fn edited_bytes_are_natively_searchable() {
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![px_word("Scanned")] });
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());

        let document = pdfium.get().load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { page_cache: Vec::new(), document, file_path: src.clone(), buffer: std::fs::read(&src).expect("read src"), dirty: false, protection: crate::state::Protection::Plaintext, linearized: false },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let (result, bytes) = add_text_layer_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");

        assert_eq!(result.pages_written, 1);
        assert!(!result.cancelled);

        // Reopen the edited bytes and read text through pdfium's native API.
        let reopened = pdfium.get()
            .load_pdf_from_byte_vec(bytes.expect("edited bytes"), None)
            .expect("reopen edited bytes");
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
    }

    /// The saved layer's text-extraction (loose) box — which drives the
    /// selection/search highlight — must coincide with the OCR box, so the
    /// highlight sits on the scanned text rather than a fraction of a line low.
    /// Seeded rect: bottom y=100, height=20 → OCR box spans y 100..120.
    #[test]
    fn layer_box_matches_ocr_box() {
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);

        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());
        // Seed the cache directly so the rect is exactly the one above (no
        // pixel→point mapping in the way).
        state.set_ocr_words("doc1", 1, vec![word]);
        let document = pdfium.get().load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document("doc1".to_string(), DocEntry { page_cache: Vec::new(), document, file_path: src.clone(), buffer: std::fs::read(&src).expect("read src"), dirty: false, protection: crate::state::Protection::Plaintext, linearized: false })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let (_, bytes) = add_text_layer_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");

        // Union the loose bounds of every character in the run.
        let reopened = pdfium.get()
            .load_pdf_from_byte_vec(bytes.expect("edited bytes"), None)
            .expect("reopen");
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

        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());
        state.set_ocr_words("doc1", 1, vec![word]);
        let document = pdfium.get().load_pdf_from_file(&src, None).expect("load src");
        state
            .insert_document("doc1".to_string(), DocEntry { page_cache: Vec::new(), document, file_path: src.clone(), buffer: std::fs::read(&src).expect("read src"), dirty: false, protection: crate::state::Protection::Plaintext, linearized: false })
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let (result, bytes) = add_text_layer_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");
        assert_eq!(result.pages_written, 1);

        let reopened = pdfium.get()
            .load_pdf_from_byte_vec(bytes.expect("edited bytes"), None)
            .expect("reopen");
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
    }

    /// Times Add Text Layer over a real large scan, with recognition taken out
    /// of the picture: the OCR cache is pre-seeded for every page, so the run
    /// measures the text-extraction scan plus the lopdf parse / author /
    /// reserialize that issue #129 made reachable on such a file for the first
    /// time. Recognition itself is unchanged by that fix and dominates the
    /// wall clock (seconds per page), so it would only hide what is being
    /// measured here.
    ///
    /// Ignored by default -- it needs a file this repo can't carry. Run with:
    ///
    /// ```text
    /// TUMBLER_BENCH_PDF=C:\path\to\scan.pdf cargo test --lib \
    ///     bench_add_text_layer_on_a_large_scan -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a large scanned PDF via TUMBLER_BENCH_PDF"]
    fn bench_add_text_layer_on_a_large_scan() {
        let Ok(path) = std::env::var("TUMBLER_BENCH_PDF") else {
            panic!("set TUMBLER_BENCH_PDF to a scanned PDF path");
        };
        let pdfium = crate::test_pdfium();
        let bytes = std::fs::read(&path).expect("read bench pdf");
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: Vec::new() });
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());

        let document = pdfium.get()
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .expect("load bench pdf");
        let page_count = document.pages().len() as u32;
        state
            .insert_document(
                "bench".to_string(),
                DocEntry {
                    page_cache: Vec::new(),
                    document,
                    file_path: path.clone(),
                    buffer: bytes.clone(),
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");

        // Seed every page so Phase A returns from cache instead of rendering
        // and recognizing.
        for page in 1..=page_count {
            state.set_ocr_words(
                "bench",
                page,
                vec![OcrWord {
                    text: "Scanned line of text".to_string(),
                    rect: TextRect { x: 40.0, y: 300.0, width: 300.0, height: 14.0 },
                }],
            );
        }

        let start = std::time::Instant::now();
        let (result, edited) = add_text_layer_impl(
            |_, _| {},
            state.get_document("bench").expect("get"),
            "bench".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");
        let elapsed = start.elapsed();

        println!(
            "{page_count} pages, {:.1} MB in -> written {}, skipped(rotated) {}, \
             {:.1} MB out, {:.2}s (excludes OCR recognition)",
            bytes.len() as f64 / 1_048_576.0,
            result.pages_written,
            result.pages_skipped_unsupported_geometry,
            edited.as_ref().map_or(0.0, |b| b.len() as f64 / 1_048_576.0),
            elapsed.as_secs_f64(),
        );
    }

    /// Diagnostic for the "search highlight sits beside the word" report:
    /// authors a layer over one page of a real scan with the real Windows OCR
    /// engine, then prints, for every hit of a query, the OCR word box the ink
    /// actually occupies next to the rectangle pdfium reports for the match.
    ///
    /// Ignored -- needs a language pack and a file the repo cannot carry:
    ///
    /// ```text
    /// TUMBLER_BENCH_PDF=...\\scan.pdf TUMBLER_DIAG_PAGE=182 \
    ///   TUMBLER_DIAG_QUERY=Heade cargo test --lib diag_search_highlight_vs_ocr_box \
    ///   -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a large scanned PDF and a Windows OCR language pack"]
    fn diag_search_highlight_vs_ocr_box() {
        let path = std::env::var("TUMBLER_BENCH_PDF").expect("TUMBLER_BENCH_PDF");
        let page_1based: u32 = std::env::var("TUMBLER_DIAG_PAGE")
            .expect("TUMBLER_DIAG_PAGE")
            .parse()
            .expect("page number");
        let query = std::env::var("TUMBLER_DIAG_QUERY").unwrap_or_else(|_| "Heade".to_string());

        let pdfium = crate::test_pdfium();
        let bytes = std::fs::read(&path).expect("read pdf");
        // The real engine, not a fake: the whole question is what the engine
        // reports versus where the layer puts it.
        let engine: Arc<dyn OcrEngine> = Arc::new(crate::commands::ocr::WindowsOcrEngine::new());
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());
        let document = pdfium.get()
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .expect("load pdf");
        state
            .insert_document(
                "diag".to_string(),
                DocEntry {
                    page_cache: Vec::new(),
                    document,
                    file_path: path.clone(),
                    buffer: bytes,
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");

        let only: std::collections::HashSet<u32> = [page_1based].into_iter().collect();
        let (result, edited) = add_text_layer_impl_filtered(
            |_, _| {},
            state.get_document("diag").expect("get"),
            "diag".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
            Some(&only),
            false,
        )
        .expect("add layer");
        println!("pages_written = {}", result.pages_written);

        let words = state.get_ocr_words("diag", page_1based).unwrap_or_default();
        println!("OCR words on page: {}", words.len());
        for w in words.iter().filter(|w| w.text.contains(&query)) {
            println!(
                "  OCR word {:?}: x {:.1}..{:.1}  (w {:.1})  y {:.1}",
                w.text,
                w.rect.x,
                w.rect.x + w.rect.width,
                w.rect.width,
                w.rect.y,
            );
        }

        let doc = pdfium.get()
            .load_pdf_from_byte_vec(edited.expect("edited bytes"), None)
            .expect("reopen");
        let page = doc.pages().get(page_1based as i32 - 1).expect("page");
        let (ox, oy) = crate::commands::text::page_origin(&page);
        println!("page origin = ({ox:.2}, {oy:.2})");
        let text = page.text().expect("text");
        let options = PdfSearchOptions::new();
        let search = text.search(&query, &options).expect("search");

        // Each hit is paired with the OCR word it should be sitting on: same
        // line (y within half a line) and overlapping horizontally. The delta
        // that matters is the *left* edge -- a hit covering only part of a word
        // ("Heade" inside "Heade's") legitimately stops short on the right.
        let mut worst: f32 = 0.0;
        for (i, seg) in search.iter(PdfSearchDirection::SearchForward).enumerate() {
            let (mut left, mut right, mut bottom) =
                (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY);
            let mut chars = String::new();
            for s in seg.iter() {
                let b = s.bounds();
                left = left.min(b.left().value);
                right = right.max(b.right().value);
                bottom = bottom.min(b.bottom().value);
                chars.push_str(&s.text());
            }
            // Into the render space the OCR cache speaks.
            let (rl, rr, rb) = (left - ox, right - ox, bottom - oy);

            // Pick the candidate with the largest horizontal overlap, not the
            // first one found: neighbouring words on an adjacent line overlap
            // the y test and would otherwise be paired, reporting a placement
            // error that is really a pairing error.
            let paired = words
                .iter()
                .filter(|w| (w.rect.y - rb).abs() < w.rect.height * 0.5)
                .map(|w| {
                    let overlap =
                        (rr.min(w.rect.x + w.rect.width) - rl.max(w.rect.x)).max(0.0);
                    (w, overlap)
                })
                .filter(|(_, overlap)| *overlap > 0.0)
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(w, _)| w);
            match paired {
                Some(w) => {
                    let d = rl - w.rect.x;
                    worst = worst.max(d.abs());
                    println!(
                        "  hit {i:2} {:?} render x {:.1}..{:.1}  <-  OCR {:?} x {:.1}..{:.1}  \
                         delta_left {:+.2}",
                        chars, rl, rr, w.text, w.rect.x, w.rect.x + w.rect.width, d,
                    );
                }
                None => println!("  hit {i:2} {chars:?} render x {rl:.1}..{rr:.1}  <-  (no OCR word paired)"),
            }
        }
        println!("worst left-edge delta: {worst:.2} pt");
    }

    /// Serializes a one-page PDF with an explicit `/MediaBox`, optional
    /// `/CropBox` and `/Rotate`, the given content stream, and a Helvetica
    /// `/F1` the content may use. Returned as bytes so a test can build a
    /// `DocEntry` without a temp file.
    ///
    /// `crate::geometry_page_bytes` covers rotation and cropping but always
    /// puts the MediaBox at `[0 0 w h]`; the offset-origin pages this module
    /// has to place text on (issue #129) need the origin itself moved.
    fn boxed_page_bytes(
        media: [f32; 4],
        crop: Option<[f32; 4]>,
        rotate: i64,
        content: &[u8],
    ) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1",
            "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
        });
        let content_id = doc.add_object(Stream::new(Dictionary::new(), content.to_vec()));
        let rect = |[x0, y0, x1, y1]: [f32; 4]| {
            vec![Object::Real(x0), Object::Real(y0), Object::Real(x1), Object::Real(y1)]
        };
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font) },
            },
            "MediaBox" => rect(media),
        };
        if let Some(c) = crop {
            page_dict.set("CropBox", rect(c));
        }
        if rotate != 0 {
            page_dict.set("Rotate", Object::Integer(rotate));
        }
        let page_id = doc.add_object(Object::Dictionary(page_dict));
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => Object::Integer(1),
        }));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize page bytes");
        out
    }

    /// Runs Add Text Layer over a one-page document built from `page_bytes`,
    /// with the OCR cache seeded to exactly `word` (so no pixel-to-point
    /// mapping sits in the way), and returns the result plus the unioned
    /// `[left, bottom, right, top]` pdfium reports for the authored layer --
    /// `None` when no layer was written.
    ///
    /// The seeded `word.rect` is in the space the cache holds: bottom-left
    /// origin, measured from the **rendered** box's corner. The returned
    /// bounds are **user space**. On an offset page those differ by the box
    /// origin -- see
    /// `pdfium_reports_text_in_user_space_but_renders_the_cropbox`, which pins
    /// both halves of that claim. Closing the gap is what these tests check.
    ///
    /// Takes the instance from the caller rather than acquiring: a second
    /// `test_pdfium()` while the caller holds one deadlocks.
    fn layer_over_page(
        pdfium: &'static pdfium_render::prelude::Pdfium,
        page_bytes: Vec<u8>,
        word: OcrWord,
    ) -> (AddTextLayerResult, Option<[f32; 4]>) {
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![word.clone()] });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        state.set_ocr_words("doc1", 1, vec![word]);

        let document = pdfium
            .load_pdf_from_byte_vec(page_bytes.clone(), None)
            .expect("load page bytes");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry {
                    page_cache: Vec::new(),
                    document,
                    file_path: String::new(),
                    buffer: page_bytes,
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");

        let (result, bytes) = add_text_layer_impl(
            |_, _| {},
            state.get_document("doc1").expect("get"),
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");

        let bounds = bytes.map(|b| {
            let doc = pdfium.load_pdf_from_byte_vec(b, None).expect("reopen edited bytes");
            let page = doc.pages().get(0).expect("page");
            let text = page.text().expect("text");
            let (mut left, mut bottom, mut right, mut top) =
                (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
            for ch in text.chars().iter() {
                if let Ok(bb) = ch.loose_bounds() {
                    left = left.min(bb.left().value);
                    bottom = bottom.min(bb.bottom().value);
                    right = right.max(bb.right().value);
                    top = top.max(bb.top().value);
                }
            }
            [left, bottom, right, top]
        });
        (result, bounds)
    }

    /// Asserts a layer's user-space bounds match the OCR box shifted by the
    /// rendered box's origin. Tolerances match the module's other placement
    /// tests: the run is stretched to the box by `Tz`, so the horizontal fit
    /// is looser than the vertical one.
    fn assert_layer_at(bounds: Option<[f32; 4]>, want: [f32; 4]) {
        let [left, bottom, right, top] = bounds.expect("a layer should have been written");
        let [want_l, want_b, want_r, want_t] = want;
        assert!((left - want_l).abs() < 4.0, "left {left}, want {want_l}");
        assert!((right - want_r).abs() < 4.0, "right {right}, want {want_r}");
        assert!((bottom - want_b).abs() < 1.5, "bottom {bottom}, want {want_b}");
        assert!((top - want_t).abs() < 1.5, "top {top}, want {want_t}");
    }

    /// Bounds of just the characters equal to `ch` in the authored layer,
    /// as `[left, right]` in user space.
    fn char_span(
        pdfium: &'static pdfium_render::prelude::Pdfium,
        page_bytes: Vec<u8>,
        words: Vec<OcrWord>,
        ch: char,
    ) -> [f32; 2] {
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: words.clone() });
        let state = AppState::new(pdfium, None).with_ocr_engine(engine.clone());
        state.set_ocr_words("doc1", 1, words);
        let document = pdfium
            .load_pdf_from_byte_vec(page_bytes.clone(), None)
            .expect("load");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry {
                    page_cache: Vec::new(),
                    document,
                    file_path: String::new(),
                    buffer: page_bytes,
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");
        let (_, bytes) = add_text_layer_impl(
            |_, _| {},
            state.get_document("doc1").expect("get"),
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");

        let doc = pdfium
            .load_pdf_from_byte_vec(bytes.expect("edited bytes"), None)
            .expect("reopen");
        let page = doc.pages().get(0).expect("page");
        let text = page.text().expect("text");
        let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);
        for c in text.chars().iter() {
            if c.unicode_char() != Some(ch) {
                continue;
            }
            if let Ok(b) = c.loose_bounds() {
                left = left.min(b.left().value);
                right = right.max(b.right().value);
            }
        }
        [left, right]
    }

    /// A line run positions only its two ends. Words *between* them are placed
    /// by uniform `Tz` stretching of a single-space-joined string, which has
    /// nothing to do with where they actually sit on the page -- so on a
    /// justified scan, whose word gaps vary, a mid-line word's invisible glyphs
    /// drift away from the ink they belong to. Vertically nothing moves, which
    /// is why this reads as a purely horizontal error.
    ///
    /// Two words on one line, 100pt apart: "ZZZZ" occupies x 150..180 and its
    /// layer must land there.
    #[test]
    fn mid_line_words_land_on_their_own_boxes() {
        let pdfium = crate::test_pdfium();
        let bytes = boxed_page_bytes([0.0, 0.0, 200.0, 400.0], None, 0, b"");
        let words = vec![
            OcrWord {
                text: "AAAA".to_string(),
                rect: TextRect { x: 10.0, y: 300.0, width: 30.0, height: 12.0 },
            },
            OcrWord {
                text: "ZZZZ".to_string(),
                rect: TextRect { x: 150.0, y: 300.0, width: 30.0, height: 12.0 },
            },
        ];

        let [left, right] = char_span(pdfium.get(), bytes, words, 'Z');

        assert!(
            (left - 150.0).abs() < 4.0 && (right - 180.0).abs() < 4.0,
            "second word at x {left}..{right}, want 150..180 -- a line run stretched \
             it away from its own box"
        );
    }

    /// The regression test for issue #129. A deskewed scan's MediaBox sits at
    /// a small non-zero origin on every page; the layer must be placed
    /// **relative to that origin**, not at the raw cache coordinates.
    ///
    /// Non-square (200x400) on purpose: on a square page a width/height mix-up
    /// cancels and a broken mapping passes.
    #[test]
    fn offset_mediabox_page_gets_a_layer_at_its_origin() {
        let pdfium = crate::test_pdfium();
        // MediaBox [50 60 250 460] -> 200x400 rendered, origin (50, 60).
        let bytes = boxed_page_bytes([50.0, 60.0, 250.0, 460.0], None, 0, b"");
        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };

        let (result, bounds) = layer_over_page(pdfium.get(), bytes, word);

        assert_eq!(result.pages_written, 1, "an offset page must still get a layer");
        assert_eq!(result.pages_skipped_unsupported_geometry, 0);
        // OCR box (30..150, 100..120) shifted by the origin (50, 60).
        assert_layer_at(bounds, [80.0, 160.0, 200.0, 180.0]);
    }

    /// pdfium renders the **CropBox**, so that -- not the MediaBox -- is the
    /// box an OCR word's rect is measured from. A page whose CropBox sits
    /// inside a larger MediaBox must have its layer placed against the CropBox
    /// corner; reading the MediaBox here would put the text ~90pt off.
    #[test]
    fn layer_is_placed_against_the_cropbox_not_the_mediabox() {
        let pdfium = crate::test_pdfium();
        // MediaBox at the origin, CropBox [70 90 270 490] -> 200x400 rendered.
        let bytes = boxed_page_bytes(
            [0.0, 0.0, 300.0, 500.0],
            Some([70.0, 90.0, 270.0, 490.0]),
            0,
            b"",
        );
        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };

        let (result, bounds) = layer_over_page(pdfium.get(), bytes, word);

        assert_eq!(result.pages_written, 1);
        assert_eq!(result.pages_skipped_unsupported_geometry, 0);
        // OCR box (30..150, 100..120) shifted by the CropBox origin (70, 90).
        assert_layer_at(bounds, [100.0, 190.0, 220.0, 210.0]);
    }

    /// Rotation remains unsupported and must still be reported rather than
    /// mis-placed. Unlike a flattened polyline, rotated text cannot be handled
    /// by mapping its corners -- the glyphs have to turn too, which needs a
    /// `Tm` rather than a bare `Td`. Until that exists, skip and say so.
    #[test]
    fn rotated_page_is_counted_as_skipped() {
        let pdfium = crate::test_pdfium();
        let bytes = crate::geometry_page_bytes(200.0, 400.0, 90, None);
        let word = OcrWord {
            text: "Scanned".to_string(),
            rect: TextRect { x: 30.0, y: 100.0, width: 120.0, height: 20.0 },
        };

        let (result, bounds) = layer_over_page(pdfium.get(), bytes, word);

        assert_eq!(result.pages_written, 0, "a rotated page must not be written");
        assert_eq!(
            result.pages_skipped_unsupported_geometry, 1,
            "the rotated page should be counted as skipped"
        );
        assert!(bounds.is_none(), "nothing written -> no edited bytes");
    }

    /// A page with a native text layer must not receive a duplicate OCR layer.
    /// The fixture's single page has real text ("Test Fixture"), so no page is
    /// text-less and no edit is produced — the buffer must be left alone (a
    /// lopdf re-serialization for no reason would churn the bytes).
    #[test]
    fn pages_with_native_text_produce_no_edit() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);

        let src = crate::fixture_path();
        let entry = DocEntry::load(pdfium.get(), &src.to_string_lossy(), None).expect("load fixture");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let (result, bytes) = add_text_layer_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");

        assert_eq!(result.pages_written, 0, "native-text page must get no layer");
        assert!(bytes.is_none(), "no layer authored → no edited bytes");
    }

    /// The layer is a deferred buffer edit: the source file's bytes on disk
    /// are untouched by the run (only an explicit Save writes them).
    #[test]
    fn source_file_is_unchanged() {
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);
        let before = std::fs::read(&src).expect("read src");

        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine { words: vec![px_word("Scanned")] });
        let state = AppState::new(pdfium.get(), None).with_ocr_engine(engine.clone());
        let document = pdfium.get().load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { page_cache: Vec::new(), document, file_path: src.clone(), buffer: std::fs::read(&src).expect("read src"), dirty: false, protection: crate::state::Protection::Plaintext, linearized: false },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let (result, bytes) = add_text_layer_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            engine,
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("add layer");
        assert_eq!(result.pages_written, 1);
        assert!(bytes.is_some());

        let after = std::fs::read(&src).expect("read src again");
        assert_eq!(before, after, "source file must not be modified");

        std::fs::remove_file(&src).ok();
    }

    /// A pre-set cancel token stops before any edit is produced.
    #[test]
    fn cancellation_produces_no_edit() {
        let pdfium = crate::test_pdfium();

        let src = temp_path("src.pdf");
        write_blank_pdf(&src);

        let state = AppState::new(pdfium.get(), None);
        let document = pdfium.get().load_pdf_from_file(&src, None).expect("load blank");
        state
            .insert_document(
                "doc1".to_string(),
                DocEntry { page_cache: Vec::new(), document, file_path: src.clone(), buffer: std::fs::read(&src).expect("read src"), dirty: false, protection: crate::state::Protection::Plaintext, linearized: false },
            )
            .expect("insert");

        let entry = state.get_document("doc1").expect("get");
        let (result, bytes) = add_text_layer_impl(
            |_, _| {},
            entry,
            "doc1".to_string(),
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("add layer");

        assert!(result.cancelled);
        assert_eq!(result.pages_written, 0);
        assert!(bytes.is_none(), "cancelled run must produce no edit");

        std::fs::remove_file(&src).ok();
    }
}

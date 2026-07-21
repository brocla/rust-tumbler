//! "Typewriter" — free-text notes placed anywhere on a page (issue #99).
//!
//! Lets the user type text over a page: the classic "typewriter" tool for
//! filling ad-hoc forms that use underline blanks instead of real form fields.
//! Each note is stored as a standard PDF **FreeText annotation** with a
//! generated appearance stream, so it survives round-trips and renders in other
//! readers. Every note carries a private `/TWid` key (plus the font/size/color
//! it was authored with) so Tumbler can find, re-edit, and re-write its own
//! notes idempotently without disturbing FreeText annotations authored
//! elsewhere.
//!
//! Like every edit (issue #31) this is a buffer edit: `apply_typewriter`
//! rewrites the in-memory buffer and marks the document dirty; an ordinary
//! Save / Save As commits it to disk. Tumbler renders the notes through the
//! `TypewriterLayer` HTML overlay (its page render leaves annotations off), so
//! the appearance stream exists purely for interoperability with other viewers.
//!
//! Coordinate note: the frontend sends each note's rect in PDF points with a
//! **top-left** origin (the same space as search/redaction rects); this module
//! flips it to PDF user space (bottom-left) using the page MediaBox. Rotated
//! pages (`/Rotate`) are not specially handled in this first cut — the common
//! unrotated page is authored correctly.

use crate::commands::save::dirty_changed_payload;
use crate::commands::text_layer::{
    contents_refs, encode_for_font, helvetica_width_1000, merged_resources_with_font,
};
use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::{Emitter, State};

/// Private annotation key marking a FreeText annotation as Tumbler's own, set to
/// the frontend annotation id. Its presence is how [`apply_typewriter`] finds
/// and replaces (rather than duplicates) previously-written notes, and how
/// [`read_typewriter`] re-hydrates them without hijacking foreign FreeText
/// annotations.
const TW_ID_KEY: &[u8] = b"TWid";

/// Padding between the note's box edge and its text, in points.
const INSET: f32 = 2.0;
/// Line advance as a multiple of the font size.
const LINE_HEIGHT_RATIO: f32 = 1.2;
/// First-baseline drop from the top inset, as a fraction of the font size —
/// roughly the ascender height, so the first line sits just below the box top.
const ASCENT_RATIO: f32 = 0.8;
/// Resource name of the note's font within its appearance stream (each note's
/// XObject has its own resource dictionary, so a fixed name never collides).
const FONT_RES: &str = "F0";

/// Font resource name for the invisible page-text layer (see
/// [`add_tumbler_text_layer`]). Prefixed so it can't collide with a page font.
const TEXT_LAYER_FONT_RES: &str = "TumblerTWFont";
/// Private key marking a content stream as Tumbler's invisible typewriter text
/// layer, so a re-apply removes and rebuilds it rather than stacking copies.
const TEXT_LAYER_TAG: &[u8] = b"TumblerTW";

/// One typewriter note. Mirrors the frontend `TypewriterAnnot` (serde
/// camelCase). The rect is PDF points, top-left origin, per (1-based) page.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TypewriterAnnot {
    pub id: String,
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub text: String,
    /// Base family: `"Helvetica"`, `"Times"`, or `"Courier"`.
    pub font_family: String,
    pub bold: bool,
    pub italic: bool,
    pub font_size: f32,
    /// RGB, each component 0.0..=1.0.
    pub color: [f32; 3],
}

// ── Font metrics (pure) ──────────────────────────────────────────────────────

/// Maps a family + bold/italic to the base-14 PostScript font name. These
/// standard fonts need no embedding, so a typewriter note adds no font data.
fn base_font_name(family: &str, bold: bool, italic: bool) -> &'static str {
    match family {
        "Times" => match (bold, italic) {
            (false, false) => "Times-Roman",
            (true, false) => "Times-Bold",
            (false, true) => "Times-Italic",
            (true, true) => "Times-BoldItalic",
        },
        "Courier" => match (bold, italic) {
            (false, false) => "Courier",
            (true, false) => "Courier-Bold",
            (false, true) => "Courier-Oblique",
            (true, true) => "Courier-BoldOblique",
        },
        // Helvetica is the default for any unrecognized family.
        _ => match (bold, italic) {
            (false, false) => "Helvetica",
            (true, false) => "Helvetica-Bold",
            (false, true) => "Helvetica-Oblique",
            (true, true) => "Helvetica-BoldOblique",
        },
    }
}

/// Advance width of a WinAnsi byte in 1000ths of an em. Courier is monospaced;
/// Helvetica uses its real AFM table (reused from the text-layer author) and
/// Times reuses it as a close approximation — this only steers the appearance
/// stream's line wrapping, which is for external viewers (Tumbler shows the
/// live overlay), so exact Times metrics aren't warranted in this first cut.
fn glyph_width_1000(family: &str, byte: u8) -> u16 {
    match family {
        "Courier" => 600,
        _ => helvetica_width_1000(byte),
    }
}

/// Width of an encoded byte run at a given font size, in points.
fn run_width(bytes: &[u8], family: &str, font_size: f32) -> f32 {
    let sum: u32 = bytes.iter().map(|&b| glyph_width_1000(family, b) as u32).sum();
    font_size * sum as f32 / 1000.0
}

/// Word-wraps the note's text to fit the box width, returning one WinAnsi-
/// encoded byte line per output row. Explicit newlines split paragraphs and are
/// preserved as blank lines; within a paragraph, words wrap greedily on spaces.
/// A single word wider than the box is left to overflow rather than hard-broken
/// (rare for the short entries this tool targets).
fn wrap_lines(text: &str, family: &str, font_size: f32, box_width: f32) -> Vec<Vec<u8>> {
    let inner = (box_width - 2.0 * INSET).max(1.0);
    let mut out: Vec<Vec<u8>> = Vec::new();
    for para in text.split('\n') {
        let encoded = encode_for_font(para);
        let mut line: Vec<u8> = Vec::new();
        for word in encoded.split(|&b| b == b' ') {
            let mut candidate = line.clone();
            if !candidate.is_empty() {
                candidate.push(b' ');
            }
            candidate.extend_from_slice(word);
            if line.is_empty() || run_width(&candidate, family, font_size) <= inner {
                line = candidate;
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_vec();
            }
        }
        out.push(line);
    }
    out
}

/// The appearance-stream content painting the note's wrapped text top-down from
/// the box top, in the chosen font, size, and fill color.
fn build_appearance_content(annot: &TypewriterAnnot) -> Result<Vec<u8>, AppError> {
    let lines = wrap_lines(&annot.text, &annot.font_family, annot.font_size, annot.width);
    let leading = annot.font_size * LINE_HEIGHT_RATIO;
    let first_baseline = annot.height - INSET - annot.font_size * ASCENT_RATIO;
    let [r, g, b] = annot.color;

    let mut ops = vec![
        Operation::new("BT", vec![]),
        Operation::new(
            "Tf",
            vec![Object::Name(FONT_RES.as_bytes().to_vec()), Object::Real(annot.font_size)],
        ),
        Operation::new("rg", vec![Object::Real(r), Object::Real(g), Object::Real(b)]),
        Operation::new("Td", vec![Object::Real(INSET), Object::Real(first_baseline)]),
    ];
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            ops.push(Operation::new("Td", vec![Object::Real(0.0), Object::Real(-leading)]));
        }
        ops.push(Operation::new(
            "Tj",
            vec![Object::String(line.clone(), StringFormat::Literal)],
        ));
    }
    ops.push(Operation::new("ET", vec![]));

    Content { operations: ops }
        .encode()
        .map_err(|e| AppError::lopdf("Failed to encode typewriter appearance", e))
}

/// The `/DA` (default appearance) string a re-editing viewer uses to regenerate
/// the appearance: font resource, size, and RGB fill.
fn default_appearance(annot: &TypewriterAnnot) -> Object {
    let [r, g, b] = annot.color;
    let da = format!("/{FONT_RES} {} Tf {r} {g} {b} rg", annot.font_size);
    Object::String(da.into_bytes(), StringFormat::Literal)
}

/// Encodes a note's text as a PDF text string: an ASCII literal, or UTF-16BE
/// with a BOM otherwise. Stores the *full* Unicode text for lossless re-editing,
/// even though the drawn appearance is limited to WinAnsi.
fn pdf_text_string(s: &str) -> Object {
    if s.is_ascii() {
        Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
    } else {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in s.encode_utf16() {
            bytes.push((unit >> 8) as u8);
            bytes.push((unit & 0xFF) as u8);
        }
        Object::String(bytes, StringFormat::Literal)
    }
}

/// Decodes a PDF text string written by [`pdf_text_string`] (or any reader):
/// UTF-16BE when it carries a BOM, otherwise a Latin-1/ASCII literal.
fn decode_pdf_text_string(obj: &Object) -> String {
    let Ok(bytes) = obj.as_str() else {
        return String::new();
    };
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

// ── Page geometry ────────────────────────────────────────────────────────────

fn object_as_f32(obj: &Object) -> f32 {
    match obj {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
    }
}

/// Resolves a page's (possibly inherited) MediaBox as `[x0, y0, x1, y1]`,
/// following `/Parent` up the page tree. `None` if unreadable.
fn inherited_media_box(doc: &Document, page_id: ObjectId) -> Option<[f32; 4]> {
    let mut current = page_id;
    for _ in 0..64 {
        let dict = doc.get_object(current).ok()?.as_dict().ok()?;
        if let Ok(value) = dict.get(b"MediaBox") {
            let arr = match value {
                Object::Reference(r) => doc.get_object(*r).ok()?.as_array().ok()?,
                Object::Array(a) => a,
                _ => return None,
            };
            if arr.len() >= 4 {
                return Some([
                    object_as_f32(&arr[0]),
                    object_as_f32(&arr[1]),
                    object_as_f32(&arr[2]),
                    object_as_f32(&arr[3]),
                ]);
            }
        }
        current = dict.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

/// Page origin and size in points: `(origin_x, origin_y, width, height)`.
/// Falls back to US Letter if the MediaBox can't be read.
fn page_media_box(doc: &Document, page_id: ObjectId) -> (f32, f32, f32, f32) {
    match inherited_media_box(doc, page_id) {
        Some([x0, y0, x1, y1]) => (x0, y0, (x1 - x0).abs(), (y1 - y0).abs()),
        None => (0.0, 0.0, 612.0, 792.0),
    }
}

// ── Annotation list plumbing ─────────────────────────────────────────────────

/// The indirect references in a page's `/Annots`, normalized across its shapes
/// (a `Reference` to an array, an inline `Array`, or missing → empty). Inline
/// annotation dictionaries — which we never author — are skipped.
fn page_annot_refs(doc: &Document, page_id: ObjectId) -> Vec<ObjectId> {
    let Some(page) = doc.get_object(page_id).ok().and_then(|o| o.as_dict().ok()) else {
        return Vec::new();
    };
    match page.get(b"Annots") {
        Ok(Object::Reference(r)) => doc
            .get_object(*r)
            .ok()
            .and_then(|o| o.as_array().ok())
            .map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect())
            .unwrap_or_default(),
        Ok(Object::Array(a)) => a.iter().filter_map(|o| o.as_reference().ok()).collect(),
        _ => Vec::new(),
    }
}

/// Whether an annotation object is one Tumbler authored (carries `/TWid`).
fn is_tumbler_annot(doc: &Document, id: ObjectId) -> bool {
    doc.get_object(id)
        .ok()
        .and_then(|o| o.as_dict().ok())
        .map(|d| d.has(TW_ID_KEY))
        .unwrap_or(false)
}

/// The appearance-stream object id referenced by an annotation's `/AP /N`.
fn annot_ap_ref(doc: &Document, id: ObjectId) -> Option<ObjectId> {
    let dict = doc.get_object(id).ok()?.as_dict().ok()?;
    let ap = dict.get(b"AP").ok()?.as_dict().ok()?;
    ap.get(b"N").ok()?.as_reference().ok()
}

/// Removes every Tumbler-authored FreeText annotation from every page (dropping
/// its object and its appearance stream), leaving foreign annotations intact.
/// Returns how many were removed — so an all-empty apply that had nothing of
/// ours to remove can be a no-op. Making apply idempotent this way means the
/// frontend always sends the *full* current note set and re-apply neither
/// duplicates nor strands old copies.
fn remove_tumbler_annots(doc: &mut Document) -> usize {
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let mut removed = 0usize;
    let mut to_delete: Vec<ObjectId> = Vec::new();
    let mut page_updates: Vec<(ObjectId, Vec<ObjectId>)> = Vec::new();

    for page_id in page_ids {
        let refs = page_annot_refs(doc, page_id);
        if refs.is_empty() {
            continue;
        }
        let mut kept = Vec::new();
        let mut changed = false;
        for r in refs {
            if is_tumbler_annot(doc, r) {
                removed += 1;
                changed = true;
                if let Some(ap) = annot_ap_ref(doc, r) {
                    to_delete.push(ap);
                }
                to_delete.push(r);
            } else {
                kept.push(r);
            }
        }
        if changed {
            page_updates.push((page_id, kept));
        }
    }

    for (page_id, kept) in page_updates {
        if let Ok(page) = doc.get_object_mut(page_id).and_then(|o| o.as_dict_mut()) {
            if kept.is_empty() {
                page.remove(b"Annots");
            } else {
                page.set("Annots", Object::Array(kept.into_iter().map(Object::Reference).collect()));
            }
        }
    }
    for id in to_delete {
        doc.objects.remove(&id);
    }
    removed
}

/// Adds one FreeText annotation per note, appending it to its page's `/Annots`.
/// Font objects are shared across notes with the same base font.
fn add_tumbler_annots(doc: &mut Document, annots: &[TypewriterAnnot]) -> Result<(), AppError> {
    let pages = doc.get_pages();
    let mut font_ids: HashMap<&'static str, ObjectId> = HashMap::new();
    let mut per_page: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();

    for annot in annots {
        let Some(&page_id) = pages.get(&annot.page) else {
            continue; // page out of range — drop rather than error
        };
        let (ox, oy, _w, h) = page_media_box(doc, page_id);
        // Top-left origin (frontend) → bottom-left user space.
        let x1 = ox + annot.x;
        let x2 = ox + annot.x + annot.width;
        let y2 = oy + h - annot.y;
        let y1 = oy + h - (annot.y + annot.height);

        let base = base_font_name(&annot.font_family, annot.bold, annot.italic);
        let font_id = *font_ids.entry(base).or_insert_with(|| {
            doc.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => base,
                "Encoding" => "WinAnsiEncoding",
            })
        });

        let ap_content = build_appearance_content(annot)?;
        let ap_dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "FormType" => Object::Integer(1),
            "BBox" => Object::Array(vec![
                Object::Real(0.0), Object::Real(0.0),
                Object::Real(annot.width), Object::Real(annot.height),
            ]),
            "Resources" => dictionary! {
                "Font" => dictionary! { FONT_RES => Object::Reference(font_id) },
            },
        };
        let ap_id = doc.add_object(Object::Stream(Stream::new(ap_dict, ap_content)));

        let annot_dict = dictionary! {
            "Type" => "Annot",
            "Subtype" => "FreeText",
            "Rect" => Object::Array(vec![
                Object::Real(x1), Object::Real(y1), Object::Real(x2), Object::Real(y2),
            ]),
            "Contents" => pdf_text_string(&annot.text),
            "DA" => default_appearance(annot),
            "F" => Object::Integer(4), // Print
            "AP" => dictionary! { "N" => Object::Reference(ap_id) },
            "BS" => dictionary! { "W" => Object::Integer(0), "S" => "S" },
            // Private round-trip keys (see TW_ID_KEY).
            "TWid" => Object::String(annot.id.clone().into_bytes(), StringFormat::Literal),
            "TWfam" => Object::Name(annot.font_family.clone().into_bytes()),
            "TWbold" => Object::Boolean(annot.bold),
            "TWitalic" => Object::Boolean(annot.italic),
            "TWsize" => Object::Real(annot.font_size),
            "TWcolor" => Object::Array(vec![
                Object::Real(annot.color[0]),
                Object::Real(annot.color[1]),
                Object::Real(annot.color[2]),
            ]),
        };
        let annot_id = doc.add_object(annot_dict);
        per_page.entry(page_id).or_default().push(annot_id);
    }

    for (page_id, new_refs) in per_page {
        let mut all: Vec<Object> =
            page_annot_refs(doc, page_id).into_iter().map(Object::Reference).collect();
        all.extend(new_refs.into_iter().map(Object::Reference));
        let page = doc
            .get_object_mut(page_id)
            .and_then(|o| o.as_dict_mut())
            .map_err(|e| AppError::lopdf("Failed to update page /Annots", e))?;
        page.set("Annots", Object::Array(all));
    }
    Ok(())
}

// ── Invisible page-text layer (search + selection) ───────────────────────────
//
// A FreeText annotation's text is not part of the page content stream, so
// pdfium — which drives Tumbler's search and text selection — never sees it.
// To make notes searchable and selectable we also embed each note's text into
// the page as an **invisible** (text render mode 3) content run, exactly like
// the OCR "sandwich" (see [`crate::commands::text_layer`]): pdfium extracts it
// but never paints it, so it doesn't double the visible overlay/appearance. A
// non-Helvetica note's run still uses Helvetica metrics — invisible text is
// only ever extracted, never seen, so the exact glyph shapes don't matter.

/// Builds the invisible content stream for all notes on one page: one run per
/// wrapped line, positioned in page user space so its extraction box lands on
/// the visible note text. Empty when no note has representable text.
fn build_text_layer_content(
    annots: &[&TypewriterAnnot],
    page_height: f32,
    ox: f32,
    oy: f32,
) -> Result<Vec<u8>, AppError> {
    // `q`/`ET`-wrapped so our text state can't leak into (or inherit from) the
    // page's own content beyond a balanced default state.
    let mut ops = vec![
        Operation::new("q", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tr", vec![Object::Integer(3)]), // invisible
    ];
    let mut any = false;
    for annot in annots {
        let lines = wrap_lines(&annot.text, &annot.font_family, annot.font_size, annot.width);
        let leading = annot.font_size * LINE_HEIGHT_RATIO;
        ops.push(Operation::new(
            "Tf",
            vec![
                Object::Name(TEXT_LAYER_FONT_RES.as_bytes().to_vec()),
                Object::Real(annot.font_size),
            ],
        ));
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            any = true;
            let y_tl = annot.y + INSET + annot.font_size * ASCENT_RATIO + i as f32 * leading;
            let x = ox + annot.x + INSET;
            let y = oy + page_height - y_tl;
            ops.push(Operation::new(
                "Tm",
                vec![
                    Object::Real(1.0), Object::Real(0.0), Object::Real(0.0),
                    Object::Real(1.0), Object::Real(x), Object::Real(y),
                ],
            ));
            ops.push(Operation::new(
                "Tj",
                vec![Object::String(line.clone(), StringFormat::Literal)],
            ));
        }
    }
    ops.push(Operation::new("ET", vec![]));
    ops.push(Operation::new("Q", vec![]));
    if !any {
        return Ok(Vec::new());
    }
    Content { operations: ops }
        .encode()
        .map_err(|e| AppError::lopdf("Failed to encode typewriter text layer", e))
}

/// Removes the invisible typewriter text layer from every page (our tagged
/// content streams), leaving the page's own content intact. Returns how many
/// were removed.
fn remove_tumbler_text_layer(doc: &mut Document) -> usize {
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let mut removed = 0usize;
    let mut to_delete: Vec<ObjectId> = Vec::new();
    let mut page_updates: Vec<(ObjectId, Vec<ObjectId>)> = Vec::new();

    for page_id in page_ids {
        let refs = contents_refs(doc, page_id);
        if refs.is_empty() {
            continue;
        }
        let mut kept = Vec::new();
        let mut changed = false;
        for r in refs {
            let ours = doc
                .get_object(r)
                .ok()
                .and_then(|o| o.as_stream().ok())
                .map(|s| s.dict.has(TEXT_LAYER_TAG))
                .unwrap_or(false);
            if ours {
                removed += 1;
                changed = true;
                to_delete.push(r);
            } else {
                kept.push(r);
            }
        }
        if changed {
            page_updates.push((page_id, kept));
        }
    }

    for (page_id, kept) in page_updates {
        if let Ok(page) = doc.get_object_mut(page_id).and_then(|o| o.as_dict_mut()) {
            match kept.len() {
                0 => { page.remove(b"Contents"); }
                1 => { page.set("Contents", Object::Reference(kept[0])); }
                _ => {
                    page.set("Contents", Object::Array(kept.into_iter().map(Object::Reference).collect()));
                }
            }
        }
    }
    for id in to_delete {
        doc.objects.remove(&id);
    }
    removed
}

/// Appends the invisible text layer for the given notes, one tagged content
/// stream per page, and merges the shared invisible font into each page's
/// resources.
fn add_tumbler_text_layer(doc: &mut Document, annots: &[TypewriterAnnot]) -> Result<(), AppError> {
    let pages = doc.get_pages();
    let mut by_page: HashMap<u32, Vec<&TypewriterAnnot>> = HashMap::new();
    for annot in annots {
        if pages.contains_key(&annot.page) {
            by_page.entry(annot.page).or_default().push(annot);
        }
    }
    if by_page.is_empty() {
        return Ok(());
    }

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });

    for (page_num, page_annots) in by_page {
        let page_id = pages[&page_num];
        let (ox, oy, _w, h) = page_media_box(doc, page_id);
        let content = build_text_layer_content(&page_annots, h, ox, oy)?;
        if content.is_empty() {
            continue;
        }
        let resources = merged_resources_with_font(doc, page_id, TEXT_LAYER_FONT_RES, font_id);
        let existing = contents_refs(doc, page_id);

        let mut stream_dict = Dictionary::new();
        stream_dict.set(TEXT_LAYER_TAG, Object::Boolean(true));
        let stream_id = doc.add_object(Object::Stream(Stream::new(stream_dict, content)));

        let mut refs: Vec<Object> = existing.into_iter().map(Object::Reference).collect();
        refs.push(Object::Reference(stream_id));

        let page = doc
            .get_object_mut(page_id)
            .and_then(|o| o.as_dict_mut())
            .map_err(|e| AppError::lopdf("Failed to update page /Contents", e))?;
        page.set("Contents", Object::Array(refs));
        page.set("Resources", Object::Dictionary(resources));
    }
    Ok(())
}

/// Replaces Tumbler's typewriter notes in `buffer` with `annots`, returning the
/// new document bytes — or `None` when nothing changed (no notes to add and none
/// of ours to remove), so the caller can skip a needless dirtying reserialize.
///
/// Each note is written twice: as a visible FreeText annotation (for other
/// readers) and as an invisible page-text run (so Tumbler's search and text
/// selection, which read the page content stream, find it).
pub fn write_typewriter_annots(
    buffer: &[u8],
    annots: &[TypewriterAnnot],
) -> Result<Option<Vec<u8>>, AppError> {
    let mut doc = Document::load_mem(buffer)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for typewriter", e))?;
    let removed = remove_tumbler_annots(&mut doc) + remove_tumbler_text_layer(&mut doc);
    if annots.is_empty() && removed == 0 {
        return Ok(None);
    }
    add_tumbler_annots(&mut doc, annots)?;
    add_tumbler_text_layer(&mut doc, annots)?;
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize typewriter annotations", e))?;
    Ok(Some(out))
}

/// Reconstructs a note from one of our FreeText annotation dictionaries.
fn reconstruct(dict: &Dictionary, page: u32, ox: f32, oy: f32, page_height: f32) -> Option<TypewriterAnnot> {
    let id = String::from_utf8_lossy(dict.get(TW_ID_KEY).ok()?.as_str().ok()?).into_owned();
    let rect = dict.get(b"Rect").ok()?.as_array().ok()?;
    if rect.len() < 4 {
        return None;
    }
    let (x1, y1, x2, y2) = (
        object_as_f32(&rect[0]),
        object_as_f32(&rect[1]),
        object_as_f32(&rect[2]),
        object_as_f32(&rect[3]),
    );
    let color = dict
        .get(b"TWcolor")
        .ok()
        .and_then(|o| o.as_array().ok())
        .filter(|a| a.len() >= 3)
        .map(|a| [object_as_f32(&a[0]), object_as_f32(&a[1]), object_as_f32(&a[2])])
        .unwrap_or([0.0, 0.0, 0.0]);
    let font_family = dict
        .get(b"TWfam")
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_else(|| "Helvetica".to_string());
    let dict_bool = |key: &[u8]| matches!(dict.get(key), Ok(Object::Boolean(true)));

    Some(TypewriterAnnot {
        id,
        page,
        x: x1 - ox,
        y: (oy + page_height) - y2,
        width: (x2 - x1).abs(),
        height: (y2 - y1).abs(),
        text: dict.get(b"Contents").ok().map(decode_pdf_text_string).unwrap_or_default(),
        font_family,
        bold: dict_bool(b"TWbold"),
        italic: dict_bool(b"TWitalic"),
        font_size: dict.get(b"TWsize").ok().map(object_as_f32).unwrap_or(12.0),
        color,
    })
}

/// Reads back every Tumbler-authored typewriter note in `buffer`, in page order.
pub fn read_typewriter_annots(buffer: &[u8]) -> Result<Vec<TypewriterAnnot>, AppError> {
    let doc = Document::load_mem(buffer)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for typewriter read", e))?;
    let mut out = Vec::new();
    for (page_num, page_id) in doc.get_pages() {
        let (ox, oy, _w, h) = page_media_box(&doc, page_id);
        for r in page_annot_refs(&doc, page_id) {
            let Some(dict) = doc.get_object(r).ok().and_then(|o| o.as_dict().ok()) else {
                continue;
            };
            if !dict.has(TW_ID_KEY) {
                continue;
            }
            if let Some(annot) = reconstruct(dict, page_num, ox, oy, h) {
                out.push(annot);
            }
        }
    }
    Ok(out)
}

// ── Commands ─────────────────────────────────────────────────────────────────

fn apply_typewriter_impl(
    state: &AppState,
    doc_id: String,
    annots: Vec<TypewriterAnnot>,
) -> Result<bool, AppError> {
    let entry = state.get_document(&doc_id)?;
    let buffer = {
        let entry = lock_mutex(&entry)?;
        entry.buffer.clone()
    };
    match write_typewriter_annots(&buffer, &annots)? {
        Some(bytes) => {
            state.set_buffer_and_refresh(&doc_id, bytes)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Writes the given typewriter notes into the document buffer as FreeText
/// annotations (replacing any Tumbler wrote before) and marks it dirty. A
/// buffer edit (issue #31): nothing touches disk until the user saves.
#[tauri::command]
pub fn apply_typewriter(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    annots: Vec<TypewriterAnnot>,
) -> Result<(), String> {
    let changed = apply_typewriter_impl(&state, doc_id.clone(), annots).map_err(String::from)?;
    if changed {
        let _ = app.emit(
            "document-dirty-changed",
            dirty_changed_payload(&state, doc_id, true),
        );
    }
    Ok(())
}

/// Reads back the typewriter notes stored in the document buffer, so the
/// frontend can re-hydrate its editable overlay when a file is (re)opened.
#[tauri::command]
pub fn read_typewriter(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<Vec<TypewriterAnnot>, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let buffer = {
        let entry = lock_mutex(&entry).map_err(String::from)?;
        entry.buffer.clone()
    };
    read_typewriter_annots(&buffer).map_err(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_annot() -> TypewriterAnnot {
        TypewriterAnnot {
            id: "note-1".to_string(),
            page: 1,
            x: 20.0,
            y: 30.0,
            width: 120.0,
            height: 40.0,
            text: "Hello world".to_string(),
            font_family: "Helvetica".to_string(),
            bold: false,
            italic: false,
            font_size: 12.0,
            color: [0.0, 0.0, 1.0],
        }
    }

    fn fixture_bytes() -> Vec<u8> {
        std::fs::read(crate::fixture_path()).expect("read fixture")
    }

    #[test]
    fn base_font_name_maps_families_and_styles() {
        assert_eq!(base_font_name("Helvetica", false, false), "Helvetica");
        assert_eq!(base_font_name("Helvetica", true, true), "Helvetica-BoldOblique");
        assert_eq!(base_font_name("Times", false, false), "Times-Roman");
        assert_eq!(base_font_name("Times", true, true), "Times-BoldItalic");
        assert_eq!(base_font_name("Courier", false, true), "Courier-Oblique");
        // Unknown family falls back to Helvetica.
        assert_eq!(base_font_name("Comic Sans", false, false), "Helvetica");
    }

    #[test]
    fn wrap_lines_breaks_on_width_and_preserves_newlines() {
        // A tall-enough font in a narrow box forces wrapping.
        let lines = wrap_lines("alpha beta gamma", "Helvetica", 12.0, 60.0);
        assert!(lines.len() > 1, "expected wrapping, got {lines:?}");

        // Explicit newlines become separate lines.
        let lines = wrap_lines("one\ntwo", "Helvetica", 12.0, 400.0);
        assert_eq!(lines, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn text_string_round_trips_ascii_and_unicode() {
        assert_eq!(decode_pdf_text_string(&pdf_text_string("Plain")), "Plain");
        assert_eq!(decode_pdf_text_string(&pdf_text_string("café ☂")), "café ☂");
    }

    #[test]
    fn write_adds_a_freetext_annotation_readable_by_pdfium() {
        let bytes = write_typewriter_annots(&fixture_bytes(), &[sample_annot()])
            .expect("write")
            .expect("some bytes");

        // pdfium can still open the edited bytes.
        let pdfium = crate::test_pdfium();
        let _guard = crate::test_pdfium_guard();
        pdfium
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .expect("pdfium opens edited bytes");

        // The FreeText annotation exists with our marker and content.
        let doc = Document::load_mem(&bytes).expect("reparse");
        let page_id = *doc.get_pages().get(&1).expect("page 1");
        let refs = page_annot_refs(&doc, page_id);
        assert_eq!(refs.len(), 1, "one annotation added");
        let dict = doc.get_object(refs[0]).unwrap().as_dict().unwrap();
        assert_eq!(dict.get(b"Subtype").unwrap().as_name().unwrap(), b"FreeText");
        assert!(dict.has(TW_ID_KEY));
        assert!(dict.get(b"AP").is_ok(), "has an appearance stream");
    }

    #[test]
    fn note_text_is_extractable_by_pdfium() {
        // The invisible page-text layer is what makes a note searchable and
        // selectable: pdfium (which drives both) must extract the note's text.
        let bytes = write_typewriter_annots(&fixture_bytes(), &[sample_annot()])
            .expect("write")
            .expect("some bytes");

        let pdfium = crate::test_pdfium();
        let _guard = crate::test_pdfium_guard();
        let doc = pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .expect("pdfium opens edited bytes");
        let page = doc.pages().get(0).expect("page 0");
        let text = page.text().expect("page text").all();
        assert!(text.contains("Hello world"), "note text missing from extraction: {text:?}");
    }

    #[test]
    fn clearing_notes_removes_the_extractable_text() {
        let with_note = write_typewriter_annots(&fixture_bytes(), &[sample_annot()])
            .expect("write")
            .expect("bytes");
        let cleared = write_typewriter_annots(&with_note, &[])
            .expect("clear")
            .expect("bytes");

        let pdfium = crate::test_pdfium();
        let _guard = crate::test_pdfium_guard();
        let doc = pdfium.load_pdf_from_byte_vec(cleared, None).expect("open");
        let page = doc.pages().get(0).expect("page 0");
        let text = page.text().expect("page text").all();
        assert!(!text.contains("Hello world"), "note text should be gone: {text:?}");
        // The page's own text survives.
        assert!(text.contains("Test Fixture"), "original text lost: {text:?}");
    }

    #[test]
    fn round_trip_read_returns_what_was_written() {
        let annot = sample_annot();
        let bytes = write_typewriter_annots(&fixture_bytes(), &[annot.clone()])
            .expect("write")
            .expect("some bytes");
        let read = read_typewriter_annots(&bytes).expect("read");
        assert_eq!(read.len(), 1);
        let got = &read[0];
        assert_eq!(got.id, annot.id);
        assert_eq!(got.text, annot.text);
        assert_eq!(got.font_family, annot.font_family);
        assert_eq!(got.font_size, annot.font_size);
        assert_eq!(got.color, annot.color);
        // Coordinates survive the top-left ↔ bottom-left flip (fixture is 200×200).
        assert!((got.x - annot.x).abs() < 0.5, "x {} vs {}", got.x, annot.x);
        assert!((got.y - annot.y).abs() < 0.5, "y {} vs {}", got.y, annot.y);
        assert!((got.width - annot.width).abs() < 0.5);
        assert!((got.height - annot.height).abs() < 0.5);
    }

    #[test]
    fn reapply_replaces_rather_than_duplicates() {
        let first = write_typewriter_annots(&fixture_bytes(), &[sample_annot()])
            .expect("write")
            .expect("bytes");

        // Re-apply with an edited note: still exactly one, with the new text.
        let mut edited = sample_annot();
        edited.text = "Replaced".to_string();
        let second = write_typewriter_annots(&first, &[edited])
            .expect("rewrite")
            .expect("bytes");

        let read = read_typewriter_annots(&second).expect("read");
        assert_eq!(read.len(), 1, "re-apply must not duplicate");
        assert_eq!(read[0].text, "Replaced");
    }

    #[test]
    fn empty_apply_clears_previous_notes() {
        let with_note = write_typewriter_annots(&fixture_bytes(), &[sample_annot()])
            .expect("write")
            .expect("bytes");
        // Applying an empty set removes ours and yields new bytes.
        let cleared = write_typewriter_annots(&with_note, &[])
            .expect("clear")
            .expect("bytes (removal happened)");
        assert!(read_typewriter_annots(&cleared).expect("read").is_empty());

        // With no notes present and none to add, it's a no-op (no reserialize).
        assert!(
            write_typewriter_annots(&cleared, &[]).expect("noop").is_none(),
            "nothing to do → None"
        );
    }

    #[test]
    fn foreign_annotations_are_preserved() {
        // Add a non-Tumbler annotation, then apply/clear our notes around it.
        let mut doc = Document::load_mem(&fixture_bytes()).expect("parse");
        let page_id = *doc.get_pages().get(&1).expect("page");
        let foreign = doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Text",
            "Rect" => Object::Array(vec![Object::Real(0.0); 4]),
        });
        doc.get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Annots", Object::Array(vec![Object::Reference(foreign)]));
        let mut base = Vec::new();
        doc.save_to(&mut base).expect("serialize");

        let with_ours = write_typewriter_annots(&base, &[sample_annot()])
            .expect("write")
            .expect("bytes");
        let cleared = write_typewriter_annots(&with_ours, &[])
            .expect("clear")
            .expect("bytes");

        // The foreign Text annotation is still on the page after clearing ours.
        let doc = Document::load_mem(&cleared).expect("reparse");
        let page_id = *doc.get_pages().get(&1).expect("page");
        let kinds: Vec<Vec<u8>> = page_annot_refs(&doc, page_id)
            .iter()
            .filter_map(|r| doc.get_object(*r).ok().and_then(|o| o.as_dict().ok()))
            .filter_map(|d| d.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).map(|n| n.to_vec()))
            .collect();
        assert_eq!(kinds, vec![b"Text".to_vec()], "foreign annot survives");
    }
}

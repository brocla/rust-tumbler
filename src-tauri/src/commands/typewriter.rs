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
//! **top-left** origin (the same space as search/redaction rects), measured
//! against the page as pdfium drew it. [`PageSpace`] flips it to PDF user
//! space (bottom-left), accounting for the render box and the page's
//! `/Rotate` (issue #121).
//!
//! Rotation lands differently here than it does for ink, which flattens into
//! the content stream and only has to move its points. A note is an
//! *annotation*: its `/Rect` is in unrotated user space — so at 90 and 270 a
//! wide, short note box becomes a tall, narrow rect — while the appearance
//! stream is drawn upright in its own space and turned into place by the
//! form's `/Matrix`, which is what keeps the text level on screen after the
//! viewer applies the page rotation. The invisible page-text run carries the
//! same turn in its text matrix, so search and selection agree with what is
//! drawn.

use crate::commands::page_space::PageSpace;
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

pub(crate) fn object_as_f32(obj: &Object) -> f32 {
    match obj {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
    }
}

// ── Annotation list plumbing ─────────────────────────────────────────────────

/// The indirect references in a page's `/Annots`, normalized across its shapes
/// (a `Reference` to an array, an inline `Array`, or missing → empty). Inline
/// annotation dictionaries — which we never author — are skipped.
pub(crate) fn page_annot_refs(doc: &Document, page_id: ObjectId) -> Vec<ObjectId> {
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
        // Top-left origin (frontend) → bottom-left, unrotated user space.
        // On a quarter-turned page this is where the note's width and height
        // exchange: the rect is as tall as the box is wide.
        let space = PageSpace::of(doc, page_id);
        let [x1, y1, x2, y2] = space.rect_to_user(annot.x, annot.y, annot.width, annot.height);

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
            // The appearance is drawn upright in its own space; this turns it
            // to counter the page rotation, so the viewer's own turn brings
            // the text back level. A viewer maps the transformed BBox onto
            // /Rect (PDF 32000-1 §12.5.5), and because the same rotation is
            // in both, the two agree in size and the note lands square.
            "Matrix" => Object::Array(
                space.upright_matrix().iter().map(|v| Object::Real(*v)).collect(),
            ),
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
    space: &PageSpace,
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
            let [x, y] = space.to_user([annot.x + INSET, y_tl]);
            // The linear part turns the run to counter the page rotation, so
            // the extracted glyph boxes sit under the visible text rather than
            // running off across the page at right angles to it.
            let [a, b, c, d, _, _] = space.upright_matrix();
            ops.push(Operation::new(
                "Tm",
                vec![
                    Object::Real(a), Object::Real(b), Object::Real(c),
                    Object::Real(d), Object::Real(x), Object::Real(y),
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
        let content = build_text_layer_content(&page_annots, &PageSpace::of(doc, page_id))?;
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
fn reconstruct(dict: &Dictionary, page: u32, space: &PageSpace) -> Option<TypewriterAnnot> {
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

    // Back through the same mapping, so a note re-hydrates at the coordinates
    // it was placed at — including the width/height exchange on a quarter-
    // turned page, which a hand-written inverse gets backwards.
    let (x, y, width, height) = space.rect_to_render([
        x1.min(x2),
        y1.min(y2),
        x1.max(x2),
        y1.max(y2),
    ]);

    Some(TypewriterAnnot {
        id,
        page,
        x,
        y,
        width,
        height,
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
        let space = PageSpace::of(&doc, page_id);
        for r in page_annot_refs(&doc, page_id) {
            let Some(dict) = doc.get_object(r).ok().and_then(|o| o.as_dict().ok()) else {
                continue;
            };
            if !dict.has(TW_ID_KEY) {
                continue;
            }
            if let Some(annot) = reconstruct(dict, page_num, &space) {
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
        pdfium.get()
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
        let doc = pdfium.get()
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
        let doc = pdfium.get().load_pdf_from_byte_vec(cleared, None).expect("open");
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

    /// True for a pixel dark enough to be painted glyph rather than paper.
    fn is_glyph(px: &[u8]) -> bool {
        px[0] < 128 && px[1] < 128 && px[2] < 128
    }

    /// A note whose text nearly fills its box, so the rendered glyph run is a
    /// tight, predictable stand-in for the box itself. Capital H is a full-
    /// height, full-width glyph with no descender, which keeps the painted
    /// area square to the box.
    fn probe_note(x: f32, y: f32) -> TypewriterAnnot {
        TypewriterAnnot {
            text: "HHHHHHHHHHHHH".to_string(),
            color: [0.0, 0.0, 0.0],
            x,
            y,
            width: 120.0,
            height: 20.0,
            ..sample_annot()
        }
    }

    /// Where [`probe_note`]'s glyphs must land, in render space, for a note
    /// placed at `(x, y)`. Derived from the layout constants rather than
    /// hard-coded, so a deliberate change to the insets moves the expectation
    /// with it: text starts one inset in, and the first line's cap height
    /// hangs between the inset and the baseline drop.
    fn probe_glyph_box(x: f32, y: f32) -> [f32; 4] {
        const CAP_HEIGHT_RATIO: f32 = 0.717; // Helvetica capital H
        let size = sample_annot().font_size;
        let run = run_width(b"HHHHHHHHHHHHH", "Helvetica", size);
        let top = y + INSET + size * ASCENT_RATIO - size * CAP_HEIGHT_RATIO;
        [x + INSET, top, x + INSET + run, top + size * CAP_HEIGHT_RATIO]
    }

    /// Glyph rasterization is not pixel-exact and the probe leans on nominal
    /// Helvetica metrics, which drift about 0.2pt per glyph against pdfium's
    /// rasterized advances — so a 13-glyph run's right edge is the loosest
    /// component here. Still far tighter than any misplacement being guarded
    /// against: the crop origin is tens of points, a rotation error hundreds.
    const GLYPH_TOL: f32 = 3.5;

    /// pdfium renders the **CropBox**, so the coordinates the frontend sends
    /// are relative to that crop. Measuring from the MediaBox instead put a
    /// note off by exactly the crop origin on any cropped page — and plenty of
    /// real scans have a CropBox strictly inside their MediaBox.
    #[test]
    fn notes_are_placed_against_the_cropbox_not_the_mediabox() {
        let pdfium = crate::test_pdfium();
        // A 200x400 sheet cropped to a 160x340 window at a non-zero origin.
        let page = crate::geometry_page_bytes(200.0, 400.0, 0, Some([15.0, 25.0, 175.0, 365.0]));
        let bytes = write_typewriter_annots(&page, &[probe_note(20.0, 30.0)])
            .expect("write")
            .expect("bytes");

        crate::assert_mark_landed(
            crate::rendered_mark_bbox(pdfium.get(), bytes, true, is_glyph),
            probe_glyph_box(20.0, 30.0),
            GLYPH_TOL,
            "cropped page",
        );
    }

    /// The read-back path shares the same box choice, so a note written to a
    /// cropped page must re-hydrate at the coordinates it was placed at rather
    /// than drifting by the crop each time the file is reopened.
    #[test]
    fn notes_on_a_cropped_page_round_trip_their_coordinates() {
        let page = crate::geometry_page_bytes(200.0, 400.0, 0, Some([15.0, 25.0, 175.0, 365.0]));
        let bytes = write_typewriter_annots(&page, &[probe_note(20.0, 30.0)])
            .expect("write")
            .expect("bytes");

        let read = read_typewriter_annots(&bytes).expect("read");
        assert_eq!(read.len(), 1);
        assert!((read[0].x - 20.0).abs() < 0.5, "x drifted: {}", read[0].x);
        assert!((read[0].y - 30.0).abs() < 0.5, "y drifted: {}", read[0].y);
    }

    /// **The bug in issue #121.** A note is placed against the page pdfium
    /// rendered; on a `/Rotate` page that render is turned, and at 90 and 270
    /// its width and height are swapped relative to user space. Written
    /// without accounting for that, the note lands somewhere else on the page
    /// — or off it — while the annotation is present, well-formed and
    /// extractable, so every other test here still passes.
    ///
    /// The page is 200x400, deliberately not square: on a square page the
    /// 90/270 swap cancels and this would pass against the broken code. The
    /// note is placed off-centre, so 0 and 180 are told apart too.
    #[test]
    fn notes_land_where_they_were_placed_on_a_rotated_page() {
        let pdfium = crate::test_pdfium();

        for rotate in [0, 90, 180, 270] {
            let page = crate::geometry_page_bytes(200.0, 400.0, rotate, None);
            let bytes = write_typewriter_annots(&page, &[probe_note(20.0, 30.0)])
                .expect("write")
                .expect("bytes");

            crate::assert_mark_landed(
                crate::rendered_mark_bbox(pdfium.get(), bytes, true, is_glyph),
                probe_glyph_box(20.0, 30.0),
                GLYPH_TOL,
                &format!("/Rotate {rotate}"),
            );
        }
    }

    /// Rotation and a crop together: pdfium renders the CropBox and *then*
    /// turns it, so the origin offset belongs in user space before the turn.
    #[test]
    fn notes_land_correctly_on_a_page_that_is_both_cropped_and_rotated() {
        let pdfium = crate::test_pdfium();
        let crop = Some([15.0, 25.0, 175.0, 365.0]);

        for rotate in [0, 90, 180, 270] {
            let page = crate::geometry_page_bytes(200.0, 400.0, rotate, crop);
            let bytes = write_typewriter_annots(&page, &[probe_note(20.0, 30.0)])
                .expect("write")
                .expect("bytes");

            crate::assert_mark_landed(
                crate::rendered_mark_bbox(pdfium.get(), bytes, true, is_glyph),
                probe_glyph_box(20.0, 30.0),
                GLYPH_TOL,
                &format!("cropped, /Rotate {rotate}"),
            );
        }
    }

    /// The glyph box above pins position; this pins *orientation*. A note that
    /// lands in the right place but on its side is still wrong, and the two
    /// failures are separable: the `/Rect` can be correct while the appearance
    /// `/Matrix` is not. A run of capital H's is far wider than it is tall, so
    /// a quarter-turned appearance inverts the comparison.
    #[test]
    fn note_text_reads_upright_whatever_the_page_rotation() {
        let pdfium = crate::test_pdfium();

        for rotate in [0, 90, 180, 270] {
            let page = crate::geometry_page_bytes(200.0, 400.0, rotate, None);
            let bytes = write_typewriter_annots(&page, &[probe_note(20.0, 30.0)])
                .expect("write")
                .expect("bytes");
            let [x0, y0, x1, y1] = crate::rendered_mark_bbox(pdfium.get(), bytes, true, is_glyph)
                .expect("note must be visible");

            assert!(
                x1 - x0 > 3.0 * (y1 - y0),
                "/Rotate {rotate}: text is not level — rendered {}x{}",
                x1 - x0,
                y1 - y0
            );
        }
    }

    /// Read-back has to invert the same mapping, including the width/height
    /// exchange at 90 and 270. If it does not, every reopen shifts and
    /// reshapes the note — and re-applying then writes the drifted version
    /// back, so the damage compounds.
    #[test]
    fn notes_round_trip_their_coordinates_on_a_rotated_page() {
        for rotate in [0, 90, 180, 270] {
            let page = crate::geometry_page_bytes(200.0, 400.0, rotate, None);
            let placed = probe_note(20.0, 30.0);
            let bytes = write_typewriter_annots(&page, &[placed.clone()])
                .expect("write")
                .expect("bytes");

            let read = read_typewriter_annots(&bytes).expect("read");
            assert_eq!(read.len(), 1, "/Rotate {rotate}");
            let got = &read[0];
            for (name, g, w) in [
                ("x", got.x, placed.x),
                ("y", got.y, placed.y),
                ("width", got.width, placed.width),
                ("height", got.height, placed.height),
            ] {
                assert!(
                    (g - w).abs() < 0.5,
                    "/Rotate {rotate}: {name} drifted, {g} vs {w}"
                );
            }
        }
    }

    /// The invisible page-text run is what makes a note searchable and
    /// selectable. Its text matrix carries the same turn as the appearance, so
    /// this is the cheap proof that rotating it did not break extraction.
    ///
    /// Reads through `page_text_in_document_order`, which is what search and
    /// selection actually use. `PdfPageText::all()` returns an empty string
    /// for the `/Rotate 270` case even though the glyphs are present and
    /// correctly placed — it reconstructs reading order from glyph geometry
    /// and gives up on a run that advances downward (issue #80). That is the
    /// documented reason production avoids it, and a test using it here would
    /// report a placement bug that does not exist.
    #[test]
    fn note_text_stays_extractable_on_a_rotated_page() {
        use crate::commands::text::page_text_in_document_order;

        let pdfium = crate::test_pdfium();

        for rotate in [0, 90, 180, 270] {
            let page = crate::geometry_page_bytes(200.0, 400.0, rotate, None);
            let bytes = write_typewriter_annots(&page, &[sample_annot()])
                .expect("write")
                .expect("bytes");

            let doc = pdfium.get().load_pdf_from_byte_vec(bytes, None).expect("open");
            let page = doc.pages().get(0).expect("page 1");
            let page_text = page.text().expect("text");
            let text = page_text_in_document_order(&page_text);
            assert!(
                text.contains("Hello world"),
                "/Rotate {rotate}: note text missing from extraction: {text:?}"
            );
        }
    }

    /// Rotating a page *after* a note is on it turns the note with the page,
    /// exactly as it turns flattened ink — because a `/Rect` is in user space
    /// and the page rotation is applied on top of it. Standard PDF behaviour,
    /// and the behaviour Tumbler's own page-ops path produces.
    ///
    /// Pinned here because it is the boundary of what issue #121 fixed. The
    /// document is right; Tumbler's *overlay* is what does not follow, since
    /// it draws notes from coordinates read once at open and never re-mapped.
    /// A future overlay fix must not "correct" the file to match the screen —
    /// the file is the part that is already correct.
    #[test]
    fn a_note_turns_with_the_page_when_the_page_is_rotated_afterwards() {
        use crate::state::DocEntry;

        let pdfium = crate::test_pdfium();
        let page = crate::geometry_page_bytes(200.0, 400.0, 0, None);
        let authored = write_typewriter_annots(&page, &[probe_note(20.0, 30.0)])
            .expect("write")
            .expect("bytes");

        let path = std::env::temp_dir().join("tumbler_note_then_rotate.pdf");
        std::fs::write(&path, &authored).expect("write temp");
        let state = AppState::new(pdfium.get(), None);
        let entry = DocEntry::load(state.pdfium, &path.to_string_lossy(), None).expect("load");
        state.insert_document("d".to_string(), entry).expect("insert");

        crate::commands::pages::rotate_pages_impl(&state, "d".to_string(), vec![1], 1)
            .expect("rotate 90° the way the toolbar does");
        let rotated = {
            let entry = state.get_document("d").expect("get");
            let entry = lock_mutex(&entry).expect("lock");
            entry.buffer.clone()
        };

        // The annotation itself is untouched: user space did not move, only
        // the page's /Rotate did.
        let doc = Document::load_mem(&rotated).expect("reparse");
        let page_id = *doc.get_pages().get(&1).expect("page 1");
        let refs = page_annot_refs(&doc, page_id);
        assert_eq!(refs.len(), 1, "the note must survive the rotation");
        let rect: Vec<f32> = doc
            .get_object(refs[0])
            .and_then(|o| o.as_dict())
            .and_then(|d| d.get(b"Rect"))
            .and_then(|r| r.as_array())
            .expect("rect")
            .iter()
            .map(object_as_f32)
            .collect();
        assert_eq!(rect, vec![20.0, 350.0, 140.0, 370.0], "/Rect must not move");

        // On screen it has turned: the note now occupies a tall, narrow
        // footprint on what is now a 400x200 page, and its glyphs run down it.
        let [x0, y0, x1, y1] = crate::rendered_mark_bbox(pdfium.get(), rotated.clone(), true, is_glyph)
            .expect("the note must still be visible");
        assert!(y1 - y0 > 3.0 * (x1 - x0), "glyphs should now run down the page, got {}x{}", x1 - x0, y1 - y0);
        let footprint = [350.0, 20.0, 370.0, 140.0];
        assert!(
            x0 >= footprint[0] - GLYPH_TOL && x1 <= footprint[2] + GLYPH_TOL
                && y0 >= footprint[1] - GLYPH_TOL && y1 <= footprint[3] + GLYPH_TOL,
            "glyphs at [{x0}, {y0}, {x1}, {y1}] fall outside the turned note box {footprint:?}"
        );

        // And read-back reports the note where the *rotated* page shows it —
        // 120 tall and 20 wide, the transpose of how it was authored. This is
        // what the overlay would need in order to follow the rotation.
        let read = read_typewriter_annots(&rotated).expect("read");
        assert_eq!(read.len(), 1);
        let got = &read[0];
        assert_eq!(
            (got.x, got.y, got.width, got.height),
            (350.0, 20.0, 20.0, 120.0),
            "read-back must report the note in the new render space"
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

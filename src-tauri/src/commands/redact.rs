//! True redaction — flatten + verification (issue #1).
//!
//! Approach A only: every page that contains at least one redaction region is
//! rendered to a raster, the regions are burned to opaque black pixels, and the
//! page is replaced with a single full-page JPEG. The page then has no text
//! objects, no fonts, no vector art, and none of its original images — there is
//! nothing left to extract, which is what makes the result *provable*.
//!
//! After the flatten, document-level leak vectors are scrubbed (Info dictionary,
//! XMP metadata, JavaScript/EmbeddedFiles name trees, the structure tree and
//! outlines — tagged-PDF `/ActualText`/`/Alt` and bookmark titles duplicate
//! visible text — page `/Metadata`/`/PieceInfo`, and AcroForm fields left
//! without a widget by the flatten), the flattened pages are re-OCR'd
//! into an invisible text layer (reusing issue #4's machinery — the burned
//! rectangles read as blank, so search comes back for everything *except* the
//! redacted spots), and then verification runs on the **final bytes, reloaded
//! fresh**:
//!
//! 1. text extraction on every redacted page — no character box may intersect a
//!    redaction region;
//! 2. defense-in-depth OCR of each burned region — no legible text (skipped,
//!    and reported as skipped, when no Windows OCR language pack is installed);
//! 3. a search for every "find & redact all" query — zero hits;
//! 4. structural postconditions (fail-closed): the leak vectors the text-based
//!    checks cannot see — structure tree, outlines, metadata, widget-less form
//!    fields — must be absent, so a scrub regression can never certify.
//!
//! Any failure populates `RedactionResult.leaks`, `verified` stays false, and
//! `save_redacted_copy` refuses to write. The staged bytes never enter
//! `DocEntry.buffer`; they live in `AppState.pending_redactions` (previewed via
//! `render_redacted_page`) until Save As writes them or Discard drops them —
//! structurally, redacted output has no path to the original file.

use crate::commands::ocr::{OcrCache, OcrEngine, OCR_UNAVAILABLE_MESSAGE};
use crate::commands::text::{page_origin, search_document_impl, TextRect};
use crate::commands::text_layer::add_text_layer_impl_filtered;
use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry, PendingRedaction};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

/// A rectangle to redact, in the same coordinate space as `TextRect`
/// (PDF points, top-left origin, per-page).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedactRegion {
    pub page: u32,
    pub rect: TextRect,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedactionResult {
    pub regions: u32,
    pub pages_flattened: u32,
    /// True only if post-redaction verification found nothing recoverable.
    pub verified: bool,
    /// Any region where verification still found extractable text — must be
    /// empty for `verified` to be true. Surfaced so the UI can fail loudly.
    pub leaks: Vec<RedactRegion>,
    /// Structural postconditions of the scrub that failed on the final bytes
    /// (check 4 — fail-closed: e.g. a surviving structure tree or a
    /// widget-less form field). Must be empty for `verified` to be true.
    pub structural_violations: Vec<String>,
    /// Whether the defense-in-depth OCR check ran (false when no Windows OCR
    /// language pack is installed). Checks 1 and 3 are the hard gate either way.
    pub ocr_check_ran: bool,
    /// Flattened pages that received a re-OCR'd invisible text layer.
    pub reocr_pages: u32,
    pub cancelled: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum RedactStage {
    Flatten,
    Reocr,
    Verify,
}

/// Progress for an in-flight redaction run, emitted on `redact-progress`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RedactProgress {
    pub stage: RedactStage,
    pub page: u32,
    pub total: u32,
}

/// Rendering DPI for the flatten raster is user-adjustable; clamp to sane bounds.
const MIN_DPI: f32 = 72.0;
const MAX_DPI: f32 = 600.0;
/// Burned boxes extend this far beyond the marked rect (the issue's "cover
/// slightly beyond the text box" guard against sub-pixel coordinate drift).
const BURN_PAD_PTS: f32 = 1.0;
/// The flattened page image is a full-page photographic raster; 85 keeps text
/// legible without ballooning the file.
const REDACT_JPEG_QUALITY: u8 = 85;
/// Resource name for the full-page image on a flattened page.
const IMAGE_NAME: &str = "TumblerRedact";
/// DPI for the verification OCR of burned regions.
const VERIFY_OCR_DPI: f32 = 200.0;
/// Two rects "intersect" for leak purposes only if they overlap by more than
/// this many points in both axes — avoids flagging text that merely borders a
/// region due to float noise in the extraction boxes.
const OVERLAP_EPSILON_PTS: f32 = 1.0;

// ── Pure geometry / raster helpers ──────────────────────────────────────────

/// Maps a region rect (points, top-left origin) to clamped pixel bounds
/// `(x0, y0, x1, y1)` in a `bw`×`bh` render of a `page_w`×`page_h` page,
/// expanded by `pad` points on every side. `None` if the result is empty.
fn region_to_pixels(
    rect: &TextRect,
    pad: f32,
    bw: u32,
    bh: u32,
    page_w: f32,
    page_h: f32,
) -> Option<(u32, u32, u32, u32)> {
    if page_w <= 0.0 || page_h <= 0.0 {
        return None;
    }
    let sx = bw as f32 / page_w;
    let sy = bh as f32 / page_h;
    let x0 = ((rect.x - pad) * sx).floor().max(0.0) as u32;
    let y0 = ((rect.y - pad) * sy).floor().max(0.0) as u32;
    let x1 = (((rect.x + rect.width + pad) * sx).ceil()).min(bw as f32) as u32;
    let y1 = (((rect.y + rect.height + pad) * sy).ceil()).min(bh as f32) as u32;
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// Burns opaque black rectangles over `rects` directly in the RGBA buffer,
/// using the same points→pixels scale the renderer used.
fn burn_regions(rgba: &mut [u8], bw: u32, bh: u32, page_w: f32, page_h: f32, rects: &[TextRect]) {
    for rect in rects {
        let Some((x0, y0, x1, y1)) = region_to_pixels(rect, BURN_PAD_PTS, bw, bh, page_w, page_h)
        else {
            continue;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * bw + x) * 4) as usize;
                rgba[i] = 0;
                rgba[i + 1] = 0;
                rgba[i + 2] = 0;
                rgba[i + 3] = 255;
            }
        }
    }
}

/// True when `a` and `b` overlap by more than [`OVERLAP_EPSILON_PTS`] in both
/// axes. Both rects are points, top-left origin.
fn rects_intersect(a: &TextRect, b: &TextRect) -> bool {
    let ox = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let oy = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    ox > OVERLAP_EPSILON_PTS && oy > OVERLAP_EPSILON_PTS
}

/// Encodes an RGBA page raster as a baseline JPEG (alpha dropped — the render
/// is opaque).
fn encode_page_jpeg(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, AppError> {
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let img = image::RgbImage::from_raw(w, h, rgb)
        .ok_or_else(|| AppError::Other("Failed to build page raster for redaction".to_string()))?;
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, REDACT_JPEG_QUALITY)
        .encode_image(&img)
        .map_err(|e| AppError::Other(format!("Failed to encode redacted page: {e}")))?;
    Ok(out)
}

/// Copies the pixel window `(x0, y0)..(x1, y1)` out of an RGBA buffer.
fn crop_rgba(rgba: &[u8], bw: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<u8> {
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for y in y0..y1 {
        let start = ((y * bw + x0) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (cw * 4) as usize]);
    }
    out
}

// ── lopdf page replacement + document scrubbing ─────────────────────────────

/// Replaces one page's content with a single full-page image: the JPEG becomes
/// an Image XObject, `/Contents` becomes one stream drawing it across the
/// page, `/Resources` is replaced wholesale (old fonts/XObjects unreferenced),
/// and the page's annotations, metadata, and private application data are
/// dropped (all of them can quote redacted text).
///
/// The page is normalized to `MediaBox [0 0 page_w page_h]` with `/Rotate 0`:
/// pdfium rendered the page in *display* orientation, so the raster already
/// includes any rotation and origin shift — rewriting the geometry keeps the
/// visual result identical for every page geometry without special cases.
fn replace_page_with_image(
    doc: &mut Document,
    page_id: ObjectId,
    jpeg: Vec<u8>,
    px_w: u32,
    px_h: u32,
    page_w: f32,
    page_h: f32,
) -> Result<(), AppError> {
    let img_id = doc.add_object(Object::Stream(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => px_w as i64,
            "Height" => px_h as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8_i64,
            "Filter" => "DCTDecode",
        },
        jpeg,
    )));
    let content = format!("q {page_w} 0 0 {page_h} 0 0 cm /{IMAGE_NAME} Do Q");
    let content_id = doc.add_object(Object::Stream(Stream::new(
        Dictionary::new(),
        content.into_bytes(),
    )));

    let media = vec![
        Object::Integer(0),
        Object::Integer(0),
        Object::Real(page_w),
        Object::Real(page_h),
    ];
    let page = doc
        .get_object_mut(page_id)
        .map_err(|e| AppError::lopdf("Failed to get page for redaction", e))?
        .as_dict_mut()
        .map_err(|e| AppError::lopdf("Redacted page is not a dictionary", e))?;
    page.set("Contents", Object::Reference(content_id));
    page.set(
        "Resources",
        Object::Dictionary(dictionary! {
            "XObject" => dictionary! { IMAGE_NAME => Object::Reference(img_id) },
        }),
    );
    page.set("MediaBox", Object::Array(media.clone()));
    // Set (not remove) CropBox/Rotate — both can be inherited from the page tree.
    page.set("CropBox", Object::Array(media));
    page.set("Rotate", Object::Integer(0));
    for key in [
        b"Annots".as_slice(),
        b"Thumb",
        b"BleedBox",
        b"TrimBox",
        b"ArtBox",
        b"Group",
        b"B",
        b"AA",
        b"StructParents",
        // Page-level metadata and private application data (/PieceInfo can
        // hold an authoring app's own copy of the original page content).
        b"Metadata",
        b"PieceInfo",
    ] {
        page.remove(key);
    }
    Ok(())
}

/// Recursion guard for AcroForm field trees. Both the scrub and the
/// verification postcondition check use the same helper with the same limit,
/// so they can't disagree about a pathological document.
const FIELD_TREE_MAX_DEPTH: u8 = 32;

/// Object ids of every annotation still referenced from some page's `/Annots`
/// (flattened pages have already had theirs removed).
fn live_annot_ids(doc: &Document) -> HashSet<ObjectId> {
    let mut live = HashSet::new();
    for page_id in doc.get_pages().into_values() {
        let annots = doc
            .get_object(page_id)
            .ok()
            .and_then(|o| o.as_dict().ok())
            .and_then(|p| p.get(b"Annots").ok().cloned());
        let array = match annots {
            Some(Object::Array(a)) => Some(a),
            Some(Object::Reference(r)) => {
                doc.get_object(r).ok().and_then(|o| o.as_array().ok()).cloned()
            }
            _ => None,
        };
        if let Some(a) = array {
            live.extend(a.iter().filter_map(|o| o.as_reference().ok()));
        }
    }
    live
}

/// True when the field subtree rooted at `id` still has at least one terminal
/// widget referenced from a page. A field whose widgets all sat on flattened
/// pages (or a merged field+widget dropped with its page's `/Annots`) has
/// none — its `/V` value could echo redacted text the user typed, invisibly
/// to text-extraction-based verification, so the scrub drops it. At the depth
/// limit the field is treated as live (kept) — the postcondition check uses
/// the same rule, so a pathological tree blocks nothing spuriously.
fn field_has_live_widget(
    doc: &Document,
    id: ObjectId,
    live: &HashSet<ObjectId>,
    depth: u8,
) -> bool {
    if depth == 0 {
        return true;
    }
    let Some(dict) = doc.get_object(id).ok().and_then(|o| o.as_dict().ok()) else {
        return false;
    };
    match dict.get(b"Kids") {
        // Hierarchical field: live iff any widget below it is live.
        Ok(Object::Array(kids)) if !kids.is_empty() => kids
            .iter()
            .filter_map(|k| k.as_reference().ok())
            .any(|kid| field_has_live_widget(doc, kid, live, depth - 1)),
        // Terminal node: the merged field+widget object itself.
        _ => live.contains(&id),
    }
}

/// Removes dead widget references from a surviving field's `/Kids`,
/// recursively, so the dropped widgets (and their appearance streams, which
/// can paint the field's text) become unreachable and are pruned.
fn prune_dead_kids(doc: &mut Document, id: ObjectId, live: &HashSet<ObjectId>, depth: u8) {
    if depth == 0 {
        return;
    }
    let kids: Vec<ObjectId> = doc
        .get_object(id)
        .ok()
        .and_then(|o| o.as_dict().ok())
        .and_then(|d| d.get(b"Kids").ok())
        .and_then(|o| o.as_array().ok())
        .map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect())
        .unwrap_or_default();
    if kids.is_empty() {
        return;
    }
    let retained: Vec<ObjectId> = kids
        .into_iter()
        .filter(|&kid| field_has_live_widget(doc, kid, live, depth - 1))
        .collect();
    for &kid in &retained {
        prune_dead_kids(doc, kid, live, depth - 1);
    }
    if let Ok(d) = doc.get_dictionary_mut(id) {
        d.set(
            "Kids",
            Object::Array(retained.iter().map(|&k| Object::Reference(k)).collect()),
        );
    }
}

/// The AcroForm `/Fields` entries as object ids, resolving an indirect
/// `/AcroForm` and an indirect `/Fields` array.
fn acroform_field_ids(doc: &Document) -> Vec<ObjectId> {
    let acroform = match doc.catalog().ok().and_then(|c| c.get(b"AcroForm").ok().cloned()) {
        Some(Object::Reference(id)) => doc.get_dictionary(id).ok().cloned(),
        Some(Object::Dictionary(d)) => Some(d),
        _ => None,
    };
    let fields = match acroform.and_then(|a| a.get(b"Fields").ok().cloned()) {
        Some(Object::Array(a)) => Some(a),
        Some(Object::Reference(r)) => {
            doc.get_object(r).ok().and_then(|o| o.as_array().ok()).cloned()
        }
        _ => None,
    };
    fields
        .map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect())
        .unwrap_or_default()
}

/// Drops every AcroForm field — hierarchical or merged — with no widget left
/// on any page, and prunes dead widget refs from surviving fields' `/Kids`.
/// Keyed on which widgets are *still referenced* (rather than which were
/// removed), so pre-existing orphan fields are cleaned too and the
/// verification postcondition ("no field without a live widget") holds by
/// construction.
fn scrub_acroform(doc: &mut Document) {
    let field_ids = acroform_field_ids(doc);
    if field_ids.is_empty() {
        return;
    }
    let live = live_annot_ids(doc);
    let retained: Vec<ObjectId> = field_ids
        .into_iter()
        .filter(|&id| field_has_live_widget(doc, id, &live, FIELD_TREE_MAX_DEPTH))
        .collect();
    for &id in &retained {
        prune_dead_kids(doc, id, &live, FIELD_TREE_MAX_DEPTH);
    }

    let fields_array =
        Object::Array(retained.iter().map(|&id| Object::Reference(id)).collect());
    let acroform = doc.catalog().ok().and_then(|c| c.get(b"AcroForm").ok().cloned());
    match acroform {
        Some(Object::Reference(id)) => {
            if let Ok(d) = doc.get_dictionary_mut(id) {
                d.set("Fields", fields_array);
            }
        }
        Some(Object::Dictionary(_)) => {
            if let Ok(catalog) = doc.catalog_mut() {
                if let Ok(Object::Dictionary(d)) = catalog.get_mut(b"AcroForm") {
                    d.set("Fields", fields_array);
                }
            }
        }
        _ => {}
    }
}

/// Scrubs the document-level leak vectors: the Info dictionary; the catalog's
/// XMP `/Metadata` and `/OpenAction`, the `/JavaScript` and `/EmbeddedFiles`
/// name trees, page thumbnails (all via `step_strip_extras`); the structure
/// tree and outlines; and AcroForm fields orphaned by the flatten. Ends with
/// a prune so the scrubbed objects don't survive as unreferenced garbage
/// still containing the text.
///
/// The structure tree (`/StructTreeRoot`) and bookmarks (`/Outlines`) are
/// dropped wholesale: a tagged PDF's `/ActualText`/`/Alt` strings and bookmark
/// titles routinely duplicate visible text, none of the verification checks
/// can read them (pdfium's text APIs cover page content only), and there is
/// no provable way to keep just the "safe" parts. The redacted copy loses
/// accessibility tagging and bookmarks — the standard redaction trade-off.
fn scrub_leak_vectors(doc: &mut Document) {
    crate::commands::optimize::step_strip_extras(doc);
    doc.trailer.remove(b"Info");
    if let Ok(catalog) = doc.catalog_mut() {
        catalog.remove(b"StructTreeRoot");
        catalog.remove(b"MarkInfo");
        catalog.remove(b"Outlines");
    }
    scrub_acroform(doc);
    doc.prune_objects();
}

/// Check 4 (fail-closed): asserts the scrub's structural postconditions on
/// the final bytes, so a scrub regression (or a text-bearing structure the
/// text-based checks can't see) can never yield a false "verified". Returns
/// human-readable violations; any entry blocks Save As.
fn verify_scrub_postconditions(
    final_bytes: &[u8],
    flattened_pages: &HashSet<u32>,
) -> Result<Vec<String>, AppError> {
    let doc = Document::load_mem(final_bytes)
        .map_err(|e| AppError::lopdf("Failed to reparse redacted output", e))?;
    let mut violations = Vec::new();

    if doc.trailer.get(b"Info").is_ok() {
        violations.push("document Info dictionary present".to_string());
    }
    if let Ok(catalog) = doc.catalog() {
        for key in ["Metadata", "StructTreeRoot", "MarkInfo", "Outlines", "OpenAction"] {
            if catalog.get(key.as_bytes()).is_ok() {
                violations.push(format!("catalog /{key} present"));
            }
        }
        let names = catalog
            .get(b"Names")
            .ok()
            .and_then(|o| match o {
                Object::Reference(r) => doc.get_dictionary(*r).ok(),
                Object::Dictionary(d) => Some(d),
                _ => None,
            });
        if let Some(names) = names {
            for key in ["JavaScript", "EmbeddedFiles"] {
                if names.get(key.as_bytes()).is_ok() {
                    violations.push(format!("name tree /{key} present"));
                }
            }
        }
    }

    for (&page_num, &page_id) in &doc.get_pages() {
        if !flattened_pages.contains(&page_num) {
            continue;
        }
        if let Ok(page) = doc.get_dictionary(page_id) {
            for key in ["Annots", "Metadata", "PieceInfo", "Thumb"] {
                if page.get(key.as_bytes()).is_ok() {
                    violations.push(format!("flattened page {page_num}: /{key} present"));
                }
            }
        }
    }

    // No form field may survive without a widget on some page — a widget-less
    // field's /V is invisible to the text checks yet fully recoverable.
    let live = live_annot_ids(&doc);
    for id in acroform_field_ids(&doc) {
        if !field_has_live_widget(&doc, id, &live, FIELD_TREE_MAX_DEPTH) {
            violations.push(format!("form field without a widget (object {} {})", id.0, id.1));
        }
    }

    Ok(violations)
}

// ── Verification ────────────────────────────────────────────────────────────

/// Verifies redacted output. Runs on `final_bytes` loaded fresh — the artifact
/// that will be written to disk, after the re-OCR pass. Returns the leaked
/// regions, whether the OCR check ran, and any structural-postcondition
/// violations (checks 1–4; empty leaks + empty violations = verified).
pub(crate) fn verify_redactions(
    pdfium: &Pdfium,
    final_bytes: &[u8],
    regions: &[RedactRegion],
    queries: &[String],
    engine: &Arc<dyn OcrEngine>,
) -> Result<(Vec<RedactRegion>, bool, Vec<String>), AppError> {
    let doc = pdfium
        .load_pdf_from_byte_vec(final_bytes.to_vec(), None)
        .map_err(|e| AppError::pdfium("Failed to reload redacted output for verification", e))?;

    let mut by_page: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, region) in regions.iter().enumerate() {
        by_page.entry(region.page).or_default().push(i);
    }

    let mut leaked: HashSet<usize> = HashSet::new();
    let mut leaks: Vec<RedactRegion> = Vec::new();

    // Check 1: no extractable character's box may intersect a region.
    for (&page_num, indices) in &by_page {
        let page = doc
            .pages()
            .get(page_num.saturating_sub(1) as i32)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
        let page_height = page.height().value;
        let (origin_x, origin_y) = page_origin(&page);
        let text = page
            .text()
            .map_err(|e| AppError::pdfium("Failed to read text for verification", e))?;
        for ch in text.chars().iter() {
            let is_visible_char = ch.unicode_char().is_some_and(|c| !c.is_whitespace());
            if !is_visible_char {
                continue;
            }
            let Ok(bounds) = ch.loose_bounds() else { continue };
            let char_rect = TextRect {
                x: bounds.left().value - origin_x,
                y: page_height - (bounds.top().value - origin_y),
                width: bounds.right().value - bounds.left().value,
                height: bounds.top().value - bounds.bottom().value,
            };
            for &i in indices {
                if !leaked.contains(&i) && rects_intersect(&char_rect, &regions[i].rect) {
                    leaked.insert(i);
                    leaks.push(regions[i].clone());
                }
            }
        }
    }

    // Check 3: zero hits for every "find & redact all" query, document-wide.
    let options = PdfSearchOptions::new().match_case(false);
    for query in queries.iter().filter(|q| !q.is_empty()) {
        let page_count = doc.pages().len();
        for page_idx in 0..page_count {
            let Ok(page) = doc.pages().get(page_idx) else { continue };
            let page_height = page.height().value;
            let (origin_x, origin_y) = page_origin(&page);
            let Ok(text) = page.text() else { continue };
            let Ok(search) = text.search(query, &options) else { continue };
            for match_segments in search.iter(PdfSearchDirection::SearchForward) {
                for i in 0..match_segments.len() {
                    if let Ok(segment) = match_segments.get(i) {
                        let bounds = segment.bounds();
                        leaks.push(RedactRegion {
                            page: (page_idx + 1) as u32,
                            rect: TextRect {
                                x: bounds.left().value - origin_x,
                                y: page_height - (bounds.top().value - origin_y),
                                width: bounds.right().value - bounds.left().value,
                                height: bounds.top().value - bounds.bottom().value,
                            },
                        });
                    }
                }
            }
        }
    }

    // Check 2 (defense in depth): OCR each burned region; no legible text may
    // come back. Catches a non-opaque cover or a wrong-scale burn. Skipped —
    // and reported as skipped — when the OCR engine is unavailable.
    let mut ocr_check_ran = true;
    'ocr: for (&page_num, indices) in &by_page {
        let page = doc
            .pages()
            .get(page_num.saturating_sub(1) as i32)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
        let page_w = page.width().value;
        let page_h = page.height().value;
        let target_width = ((page_w / 72.0) * VERIFY_OCR_DPI).round().max(1.0) as u32;
        let config = PdfRenderConfig::new().set_target_width(target_width as Pixels);
        let Ok(bitmap) = page.render_with_config(&config) else { continue };
        let rgba = bitmap.as_rgba_bytes();
        let (bw, bh) = (bitmap.width() as u32, bitmap.height() as u32);

        for &i in indices {
            // Crop exactly the burned pixels (region + burn pad). A wider
            // margin would include legitimate adjacent text, whose glyph
            // fragments could OCR as a false leak.
            let Some((x0, y0, x1, y1)) =
                region_to_pixels(&regions[i].rect, BURN_PAD_PTS, bw, bh, page_w, page_h)
            else {
                continue;
            };
            if x1 - x0 < 32 || y1 - y0 < 32 {
                continue; // too small to hold legible text at this DPI
            }
            let crop = crop_rgba(&rgba, bw, x0, y0, x1, y1);
            match engine.recognize(&crop, x1 - x0, y1 - y0) {
                Err(_) => {
                    // Engine unavailable (no language pack) — the check can't
                    // run; report that rather than failing the redaction.
                    ocr_check_ran = false;
                    break 'ocr;
                }
                Ok(words) => {
                    if words.iter().any(|w| !w.text.trim().is_empty()) && !leaked.contains(&i) {
                        leaked.insert(i);
                        leaks.push(regions[i].clone());
                    }
                }
            }
        }
    }

    // Check 4: structural postconditions (fail-closed) — leak vectors the
    // text-based checks above cannot see (structure tree, outlines, metadata,
    // widget-less form fields) must be absent from the output.
    let flattened: HashSet<u32> = by_page.keys().copied().collect();
    let structural_violations = verify_scrub_postconditions(final_bytes, &flattened)?;

    Ok((leaks, ocr_check_ran, structural_violations))
}

// ── The pipeline ────────────────────────────────────────────────────────────

/// Builds the redacted bytes from `pdf_bytes`: flatten each page that has
/// regions, scrub document-level leak vectors, re-OCR the flattened pages, and
/// verify the final bytes. Pure with respect to `AppState` so it runs inside
/// `spawn_blocking` and is directly testable. Returns the result plus the
/// final output bytes (`None` when cancelled).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_redactions_impl(
    emit: &dyn Fn(RedactProgress),
    pdfium: &'static Pdfium,
    pdf_bytes: &[u8],
    regions: &[RedactRegion],
    verify_queries: &[String],
    target_dpi: f32,
    engine: &Arc<dyn OcrEngine>,
    cancel: &Arc<AtomicBool>,
) -> Result<(RedactionResult, Option<Vec<u8>>), AppError> {
    if regions.is_empty() {
        return Err(AppError::Other("No redaction regions provided".to_string()));
    }
    let dpi = target_dpi.clamp(MIN_DPI, MAX_DPI);

    let cancelled_result = || RedactionResult {
        regions: regions.len() as u32,
        pages_flattened: 0,
        verified: false,
        leaks: Vec::new(),
        structural_violations: Vec::new(),
        ocr_check_ran: false,
        reocr_pages: 0,
        cancelled: true,
    };

    let mut by_page: BTreeMap<u32, Vec<TextRect>> = BTreeMap::new();
    for region in regions {
        if region.rect.width <= 0.0 || region.rect.height <= 0.0 {
            return Err(AppError::Other("Empty redaction region".to_string()));
        }
        by_page.entry(region.page).or_default().push(region.rect.clone());
    }

    // pdfium view for rendering, lopdf view for rewriting — both from the same
    // bytes, so their 1-based page order matches.
    let fdoc = pdfium
        .load_pdf_from_byte_vec(pdf_bytes.to_vec(), None)
        .map_err(|e| AppError::pdfium("Failed to load PDF for redaction", e))?;
    let page_count = fdoc.pages().len() as u32;
    if let Some(&bad) = by_page.keys().find(|&&p| p < 1 || p > page_count) {
        return Err(AppError::Other(format!(
            "Redaction region on page {bad}, but the document has {page_count} pages"
        )));
    }
    let mut ldoc = Document::load_mem(pdf_bytes)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for redaction", e))?;
    let page_ids = ldoc.get_pages();

    // Flatten each redacted page.
    let flatten_total = by_page.len() as u32;
    for (i, (&page_num, rects)) in by_page.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok((cancelled_result(), None));
        }
        emit(RedactProgress {
            stage: RedactStage::Flatten,
            page: i as u32 + 1,
            total: flatten_total,
        });

        let page = fdoc
            .pages()
            .get(page_num.saturating_sub(1) as i32)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
        let page_w = page.width().value;
        let page_h = page.height().value;
        let target_width = ((page_w / 72.0) * dpi).round().max(1.0) as u32;
        let config = PdfRenderConfig::new().set_target_width(target_width as Pixels);
        let bitmap = page
            .render_with_config(&config)
            .map_err(|e| AppError::pdfium(format!("Failed to render page {page_num}"), e))?;
        let (bw, bh) = (bitmap.width() as u32, bitmap.height() as u32);
        let mut rgba = bitmap.as_rgba_bytes();

        burn_regions(&mut rgba, bw, bh, page_w, page_h, rects);
        let jpeg = encode_page_jpeg(&rgba, bw, bh)?;

        let &page_id = page_ids.get(&page_num).ok_or_else(|| {
            AppError::Other(format!("Page {page_num} not found in the PDF object tree"))
        })?;
        replace_page_with_image(&mut ldoc, page_id, jpeg, bw, bh, page_w, page_h)?;
    }
    drop(fdoc);

    scrub_leak_vectors(&mut ldoc);

    let mut flattened = Vec::new();
    ldoc.save_to(&mut flattened)
        .map_err(|e| AppError::io("Failed to serialize redacted PDF", e))?;
    drop(ldoc);

    if cancel.load(Ordering::Relaxed) {
        return Ok((cancelled_result(), None));
    }

    // Re-OCR the flattened pages so the saved copy stays searchable (the burned
    // areas read as blank, so nothing redacted can come back). OCR being
    // unavailable is not a redaction failure — the copy is just not searchable.
    let mut final_bytes = flattened;
    let mut reocr_pages = 0u32;
    {
        let document = pdfium
            .load_pdf_from_byte_vec(final_bytes.clone(), None)
            .map_err(|e| AppError::pdfium("Failed to reload flattened PDF", e))?;
        let temp_entry = Arc::new(Mutex::new(DocEntry {
            document,
            file_path: String::new(),
            buffer: final_bytes.clone(),
            dirty: false,
            password: None,
            encrypted: false,
            permissions: None,
            linearized: false,
        }));
        let temp_cache: OcrCache = Arc::new(Mutex::new(HashMap::new()));
        let flattened_pages: HashSet<u32> = by_page.keys().copied().collect();
        match add_text_layer_impl_filtered(
            |page, total| emit(RedactProgress { stage: RedactStage::Reocr, page, total }),
            temp_entry,
            "redact-reocr".to_string(),
            engine.clone(),
            temp_cache,
            cancel.clone(),
            Some(&flattened_pages),
            // Per-word runs: a line-grouped run would be stretched across the
            // burned gap of a mid-line redaction, putting invisible glyphs
            // inside the region and failing verification.
            true,
        ) {
            Ok((result, edited)) => {
                if result.cancelled {
                    return Ok((cancelled_result(), None));
                }
                reocr_pages = result.pages_written;
                if let Some(bytes) = edited {
                    final_bytes = bytes;
                }
            }
            Err(AppError::Other(msg)) if msg == OCR_UNAVAILABLE_MESSAGE => {}
            Err(e) => return Err(e),
        }
    }

    // Verification runs on the final bytes — the artifact Save As will write —
    // reloaded fresh, after the re-OCR pass.
    emit(RedactProgress { stage: RedactStage::Verify, page: 0, total: 0 });
    let (leaks, ocr_check_ran, structural_violations) =
        verify_redactions(pdfium, &final_bytes, regions, verify_queries, engine)?;

    Ok((
        RedactionResult {
            regions: regions.len() as u32,
            pages_flattened: flatten_total,
            verified: leaks.is_empty() && structural_violations.is_empty(),
            leaks,
            structural_violations,
            ocr_check_ran,
            reocr_pages,
            cancelled: false,
        },
        Some(final_bytes),
    ))
}

// ── Commands ────────────────────────────────────────────────────────────────

/// "Find & redact all": every occurrence of `query` across the document as
/// redaction regions. Reuses the search machinery, so match-case / whole-word /
/// regex modes and the OCR-cache fallback for scanned pages all apply.
#[tauri::command]
pub fn find_redaction_matches(
    state: State<'_, AppState>,
    doc_id: String,
    query: String,
    match_case: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<Vec<RedactRegion>, String> {
    find_redaction_matches_impl(&state, doc_id, query, match_case, whole_word, use_regex)
        .map_err(String::from)
}

fn find_redaction_matches_impl(
    state: &AppState,
    doc_id: String,
    query: String,
    match_case: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<Vec<RedactRegion>, AppError> {
    Ok(
        search_document_impl(state, doc_id, query, match_case, whole_word, use_regex)?
            .into_iter()
            .flat_map(|result| {
                result
                    .rects
                    .into_iter()
                    .map(move |rect| RedactRegion { page: result.page, rect })
            })
            .collect(),
    )
}

/// Builds the redacted bytes, verifies them, and stages them for preview +
/// Save As. Does NOT touch the on-disk original or the document buffer —
/// redaction is irreversible, so it is Save As only.
#[tauri::command]
pub async fn apply_redactions(
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
    regions: Vec<RedactRegion>,
    verify_queries: Vec<String>,
    target_dpi: f32,
) -> Result<RedactionResult, String> {
    let pdf_bytes = {
        let entry = state.get_document(&doc_id).map_err(String::from)?;
        let entry = lock_mutex(&entry).map_err(String::from)?;
        entry.buffer.clone()
    };

    let cancel = Arc::new(AtomicBool::new(false));
    state.set_redact_job(cancel.clone());
    let pdfium = state.pdfium;
    let engine = state.ocr_engine.clone();

    let emit = move |p: RedactProgress| {
        let _ = window.emit("redact-progress", p);
    };

    let outcome = tauri::async_runtime::spawn_blocking(move || {
        apply_redactions_impl(
            &emit,
            pdfium,
            &pdf_bytes,
            &regions,
            &verify_queries,
            target_dpi,
            &engine,
            &cancel,
        )
    })
    .await
    .map_err(|e| e.to_string());

    state.take_redact_job();
    let (result, output) = outcome?.map_err(String::from)?;

    // Stage even a failed verification: the preview lets the user see what
    // leaked. Save As is gated on `verified` in `save_redacted_copy`.
    if let Some(bytes) = output {
        let document = state
            .pdfium
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .map_err(|e| AppError::pdfium("Failed to load redacted output", e))
            .map_err(String::from)?;
        state
            .set_pending_redaction(
                &doc_id,
                PendingRedaction { document, bytes, verified: result.verified },
            )
            .map_err(String::from)?;
    }
    Ok(result)
}

/// Renders a page of the staged redacted copy for the post-Apply preview.
/// Same contract as `render_page`, but reads from `pending_redactions`.
#[tauri::command]
pub fn render_redacted_page(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
    width: u32,
) -> Result<tauri::ipc::Response, String> {
    render_redacted_page_impl(&state, doc_id, page, width).map_err(String::from)
}

fn render_redacted_page_impl(
    state: &AppState,
    doc_id: String,
    page: u32,
    width: u32,
) -> Result<tauri::ipc::Response, AppError> {
    let pending = state
        .get_pending_redaction(&doc_id)
        .ok_or_else(|| AppError::Other("No redacted copy is staged".to_string()))?;
    let pending = lock_mutex(&pending)?;

    let pdf_page = pending
        .document
        .pages()
        .get(page.saturating_sub(1) as i32)
        .map_err(|e| AppError::pdfium(format!("Failed to get page {page}"), e))?;
    let config = PdfRenderConfig::new().set_target_width(width as Pixels);
    let bitmap = pdf_page
        .render_with_config(&config)
        .map_err(|e| AppError::pdfium(format!("Failed to render page {page}"), e))?;
    Ok(tauri::ipc::Response::new(bitmap.as_rgba_bytes()))
}

/// Writes the staged redacted bytes to `dest_path` (atomic temp + rename) and
/// clears the staging. Refuses when verification failed, when the destination
/// is the original file, or when it's open in another tab. A password-
/// protected document's redacted copy inherits its AES-256 encryption.
#[tauri::command]
pub fn save_redacted_copy(
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
) -> Result<String, String> {
    save_redacted_copy_impl(&state, &doc_id, &dest_path).map_err(String::from)
}

fn save_redacted_copy_impl(
    state: &AppState,
    doc_id: &str,
    dest_path: &str,
) -> Result<String, AppError> {
    let pending = state
        .get_pending_redaction(doc_id)
        .ok_or_else(|| AppError::Other("No redacted copy is staged — run Apply first".to_string()))?;

    // Destination checks against the canonical path when the file exists.
    let dest_canonical_existing = dunce::canonicalize(dest_path)
        .map(|p| p.to_string_lossy().into_owned())
        .ok();
    if let Some(existing) = &dest_canonical_existing {
        if state.is_path_open_elsewhere(existing, doc_id)? {
            return Err(AppError::Other(
                "That file is open in another tab. Close it first or choose a different name."
                    .to_string(),
            ));
        }
    }

    let entry_arc = state.get_document(doc_id)?;
    let entry = lock_mutex(&entry_arc)?;
    if let Some(existing) = &dest_canonical_existing {
        if *existing == entry.file_path {
            return Err(AppError::Other(
                "Refusing to overwrite the original file — save the redacted copy under a \
                 different name."
                    .to_string(),
            ));
        }
    }

    let bytes = {
        let pending = lock_mutex(&pending)?;
        if !pending.verified {
            return Err(AppError::Other(
                "Verification failed — this redacted copy cannot be saved.".to_string(),
            ));
        }
        match (&entry.password, entry.encrypted) {
            (Some(pw), true) => {
                let permissions = entry.permissions.unwrap_or_else(lopdf::Permissions::all);
                crate::commands::encryption::encrypt_with_password(&pending.bytes, pw, permissions)?
            }
            _ => pending.bytes.clone(),
        }
    };
    drop(entry);

    crate::commands::save::atomic_write(dest_path, &bytes)?;
    state.clear_pending_redaction(doc_id);

    Ok(dunce::canonicalize(dest_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dest_path.to_string()))
}

/// Drops the staged redacted copy (the preview's Discard button).
#[tauri::command]
pub fn discard_redaction(state: State<'_, AppState>, doc_id: String) {
    state.clear_pending_redaction(&doc_id);
}

/// Signals an in-progress redaction run to stop.
#[tauri::command]
pub fn cancel_redact(state: State<'_, AppState>) {
    state.cancel_redact_job();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ocr::OcrWord;
    use crate::state::DocEntry;

    /// OCR engine that recognizes nothing — the burned regions are blank, and
    /// these fixtures have no scanned content, so this is the honest default.
    struct EmptyOcrEngine;
    impl OcrEngine for EmptyOcrEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            Ok(Vec::new())
        }
    }

    fn empty_engine() -> Arc<dyn OcrEngine> {
        Arc::new(EmptyOcrEngine)
    }

    fn no_progress(_: RedactProgress) {}

    fn not_cancelled() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    /// Builds a PDF with one 200×200 page per entry, each drawing its text at
    /// 24pt Helvetica from (20, 150) — extractable by pdfium.
    fn text_pdf_bytes(page_texts: &[&str]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let mut kids = Vec::new();
        for text in page_texts {
            let content = format!("BT /F1 24 Tf 20 150 Td ({text}) Tj ET");
            let cid = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => cid,
                "MediaBox" => vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(200), Object::Integer(200),
                ],
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                },
            });
            kids.push(Object::Reference(page_id));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => Object::Integer(page_texts.len() as i64),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize fixture");
        out
    }

    /// Inserts in-memory PDF bytes into `state` as an open document.
    fn open_mem_doc(state: &AppState, doc_id: &str, bytes: Vec<u8>) {
        let pdfium = crate::test_pdfium();
        let document = pdfium
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .expect("load fixture bytes");
        state
            .insert_document(
                doc_id.to_string(),
                DocEntry {
                    document,
                    file_path: format!("{doc_id}.pdf"),
                    buffer: bytes,
                    dirty: false,
                    password: None,
                    encrypted: false,
                    permissions: None,
                    linearized: false,
                },
            )
            .expect("insert");
    }

    fn full_page_region(page: u32) -> RedactRegion {
        RedactRegion {
            page,
            rect: TextRect { x: 0.0, y: 0.0, width: 200.0, height: 200.0 },
        }
    }

    // ── The four tests from the issue ───────────────────────────────────────

    /// Flattening a page with a region covering its text leaves a page with no
    /// extractable text at all in the saved (reloaded) output.
    #[test]
    fn flatten_redaction_removes_all_text_from_page() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let bytes = text_pdf_bytes(&["Top Secret"]);

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &[full_page_region(1)],
            &[],
            150.0,
            &empty_engine(),
            &not_cancelled(),
        )
        .expect("apply");

        assert_eq!(result.pages_flattened, 1);
        assert!(result.verified, "leaks: {:?}", result.leaks);
        assert!(result.leaks.is_empty());
        assert!(!result.cancelled);

        let out = output.expect("output bytes");
        // Reload fresh — the artifact, not the in-memory pre-save state.
        Document::load_mem(&out).expect("output should be valid PDF for lopdf");
        let reloaded = pdfium
            .load_pdf_from_byte_vec(out, None)
            .expect("output should be valid PDF for pdfium");
        let text = reloaded.pages().get(0).expect("page").text().expect("text").all();
        assert!(
            text.trim().is_empty(),
            "flattened page must have no extractable text, got: {text:?}"
        );
    }

    /// Find & redact all: every occurrence of "SECRET" becomes a region; after
    /// apply, searching the saved output finds nothing, untouched pages keep
    /// their text, and the result is verified.
    #[test]
    fn find_and_redact_all_leaves_no_search_hits() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let bytes = text_pdf_bytes(&["SECRET alpha", "plain page", "SECRET beta"]);
        open_mem_doc(&state, "doc1", bytes.clone());

        let regions = find_redaction_matches_impl(
            &state,
            "doc1".to_string(),
            "SECRET".to_string(),
            false,
            false,
            false,
        )
        .expect("find matches");
        assert_eq!(regions.len(), 2, "one occurrence on page 1 and one on page 3");
        let pages: HashSet<u32> = regions.iter().map(|r| r.page).collect();
        assert_eq!(pages, HashSet::from([1, 3]));

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &regions,
            &["SECRET".to_string()],
            150.0,
            &empty_engine(),
            &not_cancelled(),
        )
        .expect("apply");

        assert_eq!(result.pages_flattened, 2);
        assert!(result.verified, "leaks: {:?}", result.leaks);
        assert!(result.leaks.is_empty());

        // Search the saved output: zero hits for the redacted string.
        let out = output.expect("output bytes");
        open_mem_doc(&state, "out", out);
        let hits = search_document_impl(
            &state,
            "out".to_string(),
            "SECRET".to_string(),
            false,
            false,
            false,
        )
        .expect("search output");
        assert!(hits.is_empty(), "redacted string still searchable: {} pages", hits.len());

        // The untouched page keeps its native text.
        let items = crate::commands::text::extract_page_text_impl(&state, "out".to_string(), 2)
            .expect("extract untouched page");
        assert!(
            items.iter().any(|i| i.text.contains("plain")),
            "the untouched page must keep its text"
        );
    }

    /// A fake "redaction" that only draws a cover rect without removing the
    /// text (i.e. verifying the ORIGINAL bytes) must report leaks and fail.
    #[test]
    fn verification_fails_loudly_when_text_survives() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let bytes = text_pdf_bytes(&["SECRET data"]);
        open_mem_doc(&state, "doc1", bytes.clone());

        let regions = find_redaction_matches_impl(
            &state,
            "doc1".to_string(),
            "SECRET".to_string(),
            false,
            false,
            false,
        )
        .expect("find matches");
        assert!(!regions.is_empty());

        // The wrong approach: text still in the content stream under the box.
        let (leaks, _ocr_ran, _violations) =
            verify_redactions(pdfium, &bytes, &regions, &["SECRET".to_string()], &empty_engine())
                .expect("verify");

        assert!(!leaks.is_empty(), "verification must report the surviving text as a leak");
    }

    /// The Info dictionary and XMP metadata echoing the redacted string are
    /// scrubbed from the output.
    #[test]
    fn redaction_scrubs_document_metadata() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        // Seed Info + XMP metadata that echo the redacted text.
        let bytes = {
            let mut doc = Document::load_mem(&text_pdf_bytes(&["SECRET data"])).expect("parse");
            let info_id = doc.add_object(dictionary! {
                "Title" => Object::string_literal("SECRET report"),
            });
            doc.trailer.set("Info", info_id);
            let meta_id = doc.add_object(Stream::new(
                dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
                b"<xmp>SECRET report</xmp>".to_vec(),
            ));
            let catalog_id = doc
                .trailer
                .get(b"Root")
                .and_then(Object::as_reference)
                .expect("root");
            doc.get_dictionary_mut(catalog_id)
                .expect("catalog")
                .set("Metadata", Object::Reference(meta_id));
            let mut out = Vec::new();
            doc.save_to(&mut out).expect("serialize");
            out
        };

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &[full_page_region(1)],
            &["SECRET".to_string()],
            150.0,
            &empty_engine(),
            &not_cancelled(),
        )
        .expect("apply");
        assert!(result.verified);

        let out = output.expect("output bytes");
        let doc = Document::load_mem(&out).expect("parse output");
        assert!(doc.trailer.get(b"Info").is_err(), "Info must be removed");
        assert!(
            doc.catalog().expect("catalog").get(b"Metadata").is_err(),
            "XMP Metadata must be removed"
        );
        // Strongest check: the metadata string survives nowhere in the raw bytes.
        let needle = b"SECRET report";
        assert!(
            !out.windows(needle.len()).any(|w| w == needle),
            "redacted metadata string must not survive in the output bytes"
        );
    }

    /// Structure-tree `/ActualText`, bookmark titles, and page-level
    /// `/Metadata`/`/PieceInfo` all duplicate text invisibly to pdfium's text
    /// APIs; the scrub must remove them and the output must carry no echo.
    #[test]
    fn redaction_scrubs_struct_tree_outlines_and_page_extras() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();

        let bytes = {
            let mut doc = Document::load_mem(&text_pdf_bytes(&["Top Secret"])).expect("parse");
            let elem = doc.add_object(dictionary! {
                "Type" => "StructElem",
                "S" => "P",
                "ActualText" => Object::string_literal("SECRET heading"),
            });
            let root = doc.add_object(dictionary! {
                "Type" => "StructTreeRoot",
                "K" => vec![Object::Reference(elem)],
            });
            let outlines_id = doc.new_object_id();
            let item = doc.add_object(dictionary! {
                "Title" => Object::string_literal("SECRET section"),
                "Parent" => Object::Reference(outlines_id),
            });
            doc.objects.insert(
                outlines_id,
                Object::Dictionary(dictionary! {
                    "Type" => "Outlines",
                    "First" => Object::Reference(item),
                    "Last" => Object::Reference(item),
                    "Count" => Object::Integer(1),
                }),
            );
            let meta_id = doc.add_object(Stream::new(
                dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
                b"<x>SECRET meta</x>".to_vec(),
            ));
            let catalog_id = doc
                .trailer
                .get(b"Root")
                .and_then(Object::as_reference)
                .expect("root");
            {
                let catalog = doc.get_dictionary_mut(catalog_id).expect("catalog");
                catalog.set("StructTreeRoot", Object::Reference(root));
                catalog.set("MarkInfo", Object::Dictionary(dictionary! { "Marked" => true }));
                catalog.set("Outlines", Object::Reference(outlines_id));
            }
            let page_id = *doc.get_pages().get(&1).expect("page 1");
            {
                let page = doc.get_dictionary_mut(page_id).expect("page");
                page.set("Metadata", Object::Reference(meta_id));
                page.set(
                    "PieceInfo",
                    Object::Dictionary(dictionary! {
                        "SomeApp" => dictionary! {
                            "Private" => Object::string_literal("SECRET note"),
                        },
                    }),
                );
            }
            let mut out = Vec::new();
            doc.save_to(&mut out).expect("serialize");
            out
        };

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &[full_page_region(1)],
            &["SECRET".to_string()],
            150.0,
            &empty_engine(),
            &not_cancelled(),
        )
        .expect("apply");
        assert!(
            result.verified,
            "leaks: {:?}, violations: {:?}",
            result.leaks, result.structural_violations
        );

        let out = output.expect("output bytes");
        let doc = Document::load_mem(&out).expect("parse output");
        let catalog = doc.catalog().expect("catalog");
        for key in ["StructTreeRoot", "MarkInfo", "Outlines"] {
            assert!(catalog.get(key.as_bytes()).is_err(), "/{key} must be removed");
        }
        let page = doc.get_dictionary(*doc.get_pages().get(&1).unwrap()).expect("page");
        assert!(page.get(b"Metadata").is_err(), "page /Metadata must be removed");
        assert!(page.get(b"PieceInfo").is_err(), "page /PieceInfo must be removed");
        for needle in [
            b"SECRET heading".as_slice(),
            b"SECRET section",
            b"SECRET meta",
            b"SECRET note",
        ] {
            assert!(
                !out.windows(needle.len()).any(|w| w == needle),
                "{} must not survive in the output bytes",
                String::from_utf8_lossy(needle)
            );
        }
    }

    /// A two-page document with one hierarchical text field (separate parent
    /// object holding /V, widget kids on both pages).
    fn hierarchical_form_pdf_bytes() -> Vec<u8> {
        let mut doc = Document::load_mem(&text_pdf_bytes(&["page one", "page two"])).expect("parse");
        let parent_id = doc.new_object_id();
        let widget = |doc: &mut Document| {
            doc.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "Widget",
                "Rect" => vec![
                    Object::Integer(20), Object::Integer(20),
                    Object::Integer(120), Object::Integer(50),
                ],
                "F" => Object::Integer(4),
                "Parent" => Object::Reference(parent_id),
            })
        };
        let w1 = widget(&mut doc);
        let w2 = widget(&mut doc);
        doc.objects.insert(
            parent_id,
            Object::Dictionary(dictionary! {
                "FT" => "Tx",
                "T" => Object::string_literal("ssn"),
                "V" => Object::string_literal("SECRET-123"),
                "Kids" => vec![Object::Reference(w1), Object::Reference(w2)],
            }),
        );
        let pages = doc.get_pages();
        for (page_num, w) in [(1u32, w1), (2u32, w2)] {
            let page_id = *pages.get(&page_num).expect("page");
            doc.get_dictionary_mut(page_id)
                .expect("page dict")
                .set("Annots", vec![Object::Reference(w)]);
        }
        let acroform_id = doc.add_object(dictionary! {
            "Fields" => vec![Object::Reference(parent_id)],
        });
        let catalog_id = doc
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .expect("root");
        doc.get_dictionary_mut(catalog_id)
            .expect("catalog")
            .set("AcroForm", Object::Reference(acroform_id));
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize");
        out
    }

    /// A hierarchical field whose widgets ALL sat on flattened pages is
    /// dropped — its /V (typed by the user, invisible to text extraction)
    /// must not survive.
    #[test]
    fn hierarchical_field_with_all_widgets_flattened_is_dropped() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let bytes = hierarchical_form_pdf_bytes();

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &[full_page_region(1), full_page_region(2)],
            &[],
            150.0,
            &empty_engine(),
            &not_cancelled(),
        )
        .expect("apply");
        assert!(
            result.verified,
            "leaks: {:?}, violations: {:?}",
            result.leaks, result.structural_violations
        );

        let out = output.expect("output bytes");
        let doc = Document::load_mem(&out).expect("parse output");
        assert!(acroform_field_ids(&doc).is_empty(), "orphaned field must be dropped");
        let needle = b"SECRET-123";
        assert!(
            !out.windows(needle.len()).any(|w| w == needle),
            "the field value must not survive in the output bytes"
        );
    }

    /// A field with a surviving widget on an unredacted page keeps its value
    /// (it is visible content the user chose not to redact), but the dead
    /// widget is pruned from its /Kids.
    #[test]
    fn hierarchical_field_with_surviving_widget_is_kept_and_pruned() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let bytes = hierarchical_form_pdf_bytes();

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &[full_page_region(1)], // page 2's widget survives
            &[],
            150.0,
            &empty_engine(),
            &not_cancelled(),
        )
        .expect("apply");
        assert!(
            result.verified,
            "leaks: {:?}, violations: {:?}",
            result.leaks, result.structural_violations
        );

        let doc = Document::load_mem(&output.expect("output bytes")).expect("parse output");
        let fields = acroform_field_ids(&doc);
        assert_eq!(fields.len(), 1, "the partially-live field must be kept");
        let parent = doc.get_dictionary(fields[0]).expect("field");
        assert!(parent.get(b"V").is_ok(), "the surviving field keeps its value");
        let kids = parent.get(b"Kids").and_then(|o| o.as_array()).expect("kids");
        assert_eq!(kids.len(), 1, "the flattened page's widget must be pruned from /Kids");
    }

    /// Check 4 fails closed: if a text-bearing structure the text checks can't
    /// see survives into the output (here: an injected structure tree), the
    /// verifier refuses to certify even though checks 1–3 pass.
    #[test]
    fn verification_fails_closed_on_surviving_struct_tree() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let bytes = text_pdf_bytes(&["Top Secret"]);
        let regions = [full_page_region(1)];

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &regions,
            &[],
            150.0,
            &empty_engine(),
            &not_cancelled(),
        )
        .expect("apply");
        assert!(result.verified, "baseline must verify");

        // Simulate a scrub regression: re-inject a structure tree.
        let tampered = {
            let mut doc = Document::load_mem(&output.expect("output")).expect("parse");
            let elem = doc.add_object(dictionary! {
                "Type" => "StructElem",
                "S" => "P",
                "ActualText" => Object::string_literal("SECRET heading"),
            });
            let root = doc.add_object(dictionary! {
                "Type" => "StructTreeRoot",
                "K" => vec![Object::Reference(elem)],
            });
            let catalog_id = doc.trailer.get(b"Root").and_then(Object::as_reference).unwrap();
            doc.get_dictionary_mut(catalog_id)
                .unwrap()
                .set("StructTreeRoot", Object::Reference(root));
            let mut out = Vec::new();
            doc.save_to(&mut out).expect("serialize");
            out
        };

        let (leaks, _ocr, violations) =
            verify_redactions(pdfium, &tampered, &regions, &[], &empty_engine())
                .expect("verify");
        assert!(leaks.is_empty(), "checks 1–3 see nothing — that is the point");
        assert!(
            violations.iter().any(|v| v.contains("StructTreeRoot")),
            "check 4 must flag the surviving structure tree, got: {violations:?}"
        );
    }

    // ── Supporting tests ─────────────────────────────────────────────────────

    /// Points→pixels mapping: a centered region on a half-scale render maps to
    /// the expected pixel window (plus the 1pt burn pad).
    #[test]
    fn region_to_pixels_maps_and_clamps() {
        // 200pt page rendered at 100px → scale 0.5.
        let rect = TextRect { x: 50.0, y: 50.0, width: 100.0, height: 100.0 };
        let (x0, y0, x1, y1) = region_to_pixels(&rect, 0.0, 100, 100, 200.0, 200.0).expect("px");
        assert_eq!((x0, y0, x1, y1), (25, 25, 75, 75));

        // Padding expands outward; clamping keeps it inside the bitmap.
        let edge = TextRect { x: -10.0, y: 190.0, width: 300.0, height: 50.0 };
        let (x0, y0, x1, y1) = region_to_pixels(&edge, 2.0, 100, 100, 200.0, 200.0).expect("px");
        assert_eq!((x0, y0), (0, 94));
        assert_eq!((x1, y1), (100, 100));

        // A rect entirely off-page maps to nothing.
        let off = TextRect { x: 300.0, y: 300.0, width: 10.0, height: 10.0 };
        assert!(region_to_pixels(&off, 0.0, 100, 100, 200.0, 200.0).is_none());
    }

    /// Burned pixels are opaque black; pixels outside the region are untouched.
    #[test]
    fn burn_regions_blacks_out_only_the_region() {
        let (bw, bh) = (100u32, 100u32);
        let mut rgba = vec![255u8; (bw * bh * 4) as usize];
        let rect = TextRect { x: 80.0, y: 80.0, width: 40.0, height: 40.0 };
        burn_regions(&mut rgba, bw, bh, 200.0, 200.0, &[rect]);

        let px = |x: u32, y: u32| {
            let i = ((y * bw + x) * 4) as usize;
            (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
        };
        assert_eq!(px(50, 50), (0, 0, 0, 255), "region center must be black");
        assert_eq!(px(10, 10), (255, 255, 255, 255), "outside must be untouched");
        assert_eq!(px(90, 90), (255, 255, 255, 255), "outside must be untouched");
    }

    /// OCR engine simulating Windows OCR on a flattened page where a word in
    /// the *middle* of a line was burned: it "recognizes" the surviving words
    /// on either side of the gap (and nothing in small crops, like the black
    /// region crops the verification OCR check sends). Rects are bitmap pixel
    /// space, derived from the render scale, as the real engine reports them.
    struct RemainingWordsOcr;
    impl OcrEngine for RemainingWordsOcr {
        fn recognize(&self, _rgba: &[u8], w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            // A region crop (small) contains only burned pixels — no words.
            if w < 500 {
                return Ok(Vec::new());
            }
            // Full-page render of the 200pt page: scale = px per pt.
            // "ab SECRET cd" at 24pt from x=20: "ab" spans ~20..47 and "cd"
            // ~157..182 — both clear of the burned "SECRET" box (~53..151).
            let s = w as f32 / 200.0;
            let word = |text: &str, x_pt: f32, w_pt: f32| OcrWord {
                text: text.to_string(),
                // Pixel space, origin top-left; box spans y 145..157 pt.
                rect: TextRect {
                    x: x_pt * s,
                    y: (200.0 - 157.0) * s,
                    width: w_pt * s,
                    height: 12.0 * s,
                },
            };
            Ok(vec![word("ab", 20.0, 27.0), word("cd", 157.0, 25.0)])
        }
    }

    /// Regression: redacting a word in the middle of a line must stay verified
    /// after the re-OCR pass. The layer author must not write a line run that
    /// spans the burned gap (a line-unioned, Tz-stretched run would position
    /// invisible glyphs inside the region and fail — falsely — check 1).
    #[test]
    fn reocr_layer_does_not_span_the_burned_gap() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let bytes = text_pdf_bytes(&["ab SECRET cd"]);
        open_mem_doc(&state, "doc1", bytes.clone());

        // Redact only the middle word — the burned gap splits the line.
        let regions = find_redaction_matches_impl(
            &state,
            "doc1".to_string(),
            "SECRET".to_string(),
            false,
            false,
            false,
        )
        .expect("find matches");
        assert_eq!(regions.len(), 1);

        let engine: Arc<dyn OcrEngine> = Arc::new(RemainingWordsOcr);
        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &regions,
            &["SECRET".to_string()],
            150.0,
            &engine,
            &not_cancelled(),
        )
        .expect("apply");

        assert_eq!(result.reocr_pages, 1, "the flattened page must get a re-OCR layer");
        assert!(
            result.verified,
            "middle-of-line redaction must verify clean; leaks: {:?}",
            result.leaks
        );

        // The surviving words are searchable in the output; the redacted one is gone.
        let out = output.expect("output bytes");
        let reloaded = pdfium.load_pdf_from_byte_vec(out, None).expect("reload");
        let text = reloaded.pages().get(0).expect("page").text().expect("text").all();
        assert!(text.contains("ab") && text.contains("cd"), "got: {text:?}");
        assert!(!text.contains("SECRET"), "got: {text:?}");
    }

    /// A pre-set cancel token stops the run before any output is produced.
    #[test]
    fn cancellation_produces_no_output() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let bytes = text_pdf_bytes(&["Top Secret"]);

        let (result, output) = apply_redactions_impl(
            &no_progress,
            pdfium,
            &bytes,
            &[full_page_region(1)],
            &[],
            150.0,
            &empty_engine(),
            &Arc::new(AtomicBool::new(true)),
        )
        .expect("apply");

        assert!(result.cancelled);
        assert!(output.is_none());
    }

    /// The trust gates on Save As: an unverified staging refuses to save, a
    /// verified one writes the bytes and clears the staging, and the original
    /// file path is never an accepted destination.
    #[test]
    fn save_redacted_copy_gates_on_verification_and_protects_original() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let bytes = text_pdf_bytes(&["Top Secret"]);

        // Open the "original" from a real temp file so path comparisons work.
        let original = std::env::temp_dir().join(format!("tumbler-redact-{}.pdf", uuid::Uuid::new_v4()));
        std::fs::write(&original, &bytes).expect("write original");
        let original = dunce::canonicalize(&original).expect("canonical");
        let entry = DocEntry::load(pdfium, &original.to_string_lossy(), None).expect("load");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let stage = |verified: bool| {
            let document = pdfium
                .load_pdf_from_byte_vec(bytes.clone(), None)
                .expect("load staged");
            state
                .set_pending_redaction(
                    "doc1",
                    PendingRedaction { document, bytes: bytes.clone(), verified },
                )
                .expect("stage");
        };

        // No staging at all → refuse.
        let dest = std::env::temp_dir().join(format!("tumbler-redacted-{}.pdf", uuid::Uuid::new_v4()));
        let dest_str = dest.to_string_lossy().into_owned();
        assert!(save_redacted_copy_impl(&state, "doc1", &dest_str).is_err());

        // Unverified staging → refuse.
        stage(false);
        let err = save_redacted_copy_impl(&state, "doc1", &dest_str).expect_err("must refuse");
        assert!(err.to_string().contains("Verification failed"), "got: {err}");
        assert!(!dest.exists(), "a refused save must not write anything");

        // Verified staging over the ORIGINAL path → refuse.
        stage(true);
        let err = save_redacted_copy_impl(&state, "doc1", &original.to_string_lossy())
            .expect_err("must never overwrite the original");
        assert!(err.to_string().contains("original"), "got: {err}");
        assert_eq!(
            std::fs::read(&original).expect("read original"),
            bytes,
            "original file must be untouched"
        );

        // Verified staging to a new path → writes and clears the staging.
        let saved = save_redacted_copy_impl(&state, "doc1", &dest_str).expect("save");
        assert_eq!(std::fs::read(&dest).expect("read saved"), bytes);
        assert!(!saved.is_empty());
        assert!(
            state.get_pending_redaction("doc1").is_none(),
            "staging must be cleared after a successful save"
        );

        std::fs::remove_file(&original).ok();
        std::fs::remove_file(&dest).ok();
    }
}

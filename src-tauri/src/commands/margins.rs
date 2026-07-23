//! Expand Margins — the "fit music to page" tool (issue: sheet-music PDFs
//! leave the engraved content small inside generous margins).
//!
//! Two-phase design:
//!
//! 1. **Detect** (`analyze_margins`, pdfium): render every page at low DPI,
//!    threshold to ink-vs-background, and compute each page's ink bounding
//!    box. Raster detection handles scans and born-digital files identically.
//! 2. **Apply** (`expand_margins`, lopdf on the buffer): scale the content up
//!    by one *uniform* document-wide factor — limited by the fullest page, so
//!    staff size stays consistent — re-centering horizontally and top-aligning
//!    vertically, leaving `padding_pt` of margin. The existing content streams
//!    are wrapped in a tagged `q <matrix> cm … Q` pair; annotation rects are
//!    transformed by the same matrix so typewriter notes (issue #99) stay
//!    glued to the music.
//!
//! Re-running the tool composes with (and replaces) the previous wrap rather
//! than nesting, so repeated applications converge instead of compounding.
//! Like every edit (issue #31) this is a buffer edit: nothing touches disk
//! until the user saves.
//!
//! Coordinate spaces:
//! - **Display space** — the page as pdfium renders it (`/Rotate` applied),
//!   origin bottom-left, y up, in points. Detection reports display-space
//!   boxes; the fit is computed here because padding/centering are visual.
//! - **User space** — raw content coordinates (what `cm` operates in). The
//!   display fit is conjugated through the page's rotation to produce the
//!   user-space wrap matrix.

use crate::commands::pages::{emit_pages_edited, page_info_from_doc};
use crate::commands::text_layer::contents_refs;
use crate::commands::typewriter::{
    object_as_f32, page_annot_refs, read_typewriter_annots, write_typewriter_annots,
    TypewriterAnnot,
};
use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use pdfium_render::prelude::*;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

/// Detection render resolution. 72 DPI makes 1 bitmap pixel ≈ 1 point, which
/// is plenty to locate staff systems while keeping a full-document scan fast.
const DETECT_DPI: f32 = 72.0;
/// A pixel is "ink" when its darkest channel is below this (0–255). Catches
/// black notation and saturated color marks while ignoring near-white paper.
const INK_THRESHOLD: u8 = 200;
/// Fraction of a row/column that must be ink for it to count as content.
/// Rejects isolated specks and light scanner noise.
const MIN_DENSITY: f32 = 0.005;
/// Consecutive qualifying rows/columns required before content "starts".
/// A single noisy scanline can't move the bounding box.
const MIN_RUN: usize = 2;

/// Stream-dict tag on the prepended `q … cm` wrap stream, so a re-apply can
/// find, read back, and replace the previous fit instead of nesting another.
const FIT_TAG: &[u8] = b"TumblerFit";
/// Stream-dict tag on the appended `Q` partner stream.
const FIT_END_TAG: &[u8] = b"TumblerFitEnd";

// ── Report types ─────────────────────────────────────────────────────────────

/// Ink bounding box in display points, origin bottom-left, y up.
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
pub struct InkBbox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// One page's detected geometry, in display points. `bbox` is `None` for a
/// blank page (no qualifying ink).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PageMargins {
    pub page_w: f32,
    pub page_h: f32,
    pub bbox: Option<InkBbox>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MarginsReport {
    pub pages: Vec<PageMargins>,
    pub cancelled: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MarginsProgress {
    page: u32,
    page_count: u32,
}

/// Result of an apply: the uniform scale that was written (1.0 = unchanged).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExpandResult {
    pub scale: f32,
    pub cancelled: bool,
}

// ── Ink detection (pure) ─────────────────────────────────────────────────────

/// First and last index of a run of at least [`MIN_RUN`] consecutive entries
/// meeting `min_count`. `None` when no such run exists.
fn run_extent(counts: &[u32], min_count: u32) -> Option<(u32, u32)> {
    let qualifies: Vec<bool> = counts.iter().map(|&c| c >= min_count).collect();
    let n = qualifies.len();
    if n < MIN_RUN {
        return None;
    }
    let first = (0..=n - MIN_RUN).find(|&i| qualifies[i..i + MIN_RUN].iter().all(|&q| q))?;
    let last = (MIN_RUN - 1..n)
        .rev()
        .find(|&i| qualifies[i + 1 - MIN_RUN..=i].iter().all(|&q| q))?;
    Some((first as u32, last as u32))
}

/// Ink bounding box of an RGBA bitmap in pixel coordinates (y down, inclusive),
/// or `None` for a blank page. Uses row/column ink-density profiles so isolated
/// specks can't stretch the box.
fn ink_bbox_px(rgba: &[u8], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let (w_us, h_us) = (w as usize, h as usize);
    if rgba.len() < w_us * h_us * 4 {
        return None;
    }
    let mut row_counts = vec![0u32; h_us];
    let mut col_counts = vec![0u32; w_us];
    for y in 0..h_us {
        let row = &rgba[y * w_us * 4..][..w_us * 4];
        for x in 0..w_us {
            let px = &row[x * 4..][..3];
            if px[0].min(px[1]).min(px[2]) < INK_THRESHOLD {
                row_counts[y] += 1;
                col_counts[x] += 1;
            }
        }
    }
    let min_row = ((w as f32 * MIN_DENSITY).ceil() as u32).max(1);
    let min_col = ((h as f32 * MIN_DENSITY).ceil() as u32).max(1);
    let (y0, y1) = run_extent(&row_counts, min_row)?;
    let (x0, x1) = run_extent(&col_counts, min_col)?;
    Some((x0, y0, x1, y1))
}

/// Renders one page at [`DETECT_DPI`] and returns its detected geometry in
/// display points. Annotations are left out of the render (Tumbler draws them
/// as overlays; their rects are transformed separately on apply).
fn detect_page(document: &PdfDocument, index: u32) -> Result<PageMargins, AppError> {
    let page = document
        .pages()
        .get(index as i32)
        .map_err(|e| AppError::pdfium(format!("Failed to get page {}", index + 1), e))?;
    let page_w = page.width().value;
    let page_h = page.height().value;
    let target_width = ((page_w / 72.0) * DETECT_DPI).round().max(1.0) as u32;

    let config = PdfRenderConfig::new()
        .set_target_width(target_width as Pixels)
        .render_annotations(false);
    let bitmap = page
        .render_with_config(&config)
        .map_err(|e| AppError::pdfium(format!("Failed to render page {}", index + 1), e))?;
    let (bw, bh) = (bitmap.width() as u32, bitmap.height() as u32);
    let rgba = bitmap.as_rgba_bytes();

    let bbox = ink_bbox_px(&rgba, bw, bh).map(|(x0, y0, x1, y1)| {
        let sx = page_w / bw as f32;
        let sy = page_h / bh as f32;
        InkBbox {
            x0: x0 as f32 * sx,
            x1: (x1 + 1) as f32 * sx,
            // Bitmap y runs top-down; display space is y-up.
            y0: page_h - (y1 + 1) as f32 * sy,
            y1: page_h - y0 as f32 * sy,
        }
    });
    Ok(PageMargins {
        page_w,
        page_h,
        bbox,
    })
}

/// Scans every page for its ink bounding box, reporting progress and honoring
/// the cancel token. Locks the document only per page so other tabs stay
/// responsive.
fn analyze_margins_impl(
    emit_progress: &impl Fn(u32, u32),
    entry: &Arc<Mutex<DocEntry>>,
    cancel: &AtomicBool,
) -> Result<MarginsReport, AppError> {
    let page_count = lock_mutex(entry)?.document.pages().len() as u32;
    let mut pages = Vec::with_capacity(page_count as usize);
    for i in 0..page_count {
        if cancel.load(Ordering::Relaxed) {
            return Ok(MarginsReport {
                pages,
                cancelled: true,
            });
        }
        emit_progress(i + 1, page_count);
        let pm = {
            let entry = lock_mutex(entry)?;
            detect_page(&entry.document, i)?
        };
        pages.push(pm);
    }
    Ok(MarginsReport {
        pages,
        cancelled: false,
    })
}

// ── Fit math (pure) ──────────────────────────────────────────────────────────

/// Affine matrix in PDF `cm` order: `x' = a·x + c·y + e`, `y' = b·x + d·y + f`.
type Mat = [f32; 6];

const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Composes two affines: apply `m1` first, then `m2`.
fn mat_compose(m1: Mat, m2: Mat) -> Mat {
    let [a1, b1, c1, d1, e1, f1] = m1;
    let [a2, b2, c2, d2, e2, f2] = m2;
    [
        a1 * a2 + b1 * c2,
        a1 * b2 + b1 * d2,
        c1 * a2 + d1 * c2,
        c1 * b2 + d1 * d2,
        e1 * a2 + f1 * c2 + e2,
        e1 * b2 + f1 * d2 + f2,
    ]
}

fn mat_invert(m: Mat) -> Mat {
    let [a, b, c, d, e, f] = m;
    let det = a * d - b * c;
    let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
    [ia, ib, ic, id, -(e * ia + f * ic), -(e * ib + f * id)]
}

fn mat_apply(m: Mat, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// The uniform document-wide scale: the largest factor that still fits every
/// non-blank page's ink box inside its page minus `padding` on all sides.
/// `None` when no page has any ink.
pub(crate) fn uniform_scale(pages: &[PageMargins], padding: f32) -> Option<f32> {
    let mut s = f32::INFINITY;
    for p in pages {
        if let Some(b) = &p.bbox {
            let (bw, bh) = (b.x1 - b.x0, b.y1 - b.y0);
            if bw <= 0.0 || bh <= 0.0 {
                continue;
            }
            let avail_w = (p.page_w - 2.0 * padding).max(1.0);
            let avail_h = (p.page_h - 2.0 * padding).max(1.0);
            s = s.min(avail_w / bw).min(avail_h / bh);
        }
    }
    s.is_finite().then_some(s)
}

/// Display-space fit for one page at the document scale `s`: centered
/// horizontally, top-aligned vertically (music reads top-down; a half-empty
/// last page must not float).
fn display_fit(p: &PageMargins, b: &InkBbox, s: f32, padding: f32) -> Mat {
    let bw = b.x1 - b.x0;
    let dtx = (p.page_w - s * bw) / 2.0 - s * b.x0;
    let dty = (p.page_h - padding) - s * b.y1;
    [s, 0.0, 0.0, s, dtx, dty]
}

/// Maps user space to display space for a given `/Rotate` and effective page
/// box `[ex0, ey0, ex1, ey1]`. `/Rotate 90` displays the page rotated 90°
/// clockwise, so e.g. the user-space bottom-left corner lands at the display
/// top-left. Verified against pdfium's rendering in the rotation tests below.
fn user_to_display(rotate: i64, ebox: [f32; 4]) -> Mat {
    let [ex0, ey0, ex1, ey1] = ebox;
    match rotate.rem_euclid(360) {
        90 => [0.0, -1.0, 1.0, 0.0, -ey0, ex1],
        180 => [-1.0, 0.0, 0.0, -1.0, ex1, ey1],
        270 => [0.0, 1.0, -1.0, 0.0, ey1, -ex0],
        _ => [1.0, 0.0, 0.0, 1.0, -ex0, -ey0],
    }
}

/// The user-space wrap matrix realizing a display-space fit: conjugates the
/// fit through the page rotation (`R⁻¹ ∘ D ∘ R`). The rotation cancels in the
/// linear part, so the result is always a pure uniform scale + translation.
fn user_fit_matrix(display: Mat, rotate: i64, ebox: [f32; 4]) -> Mat {
    let r = user_to_display(rotate, ebox);
    mat_compose(mat_compose(r, display), mat_invert(r))
}

// ── lopdf page geometry ──────────────────────────────────────────────────────

/// Resolves a page's (possibly inherited) rect entry — `/MediaBox` or
/// `/CropBox` — normalized so `x0 < x1`, `y0 < y1`.
fn inherited_rect(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<[f32; 4]> {
    let mut current = page_id;
    for _ in 0..64 {
        let dict = doc.get_object(current).ok()?.as_dict().ok()?;
        if let Ok(value) = dict.get(key) {
            let arr = match value {
                Object::Reference(r) => doc.get_object(*r).ok()?.as_array().ok()?,
                Object::Array(a) => a,
                _ => return None,
            };
            if arr.len() >= 4 {
                let v: Vec<f32> = arr.iter().map(object_as_f32).collect();
                return Some([
                    v[0].min(v[2]),
                    v[1].min(v[3]),
                    v[0].max(v[2]),
                    v[1].max(v[3]),
                ]);
            }
        }
        current = dict.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

/// A page's (possibly inherited) `/Rotate`, defaulting to 0.
fn inherited_rotate(doc: &Document, page_id: ObjectId) -> i64 {
    let mut current = page_id;
    for _ in 0..64 {
        let Some(dict) = doc.get_object(current).ok().and_then(|o| o.as_dict().ok()) else {
            return 0;
        };
        match dict.get(b"Rotate") {
            Ok(Object::Integer(r)) => return *r,
            _ => {}
        }
        match dict.get(b"Parent").and_then(|p| p.as_reference()) {
            Ok(parent) => current = parent,
            Err(_) => return 0,
        }
    }
    0
}

/// The box pdfium displays: `/CropBox` when present, else `/MediaBox`, else
/// US Letter.
fn effective_box(doc: &Document, page_id: ObjectId) -> [f32; 4] {
    inherited_rect(doc, page_id, b"CropBox")
        .or_else(|| inherited_rect(doc, page_id, b"MediaBox"))
        .unwrap_or([0.0, 0.0, 612.0, 792.0])
}

// ── Wrap plumbing (lopdf) ────────────────────────────────────────────────────

/// Removes a previous Tumbler fit wrap from the page, returning its matrix
/// (identity when there was none) so the new fit can compose with it.
fn take_existing_fit(doc: &mut Document, page_id: ObjectId) -> Mat {
    let refs = contents_refs(doc, page_id);
    let mut matrix = IDENTITY;
    let mut to_delete = Vec::new();
    let mut kept = Vec::new();
    for r in refs {
        let tag = doc
            .get_object(r)
            .ok()
            .and_then(|o| o.as_stream().ok())
            .map(|s| (s.dict.has(FIT_TAG), s.dict.has(FIT_END_TAG)));
        match tag {
            Some((true, _)) => {
                if let Some(m) = fit_stream_matrix(doc, r) {
                    matrix = m;
                }
                to_delete.push(r);
            }
            Some((_, true)) => to_delete.push(r),
            _ => kept.push(r),
        }
    }
    if !to_delete.is_empty() {
        if let Ok(page) = doc.get_object_mut(page_id).and_then(|o| o.as_dict_mut()) {
            match kept.len() {
                0 => {
                    page.remove(b"Contents");
                }
                1 => page.set("Contents", Object::Reference(kept[0])),
                _ => page.set(
                    "Contents",
                    Object::Array(kept.into_iter().map(Object::Reference).collect()),
                ),
            }
        }
        for id in to_delete {
            doc.objects.remove(&id);
        }
    }
    matrix
}

/// Reads the `cm` matrix out of a tagged fit wrap stream.
fn fit_stream_matrix(doc: &Document, id: ObjectId) -> Option<Mat> {
    let stream = doc.get_object(id).ok()?.as_stream().ok()?;
    let data = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    let content = Content::decode(&data).ok()?;
    let op = content.operations.iter().find(|op| op.operator == "cm")?;
    if op.operands.len() < 6 {
        return None;
    }
    let mut m = IDENTITY;
    for (slot, obj) in m.iter_mut().zip(op.operands.iter()) {
        *slot = object_as_f32(obj);
    }
    Some(m)
}

/// Brackets the page's content streams in tagged `q <m> cm` / `Q` streams.
fn wrap_page_content(doc: &mut Document, page_id: ObjectId, m: Mat) -> Result<(), AppError> {
    let encode = |ops: Vec<Operation>| {
        Content { operations: ops }
            .encode()
            .map_err(|e| AppError::lopdf("Failed to encode fit wrap", e))
    };
    let pre_bytes = encode(vec![
        Operation::new("q", vec![]),
        Operation::new("cm", m.iter().map(|v| Object::Real(*v)).collect()),
    ])?;
    let post_bytes = encode(vec![Operation::new("Q", vec![])])?;

    let mut pre_dict = Dictionary::new();
    pre_dict.set(FIT_TAG, Object::Boolean(true));
    let pre_id = doc.add_object(Object::Stream(Stream::new(pre_dict, pre_bytes)));
    let mut post_dict = Dictionary::new();
    post_dict.set(FIT_END_TAG, Object::Boolean(true));
    let post_id = doc.add_object(Object::Stream(Stream::new(post_dict, post_bytes)));

    let existing = contents_refs(doc, page_id);
    let mut refs = Vec::with_capacity(existing.len() + 2);
    refs.push(Object::Reference(pre_id));
    refs.extend(existing.into_iter().map(Object::Reference));
    refs.push(Object::Reference(post_id));

    let page = doc
        .get_object_mut(page_id)
        .and_then(|o| o.as_dict_mut())
        .map_err(|e| AppError::lopdf("Failed to update page /Contents", e))?;
    page.set("Contents", Object::Array(refs));
    Ok(())
}

/// Transforms every annotation `/Rect` on the page by `m`, so annotations
/// (typewriter notes, links, foreign markup) track the scaled content.
fn transform_annot_rects(doc: &mut Document, page_id: ObjectId, m: Mat) {
    for r in page_annot_refs(doc, page_id) {
        let rect = doc
            .get_object(r)
            .ok()
            .and_then(|o| o.as_dict().ok())
            .and_then(|d| d.get(b"Rect").ok())
            .and_then(|o| o.as_array().ok())
            .filter(|a| a.len() >= 4)
            .map(|a| [
                object_as_f32(&a[0]),
                object_as_f32(&a[1]),
                object_as_f32(&a[2]),
                object_as_f32(&a[3]),
            ]);
        let Some([x1, y1, x2, y2]) = rect else { continue };
        let (nx1, ny1) = mat_apply(m, x1, y1);
        let (nx2, ny2) = mat_apply(m, x2, y2);
        if let Ok(dict) = doc.get_object_mut(r).and_then(|o| o.as_dict_mut()) {
            dict.set(
                "Rect",
                Object::Array(vec![
                    Object::Real(nx1.min(nx2)),
                    Object::Real(ny1.min(ny2)),
                    Object::Real(nx2.max(nx1)),
                    Object::Real(ny2.max(ny1)),
                ]),
            );
        }
    }
}

/// Transforms a typewriter note (top-left media-box coordinates) by the page's
/// user-space fit matrix. `m` is always a pure uniform scale + translation.
fn transform_note(note: &TypewriterAnnot, m: Mat, ox: f32, oy: f32, page_h: f32) -> TypewriterAnnot {
    let s = m[0];
    let (x1, _) = mat_apply(m, ox + note.x, 0.0);
    let (_, y2) = mat_apply(m, 0.0, oy + page_h - note.y);
    TypewriterAnnot {
        x: x1 - ox,
        y: (oy + page_h) - y2,
        width: s * note.width,
        height: s * note.height,
        font_size: s * note.font_size,
        ..note.clone()
    }
}

// ── Apply (pure with respect to AppState) ────────────────────────────────────

/// Applies the uniform fit to `buffer`, returning the new bytes and the scale.
/// `pages` is the detection report for the buffer's *current* rendered state,
/// so a previous fit wrap is composed into the new one (annotations, which
/// already sit in current coordinates, get only the incremental matrix).
pub(crate) fn expand_margins_bytes(
    buffer: &[u8],
    pages: &[PageMargins],
    padding: f32,
) -> Result<(Vec<u8>, f32), AppError> {
    let scale = uniform_scale(pages, padding)
        .ok_or_else(|| AppError::Other("No page content found to expand".to_string()))?;

    // Typewriter notes are pulled out first and re-written at the end: their
    // invisible text-layer streams must not sit inside the wrap (they would be
    // double-transformed when regenerated from the scaled rects), and the
    // round-trip regenerates appearance streams at the new size for free.
    let notes = read_typewriter_annots(buffer)?;
    let working = if notes.is_empty() {
        buffer.to_vec()
    } else {
        write_typewriter_annots(buffer, &[])?.unwrap_or_else(|| buffer.to_vec())
    };

    let mut doc = Document::load_mem(&working)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for margin expansion", e))?;
    let page_ids: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    if page_ids.len() != pages.len() {
        return Err(AppError::Other(
            "Page count changed since margin analysis; please retry".to_string(),
        ));
    }

    // Per-page incremental fit matrices, kept to transform the notes below.
    let mut note_fits: Vec<Mat> = Vec::with_capacity(page_ids.len());
    for (page_id, pm) in page_ids.iter().zip(pages) {
        let Some(bbox) = &pm.bbox else {
            note_fits.push(IDENTITY);
            continue;
        };
        let ebox = effective_box(&doc, *page_id);
        let rotate = inherited_rotate(&doc, *page_id);
        let m_new = user_fit_matrix(display_fit(pm, bbox, scale, padding), rotate, ebox);
        let m_old = take_existing_fit(&mut doc, *page_id);
        // Content reverts to its unwrapped coordinates once the old wrap is
        // removed, so the stream gets old-then-new; annotations were already
        // moved by the old fit and get only the increment.
        wrap_page_content(&mut doc, *page_id, mat_compose(m_old, m_new))?;
        transform_annot_rects(&mut doc, *page_id, m_new);
        note_fits.push(m_new);
    }

    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize expanded PDF", e))?;

    if notes.is_empty() {
        return Ok((out, scale));
    }

    let moved: Vec<TypewriterAnnot> = {
        let pages_map = doc.get_pages();
        notes
            .iter()
            .map(|n| {
                let (m, media) = pages_map
                    .get(&n.page)
                    .map(|pid| {
                        let idx = page_ids.iter().position(|p| p == pid).unwrap_or(0);
                        let media = inherited_rect(&doc, *pid, b"MediaBox")
                            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
                        (note_fits[idx], media)
                    })
                    .unwrap_or((IDENTITY, [0.0, 0.0, 612.0, 792.0]));
                let [ox, oy, _x1, y1] = media;
                transform_note(n, m, ox, oy, y1 - oy)
            })
            .collect()
    };
    let with_notes = write_typewriter_annots(&out, &moved)?.unwrap_or(out);
    Ok((with_notes, scale))
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Scans every page's ink bounding box so the Margins panel can show the
/// detected margins and compute the achievable enlargement for any padding.
#[tauri::command]
pub async fn analyze_margins(
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<MarginsReport, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_margins_job(cancel.clone());

    let emit = move |page, page_count| {
        let _ = window.emit("margins-progress", MarginsProgress { page, page_count });
    };
    let result = tauri::async_runtime::spawn_blocking(move || {
        analyze_margins_impl(&emit, &entry, &cancel)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_margins_job();
    result?.map_err(String::from)
}

/// Re-detects the ink boxes and applies the uniform fit to the document
/// buffer. A buffer edit (issue #31): the user commits with Save / Save As.
#[tauri::command]
pub async fn expand_margins(
    app: tauri::AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
    padding_pt: f32,
) -> Result<ExpandResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_margins_job(cancel.clone());

    let emit = move |page, page_count| {
        let _ = window.emit("margins-progress", MarginsProgress { page, page_count });
    };
    // Detection is re-run against the current buffer rather than trusting a
    // possibly-stale report from the panel (pages may have been edited since).
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let report = analyze_margins_impl(&emit, &entry, &cancel)?;
        if report.cancelled {
            return Ok(None);
        }
        let buffer = lock_mutex(&entry)?.buffer.clone();
        expand_margins_bytes(&buffer, &report.pages, padding_pt).map(Some)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_margins_job();
    match outcome?.map_err(String::from)? {
        None => Ok(ExpandResult {
            scale: 1.0,
            cancelled: true,
        }),
        Some((bytes, scale)) => {
            state
                .set_buffer_and_refresh(&doc_id, bytes)
                .map_err(String::from)?;
            let info = {
                let entry = state.get_document(&doc_id).map_err(String::from)?;
                let entry = lock_mutex(&entry).map_err(String::from)?;
                page_info_from_doc(&entry.document).map_err(String::from)?
            };
            emit_pages_edited(&app, &state, doc_id, &info);
            Ok(ExpandResult {
                scale,
                cancelled: false,
            })
        }
    }
}

/// Stops an in-progress margin analysis (or the detection pass of an apply)
/// after the current page.
#[tauri::command]
pub fn cancel_margins(state: State<'_, AppState>) {
    state.cancel_margins_job();
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn assert_bbox(b: &InkBbox, expect: [f32; 4], tol: f32) {
        for (got, want) in [b.x0, b.y0, b.x1, b.y1].iter().zip(expect) {
            assert!(
                approx(*got, want, tol),
                "bbox {b:?} != expected {expect:?} (tol {tol})"
            );
        }
    }

    /// One page of `page_w`×`page_h` with a filled black rectangle at `rect`
    /// (user space, `[x0, y0, x1, y1]`), optionally `/Rotate`d.
    fn rect_doc_bytes(page_w: f32, page_h: f32, rect: Option<[f32; 4]>, rotate: i64) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content = match rect {
            Some([x0, y0, x1, y1]) => {
                format!("0 0 0 rg {x0} {y0} {} {} re f", x1 - x0, y1 - y0)
            }
            None => String::new(),
        };
        let content_id =
            doc.add_object(Object::Stream(Stream::new(Dictionary::new(), content.into_bytes())));
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![
                Object::Real(0.0), Object::Real(0.0),
                Object::Real(page_w), Object::Real(page_h),
            ]),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Dictionary(Dictionary::new()),
        };
        if rotate != 0 {
            page_dict.set("Rotate", Object::Integer(rotate));
        }
        let page_id = doc.add_object(Object::Dictionary(page_dict));
        let pages_dict = dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(vec![Object::Reference(page_id)]),
            "Count" => Object::Integer(1),
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages_dict));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize fixture");
        out
    }

    fn detect_bytes(bytes: &[u8]) -> PageMargins {
        let doc = crate::test_pdfium()
            .load_pdf_from_byte_vec(bytes.to_vec(), None)
            .expect("load bytes");
        detect_page(&doc, 0).expect("detect")
    }

    fn count_fit_streams(bytes: &[u8]) -> usize {
        let doc = Document::load_mem(bytes).expect("parse");
        doc.objects
            .values()
            .filter(|o| {
                o.as_stream()
                    .map(|s| s.dict.has(FIT_TAG))
                    .unwrap_or(false)
            })
            .count()
    }

    // ── Pure detection ───────────────────────────────────────────────────────

    #[test]
    fn run_extent_finds_sustained_runs_and_rejects_singles() {
        // A lone qualifying entry is not a run.
        assert_eq!(run_extent(&[0, 5, 0, 0], 1), None);
        // Two consecutive qualifying entries bound the extent.
        assert_eq!(run_extent(&[0, 5, 5, 0], 1), Some((1, 2)));
        // The extent spans from the first run to the last, across gaps.
        assert_eq!(run_extent(&[0, 5, 5, 0, 0, 5, 5, 5, 0], 1), Some((1, 7)));
        assert_eq!(run_extent(&[], 1), None);
    }

    #[test]
    fn ink_bbox_px_finds_black_rect() {
        let (w, h) = (100u32, 80u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in 10..30 {
            for x in 20..40 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        assert_eq!(ink_bbox_px(&rgba, w, h), Some((20, 10, 39, 29)));
    }

    #[test]
    fn ink_bbox_px_blank_and_speck_are_none() {
        let (w, h) = (100u32, 80u32);
        let blank = vec![255u8; (w * h * 4) as usize];
        assert_eq!(ink_bbox_px(&blank, w, h), None);

        // A single dark pixel fails the consecutive-run requirement.
        let mut speck = blank;
        let i = ((40 * w + 50) * 4) as usize;
        speck[i..i + 3].copy_from_slice(&[0, 0, 0]);
        assert_eq!(ink_bbox_px(&speck, w, h), None);
    }

    // ── Fit math ─────────────────────────────────────────────────────────────

    #[test]
    fn mat_invert_round_trips() {
        for m in [
            [1.4, 0.0, 0.0, 1.4, -60.0, -30.0],
            [0.0, -1.0, 1.0, 0.0, -5.0, 300.0],
        ] {
            let round = mat_compose(m, mat_invert(m));
            for (got, want) in round.iter().zip(IDENTITY) {
                assert!(approx(*got, want, 1e-4), "{round:?} != identity");
            }
        }
    }

    #[test]
    fn uniform_scale_is_limited_by_fullest_page() {
        let pages = vec![
            PageMargins {
                page_w: 300.0,
                page_h: 400.0,
                bbox: Some(InkBbox { x0: 50.0, y0: 100.0, x1: 250.0, y1: 300.0 }),
            },
            // Fuller page: less room to grow.
            PageMargins {
                page_w: 300.0,
                page_h: 400.0,
                bbox: Some(InkBbox { x0: 10.0, y0: 20.0, x1: 290.0, y1: 380.0 }),
            },
            // Blank pages don't constrain the scale.
            PageMargins { page_w: 300.0, page_h: 400.0, bbox: None },
        ];
        let s = uniform_scale(&pages, 10.0).expect("scale");
        assert!(approx(s, 280.0 / 280.0, 1e-4), "s = {s}");

        assert_eq!(uniform_scale(&[pages[2].clone()], 10.0), None);
    }

    #[test]
    fn user_fit_matrix_is_display_fit_for_unrotated_page() {
        let d = [2.0, 0.0, 0.0, 2.0, 5.0, 7.0];
        let m = user_fit_matrix(d, 0, [0.0, 0.0, 300.0, 400.0]);
        for (got, want) in m.iter().zip(d) {
            assert!(approx(*got, want, 1e-4), "{m:?} != {d:?}");
        }
    }

    /// Conjugating through a rotation must cancel in the linear part (pure
    /// uniform scale), and must realize the display-space fit: a user point's
    /// display image, put through the display fit, matches the display image
    /// of the transformed user point.
    #[test]
    fn user_fit_matrix_realizes_display_fit_under_rotation() {
        let ebox = [0.0, 0.0, 300.0, 400.0];
        let d = [1.4, 0.0, 0.0, 1.4, -80.0, -60.0];
        for rotate in [0i64, 90, 180, 270] {
            let m = user_fit_matrix(d, rotate, ebox);
            assert!(approx(m[0], 1.4, 1e-4) && approx(m[3], 1.4, 1e-4), "{m:?}");
            assert!(approx(m[1], 0.0, 1e-4) && approx(m[2], 0.0, 1e-4), "{m:?}");

            let r = user_to_display(rotate, ebox);
            for (ux, uy) in [(50.0, 100.0), (250.0, 300.0), (0.0, 0.0)] {
                let (dx, dy) = mat_apply(r, ux, uy);
                let want = mat_apply(d, dx, dy);
                let (mx, my) = mat_apply(m, ux, uy);
                let got = mat_apply(r, mx, my);
                assert!(
                    approx(got.0, want.0, 1e-3) && approx(got.1, want.1, 1e-3),
                    "rotate {rotate}: {got:?} != {want:?}"
                );
            }
        }
    }

    // ── Integration (pdfium render round-trips) ──────────────────────────────

    /// The black rect's user-space position must be recovered from the render
    /// within ~2pt (1px ≈ 1pt at 72 DPI, plus antialiased edges).
    #[test]
    fn detect_page_finds_rect_bbox() {
        let _guard = crate::test_pdfium_guard();
        let bytes = rect_doc_bytes(300.0, 400.0, Some([50.0, 100.0, 250.0, 300.0]), 0);
        let pm = detect_bytes(&bytes);
        assert!(approx(pm.page_w, 300.0, 0.5) && approx(pm.page_h, 400.0, 0.5));
        assert_bbox(&pm.bbox.expect("bbox"), [50.0, 100.0, 250.0, 300.0], 2.5);
    }

    #[test]
    fn detect_page_blank_is_none() {
        let _guard = crate::test_pdfium_guard();
        let pm = detect_bytes(&rect_doc_bytes(300.0, 400.0, None, 0));
        assert!(pm.bbox.is_none());
    }

    /// End-to-end fit: 300×400 page, 200×200 rect, padding 10 → s = 1.4,
    /// centered horizontally and top-aligned: new bbox (10, 110)–(290, 390).
    #[test]
    fn expand_scales_content_to_fill_page() {
        let _guard = crate::test_pdfium_guard();
        let bytes = rect_doc_bytes(300.0, 400.0, Some([50.0, 100.0, 250.0, 300.0]), 0);
        let report = detect_bytes(&bytes);
        let (out, scale) = expand_margins_bytes(&bytes, &[report], 10.0).expect("expand");
        assert!(approx(scale, 1.4, 0.02), "scale = {scale}");

        let after = detect_bytes(&out);
        assert_bbox(&after.bbox.expect("bbox"), [10.0, 110.0, 290.0, 390.0], 3.0);
        assert_eq!(count_fit_streams(&out), 1);
    }

    /// Re-running replaces the previous wrap (compose, don't nest) and
    /// converges: the second pass finds margins already at target and applies
    /// scale ≈ 1.
    #[test]
    fn expand_twice_converges_with_single_wrap() {
        let _guard = crate::test_pdfium_guard();
        let bytes = rect_doc_bytes(300.0, 400.0, Some([50.0, 100.0, 250.0, 300.0]), 0);
        let report = detect_bytes(&bytes);
        let (once, _) = expand_margins_bytes(&bytes, &[report], 10.0).expect("first");

        let report2 = detect_bytes(&once);
        let (twice, scale2) = expand_margins_bytes(&once, &[report2], 10.0).expect("second");
        assert!(approx(scale2, 1.0, 0.03), "second scale = {scale2}");
        assert_eq!(count_fit_streams(&twice), 1);

        let after = detect_bytes(&twice);
        assert_bbox(&after.bbox.expect("bbox"), [10.0, 110.0, 290.0, 390.0], 4.0);
    }

    /// A `/Rotate 90` page: detection reports display-space geometry (pdfium
    /// swaps the axes), and the fit fills the *displayed* page. This also
    /// pins the module's rotation convention to pdfium's actual behavior.
    #[test]
    fn expand_handles_rotated_page() {
        let _guard = crate::test_pdfium_guard();
        let bytes = rect_doc_bytes(300.0, 400.0, Some([50.0, 100.0, 250.0, 300.0]), 90);
        let pm = detect_bytes(&bytes);
        assert!(approx(pm.page_w, 400.0, 0.5) && approx(pm.page_h, 300.0, 0.5));
        // User (50,100)-(250,300) under 90° CW display: (100,50)-(300,250).
        assert_bbox(&pm.bbox.expect("bbox"), [100.0, 50.0, 300.0, 250.0], 2.5);

        let (out, scale) = expand_margins_bytes(&bytes, &[pm], 10.0).expect("expand");
        assert!(approx(scale, 1.4, 0.02), "scale = {scale}");
        let after = detect_bytes(&out);
        assert_bbox(&after.bbox.expect("bbox"), [60.0, 10.0, 340.0, 290.0], 3.0);
    }

    #[test]
    fn expand_blank_document_errors_cleanly() {
        let _guard = crate::test_pdfium_guard();
        let bytes = rect_doc_bytes(300.0, 400.0, None, 0);
        let report = detect_bytes(&bytes);
        match expand_margins_bytes(&bytes, &[report], 10.0) {
            Err(AppError::Other(msg)) => assert!(msg.contains("No page content")),
            other => panic!("expected Other error, got {other:?}"),
        }
    }

    /// Annotation rects ride along with the content: a rect coinciding with
    /// the ink box corner must land on the fitted corner.
    #[test]
    fn expand_transforms_annotation_rects() {
        let _guard = crate::test_pdfium_guard();
        let bytes = rect_doc_bytes(300.0, 400.0, Some([50.0, 100.0, 250.0, 300.0]), 0);
        let with_annot = {
            let mut doc = Document::load_mem(&bytes).expect("parse");
            let page_id = *doc.get_pages().get(&1).expect("page");
            let annot_id = doc.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "FreeText",
                "Rect" => Object::Array(vec![
                    Object::Real(50.0), Object::Real(100.0),
                    Object::Real(100.0), Object::Real(150.0),
                ]),
            });
            let page = doc
                .get_object_mut(page_id)
                .and_then(|o| o.as_dict_mut())
                .expect("page dict");
            page.set("Annots", Object::Array(vec![Object::Reference(annot_id)]));
            let mut out = Vec::new();
            doc.save_to(&mut out).expect("serialize");
            out
        };

        let report = detect_bytes(&with_annot);
        let (out, _) = expand_margins_bytes(&with_annot, &[report], 10.0).expect("expand");

        let doc = Document::load_mem(&out).expect("parse out");
        let page_id = *doc.get_pages().get(&1).expect("page");
        let refs = page_annot_refs(&doc, page_id);
        assert_eq!(refs.len(), 1);
        let rect = doc
            .get_object(refs[0])
            .ok()
            .and_then(|o| o.as_dict().ok())
            .and_then(|d| d.get(b"Rect").ok())
            .and_then(|o| o.as_array().ok())
            .map(|a| a.iter().map(object_as_f32).collect::<Vec<_>>())
            .expect("rect");
        // s = 1.4, tx = -60, ty = -30 (see expand_scales_content_to_fill_page).
        let expect = [10.0, 110.0, 80.0, 180.0];
        for (got, want) in rect.iter().zip(expect) {
            assert!(approx(*got, want, 0.1), "rect {rect:?} != {expect:?}");
        }
    }

    /// Ad-hoc harness for eyeballing the fit on a real file: set
    /// `TUMBLER_MARGINS_SAMPLE` to a PDF path and run
    /// `cargo test expand_sample_file -- --ignored --test-threads=1`;
    /// the fitted copy is written next to it as `<name>.expanded.pdf`.
    #[test]
    #[ignore]
    fn expand_sample_file() {
        let Ok(path) = std::env::var("TUMBLER_MARGINS_SAMPLE") else {
            eprintln!("TUMBLER_MARGINS_SAMPLE not set; skipping");
            return;
        };
        let _guard = crate::test_pdfium_guard();
        let bytes = std::fs::read(&path).expect("read sample");
        let doc = crate::test_pdfium()
            .load_pdf_from_byte_vec(bytes.clone(), None)
            .expect("load sample");
        let pages: Vec<PageMargins> = (0..doc.pages().len() as u32)
            .map(|i| detect_page(&doc, i).expect("detect"))
            .collect();
        drop(doc);
        for (i, p) in pages.iter().enumerate() {
            eprintln!("page {}: {:?}", i + 1, p);
        }
        let (out, scale) = expand_margins_bytes(&bytes, &pages, 18.0).expect("expand");
        eprintln!("uniform scale = {scale:.3}");
        let dest = format!("{path}.expanded.pdf");
        std::fs::write(&dest, out).expect("write output");
        eprintln!("wrote {dest}");
    }

    /// Typewriter notes survive: position, size, and font scale with the
    /// content, and the note still round-trips through read_typewriter_annots.
    #[test]
    fn expand_scales_typewriter_notes() {
        let _guard = crate::test_pdfium_guard();
        let bytes = rect_doc_bytes(300.0, 400.0, Some([50.0, 100.0, 250.0, 300.0]), 0);
        let note = TypewriterAnnot {
            id: "n1".to_string(),
            page: 1,
            x: 60.0,
            y: 120.0,
            width: 100.0,
            height: 20.0,
            text: "forte".to_string(),
            font_family: "Helvetica".to_string(),
            bold: false,
            italic: false,
            font_size: 12.0,
            color: [0.0, 0.0, 0.0],
        };
        let with_note = write_typewriter_annots(&bytes, std::slice::from_ref(&note))
            .expect("write note")
            .expect("changed");

        let report = detect_bytes(&with_note);
        let (out, _) = expand_margins_bytes(&with_note, &[report], 10.0).expect("expand");

        let notes = read_typewriter_annots(&out).expect("read notes");
        assert_eq!(notes.len(), 1);
        let n = &notes[0];
        // s = 1.4, tx = -60, ty = -30: x' = 1.4·60−60 = 24;
        // y2 = 400−120 = 280 → y2' = 1.4·280−30 = 362 → y' = 400−362 = 38.
        assert!(approx(n.x, 24.0, 0.1), "x = {}", n.x);
        assert!(approx(n.y, 38.0, 0.1), "y = {}", n.y);
        assert!(approx(n.width, 140.0, 0.1), "width = {}", n.width);
        assert!(approx(n.height, 28.0, 0.1), "height = {}", n.height);
        assert!(approx(n.font_size, 16.8, 0.05), "font = {}", n.font_size);
        assert_eq!(n.text, "forte");
    }
}

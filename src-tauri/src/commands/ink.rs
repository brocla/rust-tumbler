//! "Ink Signature" — freehand ink drawn straight onto a page (issue #120).
//!
//! A deliberately narrow drawing tool: one colour, one width, undo as the only
//! eraser. It exists for signing documents that have no signature form field,
//! not for annotation or drawing, and the scope is kept tight so it cannot
//! drift into either.
//!
//! Unlike the typewriter (issue #99), ink is **flattened into the page content
//! stream** rather than stored as an annotation. A signature is meant to become
//! part of the document: once saved it should not be selectable, draggable or
//! deletable in another editor the way a sticky note is. Flattening buys a lot
//! of simplicity as a side effect — there is no private key to tag, nothing to
//! re-hydrate on open, no read-back command, and no HTML overlay for committed
//! ink, because the page renderer draws it like any other page content. The
//! frontend's canvas overlay covers only the strokes not yet committed.
//!
//! Nothing here is a *digital* signature. Drawn ink carries no cryptographic
//! meaning, and — like every other edit — it breaks any existing `/ByteRange`
//! signature on the document. The frontend warns before the first commit,
//! reusing the same `isSigned` → confirm → re-verify path as Metadata, Margins
//! and Optimize.
//!
//! Like every edit (issue #31) this is a buffer edit: [`apply_ink`] rewrites
//! the in-memory buffer and marks the document dirty; an ordinary Save / Save
//! As commits it to disk. Closing without saving discards it, which is the only
//! "undo" once a stroke group has been committed.
//!
//! Coordinate note: the frontend sends stroke points in PDF points with a
//! **top-left** origin (the same space as search and redaction rectangles),
//! measured against the page as pdfium drew it. [`PageSpace`] flips them into
//! PDF user space, accounting for both the render box — CropBox when present,
//! else MediaBox — and the page's `/Rotate` (issue #121).
//!
//! Rotation needs nothing beyond mapping each point, because the strokes are
//! the only geometry here and a quarter turn is rigid: the round caps are
//! symmetric and the stroke width is isotropic, so a rotated polyline is just
//! a polyline through rotated points. No `cm` wrap is required, and keeping
//! the transform inside the point loop leaves [`ink_content_stream`] a pure
//! function of the space it is given.

use crate::commands::text_layer::{append_content_stream, contents_refs};
use crate::commands::page_space::PageSpace;
use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use lopdf::{Dictionary, Document, Object, Stream};
use tauri::State;

/// Ink colour, `#0B35B8` — "bright ballpoint".
///
/// Blue because a signature should not photocopy as though it were part of the
/// printed form; this shade stays visibly blue-grey when desaturated, where a
/// blue-black goes almost as dark as the surrounding print. Pure `#0000FF` was
/// rejected for reading as a hyperlink rather than ink.
pub const INK_RGB: [f32; 3] = [0.043, 0.208, 0.722];

/// Stroke width in points, shared with the `/Sig` form-field signature canvas
/// so the two ink paths cannot drift apart.
pub const INK_WIDTH_PT: f32 = 1.5;

/// A polyline of points in PDF points, top-left origin.
pub type Stroke = Vec<[f32; 2]>;

/// Builds the content stream for one page's worth of ink.
///
/// `space` carries the render box and the page's rotation, so a point the
/// frontend measured against the page it could see lands where the user drew
/// it. Returns an empty vector when there is nothing to draw, which the caller
/// treats as "no edit" rather than an error.
pub(crate) fn ink_content_stream(strokes: &[Stroke], space: &PageSpace) -> Vec<u8> {
    if strokes.iter().all(|s| s.is_empty()) {
        return Vec::new();
    }

    let [r, g, b] = INK_RGB;
    // Wrapped in q/Q so the ink cannot leak its colour, width or line style
    // into anything appended after it.
    let mut s = format!("q\n{r:.3} {g:.3} {b:.3} RG\n{INK_WIDTH_PT} w 1 J 1 j\n");

    for stroke in strokes {
        for (i, p) in stroke.iter().enumerate() {
            let [x, y] = space.to_user(*p);
            if i == 0 {
                s.push_str(&format!("{x:.2} {y:.2} m\n"));
                // A single-point stroke becomes a zero-length line; with a
                // round cap that renders as a dot, so a tap leaves a mark
                // instead of nothing.
                if stroke.len() == 1 {
                    s.push_str(&format!("{x:.2} {y:.2} l\n"));
                }
            } else {
                s.push_str(&format!("{x:.2} {y:.2} l\n"));
            }
        }
        if !stroke.is_empty() {
            s.push_str("S\n");
        }
    }

    s.push_str("Q\n");
    s.into_bytes()
}

/// Flattens one page's stroke group into the document buffer.
///
/// `page` is 1-based. Empty input is a no-op: the document is left clean rather
/// than marked dirty for a group the user drew nothing into.
#[tauri::command]
pub fn apply_ink(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
    strokes: Vec<Stroke>,
) -> Result<(), String> {
    let changed = apply_ink_impl(&state, doc_id.clone(), page, strokes).map_err(String::from)?;
    if changed {
        // Ink rewrites the page's content stream, so the frontend has to drop
        // its cached bitmap and re-render — a dirty flag alone leaves the old
        // picture on screen and the signature looks like it vanished. This is
        // the same signal a page edit or a compression run sends, and it also
        // re-verifies the digital signature the ink just invalidated.
        let info = {
            let entry = state.get_document(&doc_id).map_err(String::from)?;
            let entry = lock_mutex(&entry).map_err(String::from)?;
            crate::commands::pages::page_info_from_doc(&entry.document).map_err(String::from)?
        };
        crate::commands::pages::emit_pages_edited(&app, &state, doc_id, &info);
    }
    Ok(())
}

pub(crate) fn apply_ink_impl(
    state: &AppState,
    doc_id: String,
    page: u32,
    strokes: Vec<Stroke>,
) -> Result<bool, AppError> {
    let entry = state.get_document(&doc_id)?;
    let buffer = {
        let entry = lock_mutex(&entry)?;
        entry.buffer.clone()
    };

    match write_ink(&buffer, page, &strokes)? {
        Some(bytes) => {
            state.set_buffer_and_refresh(&doc_id, bytes)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Appends the ink to `page` in `buffer`, returning the new document bytes, or
/// `None` when there was nothing to draw.
pub(crate) fn write_ink(
    buffer: &[u8],
    page: u32,
    strokes: &[Stroke],
) -> Result<Option<Vec<u8>>, AppError> {
    let mut doc = Document::load_mem(buffer)
        .map_err(|e| AppError::lopdf("Failed to parse PDF to add ink", e))?;

    let page_id = *doc
        .get_pages()
        .get(&page)
        .ok_or_else(|| AppError::Other(format!("Page {page} not found")))?;

    let stream_bytes = ink_content_stream(strokes, &PageSpace::of(&doc, page_id));
    if stream_bytes.is_empty() {
        return Ok(None);
    }

    // Everything needing `&mut Document` is created before the page borrow: the
    // ink stream, and the q/Q guard streams that reset the graphics state
    // around the page's existing content so a leftover CTM or open clip cannot
    // shift or clip the signature.
    let existing = contents_refs(&doc, page_id);
    let stream_id = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), stream_bytes)));
    let (save_id, restore_id) = if existing.is_empty() {
        (stream_id, stream_id) // unused when there is no content to wrap
    } else {
        (
            doc.add_object(Object::Stream(Stream::new(Dictionary::new(), b"q\n".to_vec()))),
            doc.add_object(Object::Stream(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()))),
        )
    };

    {
        let page_dict = doc
            .get_object_mut(page_id)
            .map_err(|e| AppError::lopdf(format!("Failed to get page {page}"), e))?
            .as_dict_mut()
            .map_err(|e| AppError::lopdf(format!("Page {page} is not a dictionary"), e))?;
        append_content_stream(page_dict, &existing, save_id, restore_id, stream_id);
    }

    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize PDF with ink", e))?;
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;

    fn fixture_state(pdfium: &'static pdfium_render::prelude::Pdfium) -> AppState {
        let state = AppState::new(pdfium, None);
        let src = crate::fixture_path();
        let entry = DocEntry::load(state.pdfium, &src.to_string_lossy(), None).expect("load pdf");
        state.insert_document("doc1".to_string(), entry).expect("insert");
        state
    }

    /// The fixture page is 200x200 with a (0,0) origin, so a point measured
    /// downward from the top of the page maps to `200 - y` in user space.
    #[test]
    fn ink_stream_flips_top_left_points_into_user_space() {
        let s = String::from_utf8(ink_content_stream(
            &[vec![[10.0, 0.0], [50.0, 200.0]]],
            &PageSpace::new(0, [0.0, 0.0, 200.0, 200.0]),
        ))
        .expect("utf8");

        assert!(s.contains("10.00 200.00 m"), "top edge flips to y=height: {s}");
        assert!(s.contains("50.00 0.00 l"), "bottom edge flips to y=0: {s}");
        assert!(s.trim_end().ends_with('Q'), "stream is balanced: {s}");
    }

    /// A page whose box does not start at the origin (a cropped scan) must
    /// offset the ink by that origin, or the signature lands off the visible
    /// area by exactly the crop.
    #[test]
    fn ink_stream_offsets_by_the_page_box_origin() {
        let s = String::from_utf8(ink_content_stream(
            &[vec![[0.0, 0.0]]],
            &PageSpace::new(0, [20.0, 30.0, 120.0, 130.0]),
        ))
        .expect("utf8");

        // x = x0 + 0, y = y0 + (height - 0)
        assert!(s.contains("20.00 130.00 m"), "origin not applied: {s}");
    }

    /// A tap should leave a dot. A round cap on a zero-length line draws one;
    /// a bare `m` with no line segment draws nothing at all.
    #[test]
    fn single_point_stroke_becomes_a_dot() {
        let s = String::from_utf8(ink_content_stream(
            &[vec![[10.0, 10.0]]],
            &PageSpace::new(0, [0.0, 0.0, 200.0, 200.0]),
        ))
        .expect("utf8");

        assert!(s.contains("10.00 190.00 m"), "{s}");
        assert!(s.contains("10.00 190.00 l"), "zero-length line missing: {s}");
        assert!(s.contains("1 J"), "round cap missing, dot would not render: {s}");
    }

    #[test]
    fn ink_stream_carries_the_agreed_colour_and_width() {
        let s = String::from_utf8(ink_content_stream(
            &[vec![[1.0, 1.0], [2.0, 2.0]]],
            &PageSpace::new(0, [0.0, 0.0, 10.0, 10.0]),
        ))
        .expect("utf8");

        // #0B35B8
        assert!(s.contains("0.043 0.208 0.722 RG"), "ink colour: {s}");
        assert!(s.contains("1.5 w"), "stroke width: {s}");
        assert!(s.contains("1 J 1 j"), "round caps and joins: {s}");
        assert!(s.starts_with("q\n"), "state must be isolated: {s}");
    }

    #[test]
    fn empty_input_produces_no_stream() {
        let space = PageSpace::new(0, [0.0, 0.0, 200.0, 200.0]);
        assert!(ink_content_stream(&[], &space).is_empty());
        assert!(ink_content_stream(&[vec![], vec![]], &space).is_empty());
    }

    /// An empty group must leave the document alone — no edit, no dirty flag,
    /// so closing a tool the user drew nothing into never prompts to save.
    #[test]
    fn empty_group_is_not_an_edit() {
        let pdfium = crate::test_pdfium();
        let state = fixture_state(pdfium.get());

        let changed = apply_ink_impl(&state, "doc1".to_string(), 1, vec![]).expect("apply");
        assert!(!changed, "empty group must not mark the document dirty");

        let entry = state.get_document("doc1").expect("get");
        let entry = crate::state::lock_mutex(&entry).expect("lock");
        assert!(!entry.dirty, "document should still be clean");
    }

    #[test]
    fn applying_ink_marks_the_document_dirty_and_leaves_disk_untouched() {
        let pdfium = crate::test_pdfium();
        let state = fixture_state(pdfium.get());
        let on_disk = std::fs::read(crate::fixture_path()).expect("read fixture");

        let changed = apply_ink_impl(
            &state,
            "doc1".to_string(),
            1,
            vec![vec![[20.0, 20.0], [80.0, 60.0]]],
        )
        .expect("apply");
        assert!(changed);

        let entry = state.get_document("doc1").expect("get");
        let entry = crate::state::lock_mutex(&entry).expect("lock");
        assert!(entry.dirty, "ink must mark the buffer dirty");
        assert_ne!(entry.buffer, on_disk, "buffer should differ from disk");
        assert_eq!(
            std::fs::read(crate::fixture_path()).expect("re-read fixture"),
            on_disk,
            "applying ink must never write to disk"
        );
    }

    /// Flattened ink is page content, so it must still be there after the
    /// document is parsed again — this is what makes it survive a save/reload
    /// and the page operations that rewrite the document.
    #[test]
    fn ink_survives_a_round_trip_through_the_document() {
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");
        let out = write_ink(&bytes, 1, &[vec![[20.0, 20.0], [80.0, 60.0]]])
            .expect("write")
            .expect("some bytes");

        let doc = Document::load_mem(&out).expect("reload");
        let page_id = *doc.get_pages().get(&1).expect("page 1");
        let content = doc.get_page_content(page_id).expect("content");
        let text = String::from_utf8_lossy(&content);

        assert!(text.contains("0.043 0.208 0.722 RG"), "ink colour lost: {text}");
        assert!(text.contains("20.00 180.00 m"), "stroke start lost: {text}");
    }

    /// The end-to-end question the unit tests cannot answer: after the ink is
    /// flattened, does pdfium actually *draw* it? Renders the edited document
    /// and looks for a blue pixel. If the stream were appended wrongly — bad
    /// /Contents array, unbalanced q/Q, coordinates off the page — the bytes
    /// would still be there and every other test would still pass, while the
    /// user saw nothing.
    #[test]
    fn flattened_ink_is_visible_in_a_render() {
        use pdfium_render::prelude::*;

        let pdfium = crate::test_pdfium();
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");
        // A thick diagonal across the middle of the 200x200 fixture page.
        let out = write_ink(&bytes, 1, &[vec![[20.0, 100.0], [180.0, 100.0]]])
            .expect("write")
            .expect("bytes");

        let doc = pdfium
            .get()
            .load_pdf_from_byte_vec(out, None)
            .expect("reload edited pdf");
        let page = doc.pages().get(0).expect("page 1");
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new().set_target_width(200),
            )
            .expect("render");

        let rgba = bitmap.as_rgba_bytes();
        let blue_pixels = rgba
            .chunks_exact(4)
            .filter(|p| {
                // #0B35B8-ish: clearly blue-dominant and not white paper.
                p[2] > 120 && p[2] as i16 - p[0] as i16 > 60 && p[2] as i16 - p[1] as i16 > 40
            })
            .count();

        assert!(
            blue_pixels > 50,
            "expected a visible blue stroke, found {blue_pixels} blue pixels"
        );
    }

    /// True for a pixel that is Tumbler's ink blue against white paper.
    fn is_ink(px: &[u8]) -> bool {
        px[2] > 120 && px[2] as i16 - px[0] as i16 > 60 && px[2] as i16 - px[1] as i16 > 40
    }

    /// A short horizontal stroke near the top-left of the page *as displayed*.
    /// Off-centre in both axes and not square, so a mapping that lands it in
    /// the wrong corner, or turns it on its side, cannot pass.
    const STROKE: [[f32; 2]; 2] = [[20.0, 30.0], [80.0, 30.0]];
    /// Where that stroke must come back out. The 1.5pt nib with round caps
    /// grows the mark by about a nib radius on every side.
    const STROKE_BOX: [f32; 4] = [20.0, 30.0, 80.0, 30.0];
    const NIB_TOL: f32 = 2.0;

    /// **The bug in issue #121.** The user draws against the page pdfium
    /// rendered; on a `/Rotate` page that render is turned, and at 90 and 270
    /// its width and height are swapped relative to user space. Ink written
    /// without accounting for that lands somewhere else entirely — or off the
    /// page, where nothing shows at all — while every content-stream assertion
    /// above still passes.
    ///
    /// So: the same stroke, in the same coordinates the frontend would send,
    /// on the same page at each of the four rotations. It must render back in
    /// the same place every time. The page is 200×400, deliberately not
    /// square: on a square page the 90/270 swap cancels and this test would
    /// pass against the broken code.
    #[test]
    fn ink_lands_where_it_was_drawn_on_a_rotated_page() {
        let pdfium = crate::test_pdfium();

        for rotate in [0, 90, 180, 270] {
            let page = crate::geometry_page_bytes(200.0, 400.0, rotate, None);
            let out = write_ink(&page, 1, &[STROKE.to_vec()])
                .expect("write")
                .expect("bytes");

            crate::assert_mark_landed(
                crate::rendered_mark_bbox(pdfium.get(), out, false, is_ink),
                STROKE_BOX,
                NIB_TOL,
                &format!("/Rotate {rotate}"),
            );
        }
    }

    /// Rotation and a crop together. pdfium renders the CropBox and *then*
    /// rotates, so the origin offset has to be applied in user space before
    /// the turn; applying it after puts the ink off by the crop in whichever
    /// direction the rotation points.
    #[test]
    fn ink_lands_correctly_on_a_page_that_is_both_cropped_and_rotated() {
        let pdfium = crate::test_pdfium();
        // A 200×400 sheet cropped to a 160×340 window with a non-zero origin.
        let crop = Some([15.0, 25.0, 175.0, 365.0]);

        for rotate in [0, 90, 180, 270] {
            let page = crate::geometry_page_bytes(200.0, 400.0, rotate, crop);
            let out = write_ink(&page, 1, &[STROKE.to_vec()])
                .expect("write")
                .expect("bytes");

            crate::assert_mark_landed(
                crate::rendered_mark_bbox(pdfium.get(), out, false, is_ink),
                STROKE_BOX,
                NIB_TOL,
                &format!("cropped, /Rotate {rotate}"),
            );
        }
    }

    #[test]
    fn missing_page_is_an_error() {
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");
        assert!(write_ink(&bytes, 99, &[vec![[1.0, 1.0]]]).is_err());
    }
}

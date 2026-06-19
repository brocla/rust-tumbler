//! PDF size-optimization pipeline.
//!
//! Each optimization is a small, individually-runnable transform on an
//! in-memory `lopdf::Document`. `run_optimization_steps` applies the chosen
//! steps in order, reporting the serialized size before and after each one, and
//! stages the result in `AppState` so the user can write it out via
//! `save_optimized_copy` ("Save As..."). Nothing touches the on-disk file until
//! that explicit save — important because the image step (added later) is lossy.
//!
//! This first cut covers the four lopdf-only steps (recompress streams, prune
//! unused objects, delete zero-length streams, strip non-essential extras).
//! Image downsampling/recompression (`RecompressImages`) lands in a later
//! commit and is a no-op here.

use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use lopdf::{Document, ObjectId};
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    RecompressStreams,
    PruneUnused,
    DeleteZeroLength,
    StripExtras,
    RecompressImages,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub step: StepId,
    pub size_before: u64,
    pub size_after: u64,
}

/// Tally of images the image step could not safely recompress, surfaced to the
/// UI as "N images skipped (reason…)". One entry per reason. Always empty until
/// the image step is implemented.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedImages {
    pub reason: String,
    pub count: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationReport {
    pub results: Vec<StepResult>,
    pub skipped_images: Vec<SkippedImages>,
}

// --- Step functions -------------------------------------------------------
//
// Each is a pure transform on an in-memory document, directly testable without
// pdfium or any external dependency.

/// Re-Flate content/stream objects. The cheapest, safest win.
fn step_recompress_streams(doc: &mut Document) {
    doc.compress();
}

/// Remove objects no longer reachable from the document root — orphans left
/// behind by editors.
fn step_prune_unused(doc: &mut Document) {
    doc.prune_objects();
}

/// Drop zero-length stream objects.
fn step_delete_zero_length(doc: &mut Document) {
    doc.delete_zero_length_streams();
}

/// Remove non-essential extras that bloat a file without affecting rendering:
/// the catalog's XMP `/Metadata` and `/OpenAction`, the `/JavaScript` and
/// `/EmbeddedFiles` name trees, and each page's `/Thumb` thumbnail. Named
/// destinations (`/Dests`) and the rest of the `/Names` tree are left intact.
fn step_strip_extras(doc: &mut Document) {
    if let Ok(catalog) = doc.catalog_mut() {
        catalog.remove(b"Metadata");
        catalog.remove(b"OpenAction");
    }

    // Surgically remove only the JavaScript / EmbeddedFiles entries from the
    // /Names tree, not the whole tree (which also holds named destinations).
    let names_id = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Names").ok().and_then(|o| o.as_reference().ok()));
    if let Some(id) = names_id {
        if let Ok(names) = doc.get_dictionary_mut(id) {
            names.remove(b"JavaScript");
            names.remove(b"EmbeddedFiles");
        }
    }

    let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
    for id in page_ids {
        if let Ok(page) = doc.get_dictionary_mut(id) {
            page.remove(b"Thumb");
        }
    }
}

/// Apply a single step. `target_dpi`/`jpeg_quality`/`skipped` are only consumed
/// by the image step (added later); the lopdf-only steps ignore them.
fn apply_step(
    doc: &mut Document,
    step: &StepId,
    _target_dpi: f32,
    _jpeg_quality: u8,
    _skipped: &mut Vec<SkippedImages>,
) {
    match step {
        StepId::RecompressStreams => step_recompress_streams(doc),
        StepId::PruneUnused => step_prune_unused(doc),
        StepId::DeleteZeroLength => step_delete_zero_length(doc),
        StepId::StripExtras => step_strip_extras(doc),
        // Image recompression is implemented in a later commit (Phase 5 step 5).
        StepId::RecompressImages => {}
    }
}

/// Serialized byte length of the document. Measures on a throwaway clone so the
/// working document is never mutated by the act of measuring (saving a
/// stream-xref PDF can append an xref-stream object).
fn serialized_size(doc: &Document) -> u64 {
    let mut buf = Vec::new();
    let mut clone = doc.clone();
    // A serialization failure here only means the size is unknown; report 0
    // rather than aborting the whole optimization.
    if clone.save_to(&mut buf).is_err() {
        return 0;
    }
    buf.len() as u64
}

// --- Commands -------------------------------------------------------------

#[tauri::command]
pub fn run_optimization_steps(
    state: State<'_, AppState>,
    doc_id: String,
    steps: Vec<StepId>,
    target_dpi: f32,
    jpeg_quality: u8,
) -> Result<OptimizationReport, String> {
    run_optimization_steps_impl(&state, doc_id, steps, target_dpi, jpeg_quality).map_err(String::from)
}

fn run_optimization_steps_impl(
    state: &AppState,
    doc_id: String,
    steps: Vec<StepId>,
    target_dpi: f32,
    jpeg_quality: u8,
) -> Result<OptimizationReport, AppError> {
    // The file on disk is the source of truth (in-place edits like page ops and
    // metadata already write through to it); load from there rather than the
    // pdfium handle.
    let file_path = {
        let entry = state.get_document(&doc_id)?;
        let entry = lock_mutex(&entry)?;
        entry.file_path.clone()
    };

    let pdf_bytes =
        std::fs::read(&file_path).map_err(|e| AppError::io("Failed to read PDF for optimization", e))?;
    let mut doc = Document::load_mem(&pdf_bytes)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for optimization", e))?;

    let mut results = Vec::with_capacity(steps.len());
    let mut skipped_images = Vec::new();

    for step in &steps {
        let size_before = serialized_size(&doc);
        apply_step(&mut doc, step, target_dpi, jpeg_quality, &mut skipped_images);
        let size_after = serialized_size(&doc);
        results.push(StepResult {
            step: step.clone(),
            size_before,
            size_after,
        });
    }

    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize optimized PDF", e))?;
    state.set_pending_optimized(doc_id, out);

    Ok(OptimizationReport {
        results,
        skipped_images,
    })
}

#[tauri::command]
pub fn save_optimized_copy(
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
) -> Result<(), String> {
    save_optimized_copy_impl(&state, doc_id, dest_path).map_err(String::from)
}

fn save_optimized_copy_impl(
    state: &AppState,
    doc_id: String,
    dest_path: String,
) -> Result<(), AppError> {
    let bytes = state.get_pending_optimized(&doc_id).ok_or_else(|| {
        AppError::Other("No optimized output to save — run optimization first.".to_string())
    })?;

    // Write to a temp file in the destination directory, then atomically
    // replace, so an interrupted write can't truncate an existing file at
    // `dest_path`. Mirrors the save pattern in `metadata.rs`.
    let tmp_path = format!("{dest_path}.tmp-{}", uuid::Uuid::new_v4());
    if let Err(e) = std::fs::write(&tmp_path, &bytes) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(AppError::io("Failed to write optimized PDF", e));
    }
    std::fs::rename(&tmp_path, &dest_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::io("Failed to save optimized PDF", e)
    })?;

    state.clear_pending_optimized(&doc_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;
    use lopdf::{Dictionary, Object};

    fn load_fixture() -> Document {
        let bytes = std::fs::read(crate::fixture_path()).expect("read fixture");
        Document::load_mem(&bytes).expect("parse fixture")
    }

    /// Recompressing streams should never meaningfully grow the file (it may be
    /// a no-op if already compressed). A small slack absorbs xref/rounding
    /// overhead.
    #[test]
    fn recompress_streams_does_not_grow_size() {
        let mut doc = load_fixture();
        let before = serialized_size(&doc);
        step_recompress_streams(&mut doc);
        let after = serialized_size(&doc);
        assert!(after <= before + 100, "after={after} before={before}");
    }

    /// All four lopdf-only steps in sequence must leave a document that still
    /// parses as valid PDF.
    #[test]
    fn lopdf_steps_keep_document_loadable() {
        let mut doc = load_fixture();
        let mut skipped = Vec::new();
        for step in [
            StepId::RecompressStreams,
            StepId::PruneUnused,
            StepId::DeleteZeroLength,
            StepId::StripExtras,
        ] {
            apply_step(&mut doc, &step, 150.0, 80, &mut skipped);
        }
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize");
        Document::load_mem(&out).expect("optimized output should be valid PDF");
        assert!(skipped.is_empty());
    }

    /// `step_strip_extras` must remove the catalog's `/Metadata` and
    /// `/OpenAction`, drop `/JavaScript` and `/EmbeddedFiles` from the `/Names`
    /// tree while leaving other name entries (e.g. `/Dests`) intact, and remove
    /// each page's `/Thumb`.
    #[test]
    fn strip_extras_removes_only_targeted_keys() {
        let mut doc = load_fixture();

        // Inject the extras the step is meant to strip.
        let meta_id = doc.add_object(Object::Dictionary(Dictionary::new()));
        let mut names = Dictionary::new();
        names.set("JavaScript", Object::Null);
        names.set("EmbeddedFiles", Object::Null);
        names.set("Dests", Object::Null); // must survive
        let names_id = doc.add_object(Object::Dictionary(names));

        let catalog_id = doc
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .expect("catalog ref");
        {
            let catalog = doc.get_dictionary_mut(catalog_id).expect("catalog");
            catalog.set("Metadata", Object::Reference(meta_id));
            catalog.set("OpenAction", Object::Null);
            catalog.set("Names", Object::Reference(names_id));
        }
        let page_id = *doc.get_pages().values().next().expect("at least one page");
        doc.get_dictionary_mut(page_id)
            .expect("page")
            .set("Thumb", Object::Null);

        step_strip_extras(&mut doc);

        let catalog = doc.catalog().expect("catalog");
        assert!(catalog.get(b"Metadata").is_err(), "Metadata should be removed");
        assert!(catalog.get(b"OpenAction").is_err(), "OpenAction should be removed");

        let names = doc.get_dictionary(names_id).expect("names dict");
        assert!(names.get(b"JavaScript").is_err(), "JavaScript should be removed");
        assert!(names.get(b"EmbeddedFiles").is_err(), "EmbeddedFiles should be removed");
        assert!(names.get(b"Dests").is_ok(), "Dests should survive");

        let page = doc.get_dictionary(page_id).expect("page");
        assert!(page.get(b"Thumb").is_err(), "Thumb should be removed");
    }

    /// The pipeline records one result per requested step, stages a valid
    /// output for Save As, and clears the staged bytes once saved (so a second
    /// save with nothing pending errors).
    #[test]
    fn pipeline_records_steps_stages_and_saves_output() {
        let src = crate::fixture_path();
        let tmp = std::env::temp_dir().join("tumbler_optimize_in.pdf");
        std::fs::copy(&src, &tmp).expect("copy fixture");
        let file_path = tmp.to_string_lossy().into_owned();

        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let document = pdfium
            .load_pdf_from_file(&file_path, None)
            .expect("load pdf");
        state
            .insert_document(
                "doc-1".to_string(),
                DocEntry {
                    document,
                    file_path: file_path.clone(),
                },
            )
            .expect("insert");

        let steps = vec![
            StepId::RecompressStreams,
            StepId::PruneUnused,
            StepId::DeleteZeroLength,
            StepId::StripExtras,
        ];
        let report = run_optimization_steps_impl(&state, "doc-1".to_string(), steps, 150.0, 80)
            .expect("run optimization");

        assert_eq!(report.results.len(), 4);
        assert!(report.skipped_images.is_empty());
        assert_eq!(report.results[0].step, StepId::RecompressStreams);

        let dest = std::env::temp_dir().join("tumbler_optimize_out.pdf");
        let dest_path = dest.to_string_lossy().into_owned();
        save_optimized_copy_impl(&state, "doc-1".to_string(), dest_path.clone())
            .expect("save optimized copy");

        let out_bytes = std::fs::read(&dest).expect("read saved output");
        Document::load_mem(&out_bytes).expect("saved output should be valid PDF");

        // Pending bytes were cleared by the successful save.
        assert!(
            save_optimized_copy_impl(&state, "doc-1".to_string(), dest_path).is_err(),
            "second save should fail with nothing pending"
        );

        std::fs::remove_file(&tmp).ok();
        std::fs::remove_file(&dest).ok();
    }

    /// Saving with no prior optimization run is an error, not a panic.
    #[test]
    fn save_without_pending_output_errors() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let dest = std::env::temp_dir()
            .join("tumbler_optimize_none.pdf")
            .to_string_lossy()
            .into_owned();
        assert!(save_optimized_copy_impl(&state, "missing".to_string(), dest).is_err());
    }
}

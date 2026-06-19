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
use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
#[derive(Serialize, Debug)]
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

// --- Step 5: image downsampling + recompression --------------------------
//
// Decode each Image XObject we can handle (DCTDecode JPEGs and 8-bpc
// DeviceRGB/DeviceGray FlateDecode rasters), figure out its displayed size on
// the page from the CTM at its `Do` operator, and if it's being shown at more
// than `target_dpi`, resize it down and re-encode as a baseline JPEG. Images we
// can't safely touch are left byte-for-byte and tallied into `skipped`.

/// 2x2 linear part of a PDF transformation matrix. Translation doesn't affect
/// an image's displayed size, so it's omitted. Row-vector convention: a point
/// (x, y) maps to (x*a + y*c, x*b + y*d).
#[derive(Clone, Copy)]
struct Mat2 {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
}

impl Mat2 {
    const IDENTITY: Mat2 = Mat2 { a: 1.0, b: 0.0, c: 0.0, d: 1.0 };

    /// `cm` concatenation: the new matrix (`self`) applies first, then the old
    /// CTM — i.e. CTM_new = self × ctm. Only the 2x2 part is tracked.
    fn concat(self, ctm: Mat2) -> Mat2 {
        Mat2 {
            a: self.a * ctm.a + self.b * ctm.c,
            b: self.a * ctm.b + self.b * ctm.d,
            c: self.c * ctm.a + self.d * ctm.c,
            d: self.c * ctm.b + self.d * ctm.d,
        }
    }

    /// Displayed width in points: the image's unit-square x-basis (1,0) maps to
    /// (a, b).
    fn displayed_width(self) -> f32 {
        (self.a * self.a + self.b * self.b).sqrt()
    }
}

fn is_image_xobject(obj: &Object) -> bool {
    obj.as_stream()
        .ok()
        .and_then(|s| s.dict.get(b"Subtype").ok())
        .and_then(|o| o.as_name_str().ok())
        == Some("Image")
}

/// Add every image-XObject name → id entry from one resources dictionary,
/// restricted to ids already known to be images (so Form XObjects are excluded).
fn collect_image_names(
    resources: &Dictionary,
    doc: &Document,
    image_ids: &HashSet<ObjectId>,
    out: &mut HashMap<Vec<u8>, ObjectId>,
) {
    let xobjects = match resources.get(b"XObject") {
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok(),
        Ok(Object::Dictionary(d)) => Some(d),
        _ => None,
    };
    if let Some(xobjects) = xobjects {
        for (name, value) in xobjects.iter() {
            if let Ok(id) = value.as_reference() {
                if image_ids.contains(&id) {
                    out.insert(name.clone(), id);
                }
            }
        }
    }
}

/// Largest displayed width (points) of each image across all pages, from the
/// CTM in effect at each `Do`. Images never drawn directly on a page's content
/// stream are absent — the caller treats those as "unreferenced" and skips them.
fn measure_displayed_widths(
    doc: &Document,
    image_ids: &HashSet<ObjectId>,
) -> HashMap<ObjectId, f32> {
    let mut widths: HashMap<ObjectId, f32> = HashMap::new();

    for page_id in doc.get_pages().into_values() {
        let mut names: HashMap<Vec<u8>, ObjectId> = HashMap::new();
        if let Ok((inline, referenced)) = doc.get_page_resources(page_id) {
            if let Some(res) = inline {
                collect_image_names(res, doc, image_ids, &mut names);
            }
            for res_id in referenced {
                if let Ok(res) = doc.get_dictionary(res_id) {
                    collect_image_names(res, doc, image_ids, &mut names);
                }
            }
        }
        if names.is_empty() {
            continue;
        }

        let content = match doc.get_and_decode_page_content(page_id) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut ctm = Mat2::IDENTITY;
        let mut stack: Vec<Mat2> = Vec::new();
        for op in &content.operations {
            match op.operator.as_str() {
                "q" => stack.push(ctm),
                "Q" => {
                    if let Some(prev) = stack.pop() {
                        ctm = prev;
                    }
                }
                "cm" => {
                    let v: Vec<f32> = op.operands.iter().filter_map(|o| o.as_float().ok()).collect();
                    if v.len() == 6 {
                        let cm = Mat2 { a: v[0], b: v[1], c: v[2], d: v[3] };
                        ctm = cm.concat(ctm);
                    }
                }
                "Do" => {
                    if let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) {
                        if let Some(&id) = names.get(name) {
                            let w = ctm.displayed_width();
                            if w > 0.0 {
                                let entry = widths.entry(id).or_insert(0.0);
                                *entry = entry.max(w);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    widths
}

#[derive(Clone, Copy, PartialEq)]
enum ColorKind {
    Gray,
    Rgb,
    Other,
}

/// Resolve an image's color space to one we can rebuild raw samples for. Only
/// the device gray/RGB families qualify; everything else (CMYK, ICCBased,
/// Indexed, Separation…) is `Other`.
fn color_kind(dict: &Dictionary, doc: &Document) -> ColorKind {
    let cs = match dict.get(b"ColorSpace") {
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(o) => o,
            Err(_) => return ColorKind::Other,
        },
        Ok(o) => o,
        Err(_) => return ColorKind::Other,
    };
    match cs.as_name().ok() {
        Some(b"DeviceGray") | Some(b"CalGray") | Some(b"G") => ColorKind::Gray,
        Some(b"DeviceRGB") | Some(b"CalRGB") | Some(b"RGB") => ColorKind::Rgb,
        _ => ColorKind::Other,
    }
}

fn is_indexed(dict: &Dictionary, doc: &Document) -> bool {
    let cs = match dict.get(b"ColorSpace") {
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(o) => o,
            Err(_) => return false,
        },
        Ok(o) => o,
        Err(_) => return false,
    };
    match cs {
        Object::Array(arr) => {
            matches!(arr.first().and_then(|o| o.as_name_str().ok()), Some("Indexed") | Some("I"))
        }
        Object::Name(n) => n == b"Indexed" || n == b"I",
        _ => false,
    }
}

/// True if the image's DecodeParms request a PNG/TIFF predictor. We don't
/// reverse predictors in this first cut, so such FlateDecode images are skipped
/// rather than decoded into garbage.
fn has_predictor(dict: &Dictionary, doc: &Document) -> bool {
    fn pred_in(d: &Dictionary) -> bool {
        d.get(b"Predictor").and_then(|o| o.as_float()).map(|p| p > 1.0).unwrap_or(false)
    }
    let dp = match dict.get(b"DecodeParms").or_else(|_| dict.get(b"DP")) {
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(o) => o,
            Err(_) => return false,
        },
        Ok(o) => o,
        Err(_) => return false,
    };
    match dp {
        Object::Dictionary(d) => pred_in(d),
        Object::Array(arr) => arr.iter().any(|o| match o {
            Object::Dictionary(d) => pred_in(d),
            Object::Reference(id) => {
                doc.get_object(*id).and_then(Object::as_dict).map(pred_in).unwrap_or(false)
            }
            _ => false,
        }),
        _ => false,
    }
}

/// zlib-inflate (with a raw-deflate fallback) for a FlateDecode image stream.
/// `decompressed_content()` refuses Image XObjects, so we do it here.
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).is_ok() && !out.is_empty() {
        return Some(out);
    }
    out.clear();
    if flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).is_ok() && !out.is_empty() {
        return Some(out);
    }
    None
}

enum PlanResult {
    /// Replace the stream with a smaller JPEG of the given dimensions.
    Replace { content: Vec<u8>, w: u32, h: u32, gray: bool },
    /// Leave the image untouched and don't report it (already small enough, or
    /// the re-encode wasn't actually smaller).
    Leave,
    /// Leave the image untouched and tally it under this reason.
    Skip(&'static str),
}

/// Decide what to do with one image, holding only shared borrows of `doc`.
fn plan_one(
    doc: &Document,
    id: ObjectId,
    displayed_w: Option<f32>,
    target_dpi: f32,
    quality: u8,
) -> PlanResult {
    let stream = match doc.get_object(id).and_then(|o| o.as_stream()) {
        Ok(s) => s,
        Err(_) => return PlanResult::Skip("decode"),
    };
    let dict = &stream.dict;

    let width = dict.get(b"Width").and_then(|o| o.as_float()).map(|f| f as u32).unwrap_or(0);
    let height = dict.get(b"Height").and_then(|o| o.as_float()).map(|f| f as u32).unwrap_or(0);
    if width == 0 || height == 0 {
        return PlanResult::Skip("decode");
    }
    let bpc = dict.get(b"BitsPerComponent").and_then(|o| o.as_float()).map(|f| f as i64).unwrap_or(8);
    if bpc < 8 {
        return PlanResult::Skip("bilevel");
    }
    if is_indexed(dict, doc) {
        return PlanResult::Skip("indexed");
    }

    // Where is it drawn? Without a draw site we can't judge its DPI.
    let displayed_w = match displayed_w {
        Some(w) if w > 0.0 => w,
        _ => return PlanResult::Skip("unreferenced"),
    };
    let current_dpi = width as f32 * 72.0 / displayed_w;
    if current_dpi <= target_dpi {
        return PlanResult::Leave;
    }
    let scale = target_dpi / current_dpi;
    let new_w = ((width as f32 * scale).round() as u32).max(1);
    let new_h = ((height as f32 * scale).round() as u32).max(1);

    let filters = stream.filters().unwrap_or_default();
    let filter = if filters.len() == 1 { filters[0].as_str() } else { "" };

    let (img, gray) = match filter {
        "DCTDecode" => match image::load_from_memory(&stream.content) {
            Ok(d) => {
                let gray = matches!(d.color(), image::ColorType::L8 | image::ColorType::L16);
                (d, gray)
            }
            Err(_) => return PlanResult::Skip("decode"),
        },
        "FlateDecode" => {
            let gray = match color_kind(dict, doc) {
                ColorKind::Gray => true,
                ColorKind::Rgb => false,
                ColorKind::Other => return PlanResult::Skip("colorspace"),
            };
            if bpc != 8 {
                return PlanResult::Skip("colorspace");
            }
            if has_predictor(dict, doc) {
                return PlanResult::Skip("predictor");
            }
            let raw = match inflate(&stream.content) {
                Some(r) => r,
                None => return PlanResult::Skip("decode"),
            };
            let channels = if gray { 1 } else { 3 };
            let expected = width as usize * height as usize * channels;
            if raw.len() < expected {
                return PlanResult::Skip("decode");
            }
            let built = if gray {
                image::GrayImage::from_raw(width, height, raw[..expected].to_vec())
                    .map(image::DynamicImage::ImageLuma8)
            } else {
                image::RgbImage::from_raw(width, height, raw[..expected].to_vec())
                    .map(image::DynamicImage::ImageRgb8)
            };
            match built {
                Some(d) => (d, gray),
                None => return PlanResult::Skip("decode"),
            }
        }
        "CCITTFaxDecode" => return PlanResult::Skip("ccitt"),
        "JPXDecode" => return PlanResult::Skip("jpx"),
        "JBIG2Decode" => return PlanResult::Skip("jbig2"),
        "Crypt" => return PlanResult::Skip("crypt"),
        _ => return PlanResult::Skip("unsupported_filter"),
    };

    let resized = img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    let encoded = {
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        if gray {
            enc.encode_image(&resized.to_luma8()).is_ok()
        } else {
            enc.encode_image(&resized.to_rgb8()).is_ok()
        }
    };
    if !encoded {
        return PlanResult::Skip("decode");
    }
    // Don't adopt a re-encode that didn't actually shrink the stream.
    if out.len() >= stream.content.len() {
        return PlanResult::Leave;
    }
    PlanResult::Replace { content: out, w: new_w, h: new_h, gray }
}

fn step_recompress_images(
    doc: &mut Document,
    target_dpi: f32,
    jpeg_quality: u8,
    skipped: &mut Vec<SkippedImages>,
) {
    let image_ids: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter_map(|(id, obj)| is_image_xobject(obj).then_some(*id))
        .collect();
    if image_ids.is_empty() {
        return;
    }
    let id_set: HashSet<ObjectId> = image_ids.iter().copied().collect();
    let widths = measure_displayed_widths(doc, &id_set);

    // Plan with shared borrows, then write with a single mutable pass — an image
    // can't be re-encoded while we're iterating the object map.
    let mut plans: Vec<(ObjectId, Vec<u8>, u32, u32, bool)> = Vec::new();
    let mut counts: HashMap<&'static str, u32> = HashMap::new();
    for id in &image_ids {
        match plan_one(doc, *id, widths.get(id).copied(), target_dpi, jpeg_quality) {
            PlanResult::Replace { content, w, h, gray } => plans.push((*id, content, w, h, gray)),
            PlanResult::Skip(reason) => *counts.entry(reason).or_insert(0) += 1,
            PlanResult::Leave => {}
        }
    }

    for (id, content, w, h, gray) in plans {
        if let Ok(stream) = doc.get_object_mut(id).and_then(|o| o.as_stream_mut()) {
            stream.set_content(content);
            stream.dict.set("Filter", "DCTDecode");
            stream.dict.remove(b"DecodeParms");
            stream.dict.remove(b"DP");
            stream.dict.set("Width", w as i64);
            stream.dict.set("Height", h as i64);
            stream.dict.set("BitsPerComponent", 8_i64);
            stream.dict.set("ColorSpace", if gray { "DeviceGray" } else { "DeviceRGB" });
        }
    }

    for (reason, count) in counts {
        skipped.push(SkippedImages { reason: reason.to_string(), count });
    }
}

/// Apply a single step. `target_dpi`/`jpeg_quality`/`skipped` are only consumed
/// by the image step; the lopdf-only steps ignore them.
fn apply_step(
    doc: &mut Document,
    step: &StepId,
    target_dpi: f32,
    jpeg_quality: u8,
    skipped: &mut Vec<SkippedImages>,
) {
    match step {
        StepId::RecompressStreams => step_recompress_streams(doc),
        StepId::PruneUnused => step_prune_unused(doc),
        StepId::DeleteZeroLength => step_delete_zero_length(doc),
        StepId::StripExtras => step_strip_extras(doc),
        StepId::RecompressImages => step_recompress_images(doc, target_dpi, jpeg_quality, skipped),
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
    use lopdf::{Dictionary, Object, Stream};

    /// A decodable JPEG of the given size, for building image fixtures.
    fn jpeg_bytes(w: u32, h: u32, quality: u8) -> Vec<u8> {
        let mut buf = image::RgbImage::new(w, h);
        for (x, y, p) in buf.enumerate_pixels_mut() {
            *p = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&buf)
            .unwrap();
        out
    }

    /// Build a one-page document with a single image XObject. `media` is the
    /// square page size in points; `cm` (if `Some`) draws the image filling that
    /// page via the given matrix. Returns `(doc, image_object_id)`.
    #[allow(clippy::too_many_arguments)]
    fn doc_with_image(
        filter: &str,
        bpc: i64,
        color: &str,
        width: i64,
        height: i64,
        content: Vec<u8>,
        media: f32,
        cm: Option<[f32; 6]>,
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.5");

        let mut img_dict = Dictionary::new();
        img_dict.set("Type", "XObject");
        img_dict.set("Subtype", "Image");
        img_dict.set("Width", width);
        img_dict.set("Height", height);
        img_dict.set("ColorSpace", color);
        img_dict.set("BitsPerComponent", bpc);
        img_dict.set("Filter", filter);
        let img_id = doc.add_object(Stream::new(img_dict, content));

        let page_content = match cm {
            Some([a, b, c, d, e, f]) => format!("q {a} {b} {c} {d} {e} {f} cm /Im0 Do Q"),
            None => "q Q".to_string(),
        };
        let content_id = doc.add_object(Stream::new(Dictionary::new(), page_content.into_bytes()));

        let mut xobject = Dictionary::new();
        xobject.set("Im0", Object::Reference(img_id));
        let mut resources = Dictionary::new();
        resources.set("XObject", Object::Dictionary(xobject));

        let mut page = Dictionary::new();
        page.set("Type", "Page");
        page.set(
            "MediaBox",
            Object::Array(vec![0.into(), 0.into(), Object::Real(media), Object::Real(media)]),
        );
        page.set("Resources", Object::Dictionary(resources));
        page.set("Contents", Object::Reference(content_id));
        let page_id = doc.add_object(page);

        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
        pages.set("Count", 1_i64);
        let pages_id = doc.add_object(pages);

        doc.get_dictionary_mut(page_id)
            .unwrap()
            .set("Parent", Object::Reference(pages_id));

        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", Object::Reference(catalog_id));

        (doc, img_id)
    }

    fn image_dict<'a>(doc: &'a Document, id: ObjectId) -> &'a Dictionary {
        &doc.get_object(id).unwrap().as_stream().unwrap().dict
    }

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

    /// A 600px image shown across 1 inch (600 DPI) recompresses smaller when
    /// targeting 150 DPI, the stored Width drops to 150, and nothing is skipped.
    #[test]
    fn recompress_images_downsamples_high_dpi_jpeg() {
        let jpeg = jpeg_bytes(600, 600, 90);
        let (mut doc, img_id) = doc_with_image(
            "DCTDecode",
            8,
            "DeviceRGB",
            600,
            600,
            jpeg,
            72.0,
            Some([72.0, 0.0, 0.0, 72.0, 0.0, 0.0]),
        );

        let before = serialized_size(&doc);
        let mut skipped = Vec::new();
        step_recompress_images(&mut doc, 150.0, 75, &mut skipped);
        let after = serialized_size(&doc);

        assert!(after < before, "expected shrink: after={after} before={before}");
        assert!(skipped.is_empty(), "unexpected skips: {skipped:?}");

        let w = image_dict(&doc, img_id).get(b"Width").unwrap().as_float().unwrap();
        assert_eq!(w as u32, 150);
    }

    /// An image already shown below the target DPI is left byte-for-byte and not
    /// reported as skipped.
    #[test]
    fn recompress_images_leaves_low_dpi_image_untouched() {
        let jpeg = jpeg_bytes(100, 100, 90);
        let (mut doc, img_id) = doc_with_image(
            "DCTDecode",
            8,
            "DeviceRGB",
            100,
            100,
            jpeg,
            72.0,
            Some([72.0, 0.0, 0.0, 72.0, 0.0, 0.0]),
        );

        let before = serialized_size(&doc);
        let mut skipped = Vec::new();
        step_recompress_images(&mut doc, 150.0, 75, &mut skipped);
        let after = serialized_size(&doc);

        assert!(skipped.is_empty());
        assert_eq!(after, before);
        let w = image_dict(&doc, img_id).get(b"Width").unwrap().as_float().unwrap();
        assert_eq!(w as u32, 100);
    }

    /// A filter the `image` crate can't decode (JPEG2000) is left untouched and
    /// tallied under its reason.
    #[test]
    fn recompress_images_skips_and_reports_jpx() {
        let (mut doc, img_id) = doc_with_image(
            "JPXDecode",
            8,
            "DeviceRGB",
            600,
            600,
            vec![0u8; 64],
            72.0,
            Some([72.0, 0.0, 0.0, 72.0, 0.0, 0.0]),
        );

        let mut skipped = Vec::new();
        step_recompress_images(&mut doc, 150.0, 75, &mut skipped);

        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, "jpx");
        assert_eq!(skipped[0].count, 1);
        // Untouched: the filter is still JPXDecode.
        let f = image_dict(&doc, img_id).get(b"Filter").unwrap().as_name_str().unwrap();
        assert_eq!(f, "JPXDecode");
    }

    /// A 1-bit (bilevel) image is skipped — re-encoding line art as JPEG would
    /// make it larger and blurrier.
    #[test]
    fn recompress_images_skips_bilevel() {
        let (mut doc, _img) = doc_with_image(
            "FlateDecode",
            1,
            "DeviceGray",
            600,
            600,
            vec![0u8; 32],
            72.0,
            Some([72.0, 0.0, 0.0, 72.0, 0.0, 0.0]),
        );

        let mut skipped = Vec::new();
        step_recompress_images(&mut doc, 150.0, 75, &mut skipped);

        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, "bilevel");
    }

    /// An image XObject that's never drawn on a page has no displayed size, so
    /// its DPI is unknown — skip and report rather than guess.
    #[test]
    fn recompress_images_skips_unreferenced_image() {
        let jpeg = jpeg_bytes(600, 600, 90);
        let (mut doc, _img) =
            doc_with_image("DCTDecode", 8, "DeviceRGB", 600, 600, jpeg, 72.0, None);

        let mut skipped = Vec::new();
        step_recompress_images(&mut doc, 150.0, 75, &mut skipped);

        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, "unreferenced");
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

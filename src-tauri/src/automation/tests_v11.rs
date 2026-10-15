//! Tests for the v1.1 tools: forms, typewriter, ink and redaction. Each tool
//! that takes rectangles is checked on a rotated, non-square page by rendering
//! it and finding where the marks landed (CLAUDE.md, "Page-authoring tools and
//! rotated pages").

use super::paths::tests::TempDir;
use super::tests::*;
use super::*;
use crate::commands::linearize::Linearizer;
use crate::commands::ocr::{OcrEngine, OcrWord};
use crate::commands::text::TextRect;
use crate::error::AppError;
use crate::state::AppState;
use pdfium_render::prelude::Pdfium;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn forms_fixture(dir: &TempDir) -> PathBuf {
    let p = dir.0.join("form.pdf");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forms/acroform_basic.pdf"), &p).unwrap();
    p
}

/// A /Rotate 90 page, 300 wide by 200 high in user space, so displayed 200 by
/// 300: non-square, so a mixed-up width and height cannot cancel out.
fn rotated(dir: &TempDir, name: &str) -> PathBuf {
    dir.file(name, &crate::geometry_page_bytes(300.0, 200.0, 90, None))
}

fn buffer(auto: &Automation, doc: u64) -> Vec<u8> {
    buffer_and_dirty(auto, doc).0
}

/// An OCR engine that recognizes nothing, the honest answer for blank or
/// burned pages.
pub(super) struct NoWords;
impl OcrEngine for NoWords {
    fn recognize(&self, _: &[u8], _: u32, _: u32) -> Result<Vec<OcrWord>, AppError> {
        Ok(Vec::new())
    }
}

/// "Reads" a word inside every small crop: the burned-region crops that
/// redaction's verification OCRs, so verification must fail.
pub(super) struct SeesThroughBlackBoxes;
impl OcrEngine for SeesThroughBlackBoxes {
    fn recognize(&self, _: &[u8], w: u32, h: u32) -> Result<Vec<OcrWord>, AppError> {
        if w >= 500 {
            return Ok(Vec::new());
        }
        let rect = TextRect { x: 0.0, y: 0.0, width: w as f32, height: h as f32 };
        Ok(vec![OcrWord { text: "Fixture".into(), rect }])
    }
}

struct CopyLinearizer;
impl Linearizer for CopyLinearizer {
    fn linearize(&self, src: &Path, dest: &Path) -> Result<(), AppError> {
        std::fs::copy(src, dest).map(|_| ()).map_err(|e| AppError::io("copy", e))
    }
}

pub(super) fn session_with(pdfium: &'static Pdfium, engine: Arc<dyn OcrEngine>, root: &TempDir) -> Automation {
    let state = AppState::new(pdfium, None).with_ocr_engine(engine).with_linearizer(Arc::new(CopyLinearizer));
    Automation::new(state).with_root(root.path()).unwrap()
}

fn field<'a>(fields: &'a Value, id: &str) -> &'a Value {
    fields.as_array().unwrap().iter().find(|f| f["id"] == id).unwrap_or_else(|| panic!("no field {id}: {fields}"))
}

// ── Forms ────────────────────────────────────────────────────────────────────

#[test]
fn form_fields_lists_every_field() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &forms_fixture(&dir));
    let fields = ok(&mut auto, "form_fields", json!({ "doc": doc }))["fields"].clone();
    assert_eq!(field(&fields, "fullName")["field_type"], "text");
    assert_eq!(field(&fields, "subscribe")["export_value"], "Yes");
    assert_eq!(field(&fields, "subscribe")["page"], 1);

    let plain = open(&mut auto, &fixture_copy(&dir, "plain.pdf"));
    assert_eq!(ok(&mut auto, "form_fields", json!({ "doc": plain }))["fields"], json!([]));
}

#[test]
fn form_fill_sets_values_by_name() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &forms_fixture(&dir));
    let r = ok(
        &mut auto,
        "form_fill",
        json!({ "doc": doc, "values": { "fullName": "Ada Lovelace", "subscribe": "Yes", "color": "Red" } }),
    );
    assert_eq!((r["dirty"].clone(), r["filled"].clone()), (json!(true), json!(3)));
    let fields = ok(&mut auto, "form_fields", json!({ "doc": doc }))["fields"].clone();
    assert_eq!(field(&fields, "fullName")["value"], "Ada Lovelace");
    assert_eq!(field(&fields, "subscribe")["value"], "Yes");
}

/// Every name and value is checked before anything is written, so one typo
/// writes nothing.
#[test]
fn form_fill_writes_nothing_if_any_name_or_choice_is_wrong() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &forms_fixture(&dir));
    let before = buffer(&auto, doc);
    let m = failed_with(&mut auto, "form_fill", json!({ "doc": doc, "values": { "fullName": "Ada", "fullNmae": "x" } }));
    assert!(m.contains("fullNmae"), "{m}");
    let m = failed_with(&mut auto, "form_fill", json!({ "doc": doc, "values": { "fullName": "Ada", "subscribe": "Maybe" } }));
    assert!(m.contains("subscribe") && m.contains("Yes"), "{m}");
    assert_eq!(buffer(&auto, doc), before);
    assert!(!buffer_and_dirty(&auto, doc).1);
}

#[test]
fn form_reset_clears_all_or_named_fields() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &forms_fixture(&dir));
    ok(&mut auto, "form_fill", json!({ "doc": doc, "values": { "fullName": "Ada", "comments": "Hi" } }));
    ok(&mut auto, "form_reset", json!({ "doc": doc, "fields": ["comments"] }));
    let fields = ok(&mut auto, "form_fields", json!({ "doc": doc }))["fields"].clone();
    assert_eq!((field(&fields, "fullName")["value"].clone(), field(&fields, "comments")["value"].clone()), (json!("Ada"), json!("")));
    ok(&mut auto, "form_reset", json!({ "doc": doc }));
    let fields = ok(&mut auto, "form_fields", json!({ "doc": doc }))["fields"].clone();
    assert_eq!(field(&fields, "fullName")["value"], "");
    failed_with(&mut auto, "form_reset", json!({ "doc": doc, "fields": ["nope"] }));
}

// ── Typewriter ───────────────────────────────────────────────────────────────

fn note(x: f32, y: f32, text: &str) -> Value {
    json!({ "x": x, "y": y, "width": 120.0, "height": 30.0, "text": text })
}

#[test]
fn typewriter_set_replaces_one_pages_notes() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let two = dir.file("two.pdf", &super::tests_edit::pages_pdf(&[(300.0, 400.0), (300.0, 400.0)]));
    let doc = open(&mut auto, &two);
    let r = ok(&mut auto, "typewriter_set", json!({ "doc": doc, "page": 2, "notes": [note(20.0, 20.0, "keep")] }));
    assert_eq!(r["dirty"], true);
    ok(&mut auto, "typewriter_set", json!({ "doc": doc, "page": 1, "notes": [note(20.0, 20.0, "one"), note(20.0, 80.0, "two")] }));

    let all = ok(&mut auto, "typewriter_list", json!({ "doc": doc }))["notes"].clone();
    let texts: Vec<(u64, String)> =
        all.as_array().unwrap().iter().map(|n| (n["page"].as_u64().unwrap(), n["text"].as_str().unwrap().to_string())).collect();
    assert!(texts.contains(&(1, "one".into())) && texts.contains(&(1, "two".into())) && texts.contains(&(2, "keep".into())), "{all}");
    assert_eq!(all[0]["font_family"], "Helvetica", "snake_case keys, default font: {all}");

    ok(&mut auto, "typewriter_set", json!({ "doc": doc, "page": 1, "notes": [] }));
    let page1 = ok(&mut auto, "typewriter_list", json!({ "doc": doc, "page": 1 }))["notes"].clone();
    assert_eq!(page1, json!([]));
    assert_eq!(ok(&mut auto, "typewriter_list", json!({ "doc": doc, "page": 2 }))["notes"].as_array().unwrap().len(), 1);
}

/// The note's text renders inside the box it was given, on a rotated page.
#[test]
fn typewriter_note_lands_where_placed_on_a_rotated_page() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &rotated(&dir, "rot.pdf"));
    let (x, y, w, h) = (30.0, 60.0, 140.0, 40.0);
    let n = json!({ "x": x, "y": y, "width": w, "height": h, "text": "MMMM", "font_size": 24 });
    ok(&mut auto, "typewriter_set", json!({ "doc": doc, "page": 1, "notes": [n] }));
    let got = crate::rendered_mark_bbox(pdfium.get(), buffer(&auto, doc), true, |px| px[0] < 128 && px[1] < 128 && px[2] < 128)
        .expect("the note's text was drawn");
    let inside = got[0] >= x - 2.0 && got[1] >= y - 2.0 && got[2] <= x + w + 2.0 && got[3] <= y + h + 2.0;
    assert!(inside, "text at {got:?}, box {x},{y} {w}x{h}");
}

#[test]
fn typewriter_set_checks_its_arguments() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    invalid(&mut auto, "typewriter_set", json!({ "doc": doc, "page": 2, "notes": [] }));
    let bad_font = json!({ "x": 1, "y": 1, "width": 10, "height": 10, "text": "x", "font_family": "Comic Sans" });
    invalid(&mut auto, "typewriter_set", json!({ "doc": doc, "page": 1, "notes": [bad_font] }));
    let no_size = json!({ "x": 1, "y": 1, "width": 0, "height": 10, "text": "x" });
    invalid(&mut auto, "typewriter_set", json!({ "doc": doc, "page": 1, "notes": [no_size] }));
}

// ── Ink ──────────────────────────────────────────────────────────────────────

#[test]
fn ink_lands_where_drawn_on_a_rotated_page() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("v11");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &rotated(&dir, "rot.pdf"));
    // An L: across at y=40 from x=30 to 150, then down to y=250.
    let r = ok(&mut auto, "ink_add", json!({ "doc": doc, "page": 1, "strokes": [[[30, 40], [150, 40], [150, 250]]] }));
    assert_eq!(r["dirty"], true);
    let blue = |px: &[u8]| px[2] > 120 && px[0] < 100;
    let got = crate::rendered_mark_bbox(pdfium.get(), buffer(&auto, doc), false, blue);
    crate::assert_mark_landed(got, [30.0, 40.0, 150.0, 250.0], 2.5, "ink on a /Rotate 90 page");
    invalid(&mut auto, "ink_add", json!({ "doc": doc, "page": 1, "strokes": [] }));
}

// ── Redaction ────────────────────────────────────────────────────────────────

#[test]
fn redact_find_reports_regions_without_changing_anything() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("v11");
    let mut auto = session_with(pdfium.get(), Arc::new(NoWords), &root);
    fixture_copy(&root, "a.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let r = ok(&mut auto, "redact_find", json!({ "doc": 1, "terms": ["fixture"] }));
    assert_eq!(r["regions"].as_array().unwrap().len(), 1);
    assert_eq!(r["regions"][0]["page"], 1);
    assert_eq!(r["terms"][0], json!({ "term": "fixture", "matches": 1 }));
    assert!(r["metadata_matches"].is_array());
}

/// The redacted copy goes to a new file; the open document and its file are
/// untouched (Tumbler's Save-As-only rule).
#[test]
fn redact_apply_writes_a_verified_copy_and_leaves_the_original() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("v11");
    let mut auto = session_with(pdfium.get(), Arc::new(NoWords), &root);
    let original = fixture_copy(&root, "a.pdf");
    let original_bytes = std::fs::read(&original).unwrap();
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let before = buffer(&auto, 1);

    let r = ok(&mut auto, "redact_apply", json!({ "doc": 1, "terms": ["Fixture"], "out": "redacted.pdf", "dpi": 100 }));
    assert_eq!(r["verified"], true, "{r}");
    assert_eq!(r["path"], s(&root.0.join("redacted.pdf")));
    assert_eq!(buffer(&auto, 1), before, "the document buffer is never redacted");
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);

    let copy = ok(&mut auto, "doc_open", json!({ "path": "redacted.pdf" }))["doc"].as_u64().unwrap();
    assert_eq!(ok(&mut auto, "text_find", json!({ "doc": copy, "query": "Fixture" }))["total"], 0);
}

#[test]
fn redact_apply_refuses_the_original_file_and_empty_requests() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("v11");
    let mut auto = session_with(pdfium.get(), Arc::new(NoWords), &root);
    fixture_copy(&root, "a.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let m = failed_with(&mut auto, "redact_apply", json!({ "doc": 1, "terms": ["Fixture"], "out": "a.pdf" }));
    assert!(m.contains("original"), "{m}");
    invalid(&mut auto, "redact_apply", json!({ "doc": 1, "out": "x.pdf" }));
    failed_with(&mut auto, "redact_apply", json!({ "doc": 1, "terms": ["Fixture"], "out": r"..\x.pdf" }));
}

/// When verification finds something recoverable, nothing is written; the
/// result says what leaked.
#[test]
fn redact_apply_never_writes_unverified_output() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("v11");
    let mut auto = session_with(pdfium.get(), Arc::new(SeesThroughBlackBoxes), &root);
    fixture_copy(&root, "a.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let r = ok(&mut auto, "redact_apply", json!({ "doc": 1, "terms": ["Fixture"], "out": "leaky.pdf", "dpi": 100 }));
    assert_eq!((r["verified"].clone(), r["saved"].clone()), (json!(false), json!(false)), "{r}");
    assert!(!r["leaks"].as_array().unwrap().is_empty(), "{r}");
    assert!(!root.0.join("leaky.pdf").exists());
}

/// A rectangle given in display space is burned exactly there on a rotated
/// page.
#[test]
fn redact_rect_lands_where_given_on_a_rotated_page() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("v11");
    let mut auto = session_with(pdfium.get(), Arc::new(NoWords), &root);
    rotated(&root, "rot.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "rot.pdf" }));
    let rect = json!({ "page": 1, "x": 40, "y": 70, "width": 100, "height": 50 });
    let r = ok(&mut auto, "redact_apply", json!({ "doc": 1, "rects": [rect], "out": "out.pdf", "dpi": 144 }));
    assert_eq!(r["verified"], true, "{r}");
    let out = std::fs::read(root.0.join("out.pdf")).unwrap();
    let black = |px: &[u8]| px[0] < 40 && px[1] < 40 && px[2] < 40;
    let got = crate::rendered_mark_bbox(pdfium.get(), out, false, black);
    // Burned boxes are padded by a point on every side.
    crate::assert_mark_landed(got, [39.0, 69.0, 141.0, 121.0], 2.0, "redaction on a /Rotate 90 page");
}

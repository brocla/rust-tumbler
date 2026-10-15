//! Tests for the editing tools (phase 2): pages, metadata and passwords, OCR,
//! compression, text export, linearize, Expand Margins, and the command
//! coverage check. Shared helpers live in `tests.rs`.

use super::paths::tests::TempDir;
use super::tests::*;
use super::*;
use crate::commands::ocr::{OcrEngine, OcrWord};
use crate::error::AppError;
use crate::state::AppState;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// A PDF with one blank page per `(width, height)`, built with lopdf so the
/// fixture needs no pdfium.
pub(super) fn pages_pdf(sizes: &[(f32, f32)]) -> Vec<u8> {
    use lopdf::{dictionary, Dictionary, Document, Object, Stream};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let kids: Vec<Object> = sizes
        .iter()
        .map(|&(w, h)| {
            let content = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), Vec::new())));
            Object::Reference(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)],
                "Contents" => Object::Reference(content),
                "Resources" => Dictionary::new(),
            }))
        })
        .collect();
    let count = kids.len() as i64;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog));
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("serialize");
    out
}

/// Three blank pages of different sizes, so a reorder or delete shows.
const THREE: [(f32, f32); 3] = [(200.0, 200.0), (300.0, 300.0), (400.0, 200.0)];

fn sizes(auto: &mut Automation, doc: u64) -> Vec<(f64, f64)> {
    let info = ok(auto, "doc_info", json!({ "doc": doc }));
    info["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["width"].as_f64().unwrap(), p["height"].as_f64().unwrap()))
        .collect()
}

fn lopdf_page_count(path: &Path) -> usize {
    lopdf::Document::load(path).expect("load output").get_pages().len()
}

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)
}

fn copy_in(dir: &TempDir, rel: &str, name: &str) -> PathBuf {
    let p = dir.0.join(name);
    std::fs::copy(fixture(rel), &p).expect("copy fixture");
    p
}

/// A session whose OCR engine fails as a machine without a language pack does.
fn session_without_ocr(pdfium: &'static pdfium_render::prelude::Pdfium) -> Automation {
    struct NoLanguagePack;
    impl OcrEngine for NoLanguagePack {
        fn recognize(&self, _: &[u8], _: u32, _: u32) -> Result<Vec<OcrWord>, AppError> {
            Err(AppError::Other("No OCR language pack is installed".into()))
        }
    }
    Automation::new(AppState::new(pdfium, None).with_ocr_engine(Arc::new(NoLanguagePack)))
}

fn cancelled() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

fn no_progress() -> Progress {
    Arc::new(|_, _| {})
}

// ── Pages ────────────────────────────────────────────────────────────────────

#[test]
fn page_delete_removes_pages_in_memory() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("pages");
    let mut auto = session(pdfium.get());
    let path = dir.file("three.pdf", &pages_pdf(&THREE));
    let doc = open(&mut auto, &path);
    let r = ok(&mut auto, "page_delete", json!({ "doc": doc, "pages": [2] }));
    assert_eq!((r["page_count"].clone(), r["dirty"].clone()), (json!(2), json!(true)));
    assert_eq!(r["invalidates_signatures"], false);
    assert_eq!(sizes(&mut auto, doc), [(200.0, 200.0), (400.0, 200.0)]);
    // Nothing reaches the file until doc_save.
    assert_eq!(lopdf_page_count(&path), 3);

    invalid(&mut auto, "page_delete", json!({ "doc": doc, "pages": [3] }));
    let m = failed_with(&mut auto, "page_delete", json!({ "doc": doc, "pages": [1, 2] }));
    assert!(m.contains("all pages"), "{m}");
}

#[test]
fn page_rotate_turns_pages_by_degrees() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("pages");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &dir.file("three.pdf", &pages_pdf(&THREE)));
    ok(&mut auto, "page_rotate", json!({ "doc": doc, "pages": [3], "degrees": 90 }));
    assert_eq!(sizes(&mut auto, doc)[2], (200.0, 400.0));
    // -90 is a quarter turn back; with no pages, every page turns.
    ok(&mut auto, "page_rotate", json!({ "doc": doc, "degrees": -90 }));
    assert_eq!(sizes(&mut auto, doc), [(200.0, 200.0), (300.0, 300.0), (400.0, 200.0)]);
    let m = invalid(&mut auto, "page_rotate", json!({ "doc": doc, "degrees": 45 }));
    assert!(m.contains("90"), "{m}");
}

#[test]
fn page_reorder_takes_the_full_new_order() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("pages");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &dir.file("three.pdf", &pages_pdf(&THREE)));
    ok(&mut auto, "page_reorder", json!({ "doc": doc, "order": [3, 1, 2] }));
    assert_eq!(sizes(&mut auto, doc), [(400.0, 200.0), (200.0, 200.0), (300.0, 300.0)]);
    invalid(&mut auto, "page_reorder", json!({ "doc": doc, "order": [1, 2] }));
    invalid(&mut auto, "page_reorder", json!({ "doc": doc, "order": [1, 1, 2] }));
}

#[test]
fn page_insert_file_inserts_another_pdfs_pages() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("pages");
    let outside = TempDir::new("outside");
    let mut auto = rooted(pdfium.get(), root.path());
    root.file("three.pdf", &pages_pdf(&THREE));
    fixture_copy(&root, "one.pdf");
    let doc = ok(&mut auto, "doc_open", json!({ "path": "three.pdf" }))["doc"].as_u64().unwrap();

    let r = ok(&mut auto, "page_insert_file", json!({ "doc": doc, "path": "one.pdf", "after": 1 }));
    assert_eq!(r["page_count"], 4);
    assert_eq!(sizes(&mut auto, doc)[1], (200.0, 200.0));
    // Default: at the end. after 0: before the first page.
    ok(&mut auto, "page_insert_file", json!({ "doc": doc, "path": "one.pdf" }));
    ok(&mut auto, "page_insert_file", json!({ "doc": doc, "path": "one.pdf", "after": 0 }));
    assert_eq!(auto.page_count(doc).unwrap(), 6);

    let elsewhere = fixture_copy(&outside, "x.pdf");
    let m = failed_with(&mut auto, "page_insert_file", json!({ "doc": doc, "path": s(&elsewhere) }));
    assert!(m.contains("outside"), "{m}");
    invalid(&mut auto, "page_insert_file", json!({ "doc": doc, "path": "one.pdf", "after": 7 }));
}

#[test]
fn doc_split_writes_one_file_per_range_and_leaves_the_document_alone() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("split");
    let mut auto = rooted(pdfium.get(), root.path());
    root.file("three.pdf", &pages_pdf(&THREE));
    std::fs::create_dir(root.0.join("out")).unwrap();
    let doc = ok(&mut auto, "doc_open", json!({ "path": "three.pdf" }))["doc"].as_u64().unwrap();

    let r = ok(&mut auto, "doc_split", json!({ "doc": doc, "out_dir": "out" }));
    let files: Vec<String> = r["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
    let want: Vec<String> = ["three-1.pdf", "three-2.pdf", "three-3.pdf"].iter().map(|n| s(&root.0.join("out").join(n))).collect();
    assert_eq!(files, want);
    assert!(files.iter().all(|f| lopdf_page_count(Path::new(f)) == 1));

    std::fs::create_dir(root.0.join("ranges")).unwrap();
    let r = ok(
        &mut auto,
        "doc_split",
        json!({ "doc": doc, "out_dir": "ranges", "ranges": [{ "first": 1, "last": 2 }, { "first": 3, "last": 3 }] }),
    );
    let first = r["files"][0]["path"].as_str().unwrap();
    assert!(first.ends_with("three-1-2.pdf"), "{first}");
    assert_eq!(lopdf_page_count(Path::new(first)), 2);
    assert!(!buffer_and_dirty(&auto, doc).1, "split must not touch the document");
}

/// Split never replaces a file: if any output exists, nothing is written.
#[test]
fn doc_split_refuses_to_overwrite_and_checks_ranges() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("split");
    let mut auto = rooted(pdfium.get(), root.path());
    root.file("three.pdf", &pages_pdf(&THREE));
    std::fs::create_dir(root.0.join("out")).unwrap();
    root.file(r"out\three-3.pdf", b"keep me");
    ok(&mut auto, "doc_open", json!({ "path": "three.pdf" }));
    let m = failed_with(&mut auto, "doc_split", json!({ "doc": 1, "out_dir": "out" }));
    assert!(m.contains("three-3.pdf") && m.contains("exists"), "{m}");
    assert!(!root.0.join("out").join("three-1.pdf").exists());
    assert_eq!(std::fs::read(root.0.join("out").join("three-3.pdf")).unwrap(), b"keep me");

    invalid(&mut auto, "doc_split", json!({ "doc": 1, "out_dir": "out", "ranges": [{ "first": 2, "last": 1 }] }));
    invalid(&mut auto, "doc_split", json!({ "doc": 1, "out_dir": "out", "ranges": [{ "first": 1, "last": 4 }] }));
    invalid(&mut auto, "doc_split", json!({ "doc": 1, "out_dir": "out", "every": 2, "ranges": [{ "first": 1, "last": 1 }] }));
    failed_with(&mut auto, "doc_split", json!({ "doc": 1, "out_dir": r"..\elsewhere" }));
}

#[test]
fn doc_split_every_n_pages() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("split");
    let mut auto = rooted(pdfium.get(), root.path());
    root.file("three.pdf", &pages_pdf(&THREE));
    ok(&mut auto, "doc_open", json!({ "path": "three.pdf" }));
    let r = ok(&mut auto, "doc_split", json!({ "doc": 1, "out_dir": ".", "every": 2 }));
    let pages: Vec<(u64, u64)> = r["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["first_page"].as_u64().unwrap(), f["last_page"].as_u64().unwrap()))
        .collect();
    assert_eq!(pages, [(1, 2), (3, 3)]);
}

// ── Metadata and passwords ───────────────────────────────────────────────────

#[test]
fn doc_set_info_changes_only_the_given_fields() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("meta");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    ok(&mut auto, "doc_set_info", json!({ "doc": doc, "author": "Ada" }));
    let before = ok(&mut auto, "doc_info", json!({ "doc": doc }))["metadata"].clone();
    let r = ok(&mut auto, "doc_set_info", json!({ "doc": doc, "title": "New title" }));
    assert_eq!(r["dirty"], true);
    let after = ok(&mut auto, "doc_info", json!({ "doc": doc }))["metadata"].clone();
    assert_eq!(after["title"], "New title");
    assert_eq!(after["author"], "Ada");
    assert_eq!(after["subject"], before["subject"]);
    let m = invalid(&mut auto, "doc_set_info", json!({ "doc": doc }));
    assert!(m.contains("title"), "{m}");
}

#[test]
fn doc_protect_then_save_needs_the_password_to_reopen() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("protect");
    let mut auto = session(pdfium.get());
    let path = fixture_copy(&dir, "a.pdf");
    let doc = open(&mut auto, &path);
    let r = ok(&mut auto, "doc_protect", json!({ "doc": doc, "password": "s3cret" }));
    assert_eq!((r["dirty"].clone(), r["encrypted"].clone()), (json!(true), json!(true)));
    ok(&mut auto, "doc_save", json!({ "doc": doc }));
    ok(&mut auto, "doc_close", json!({ "doc": doc }));
    let m = failed_with(&mut auto, "doc_open", json!({ "path": s(&path) }));
    assert!(m.contains("password"), "{m}");
    ok(&mut auto, "doc_open", json!({ "path": s(&path), "password": "s3cret" }));
    invalid(&mut auto, "doc_protect", json!({ "doc": 2, "password": "" }));
}

#[test]
fn doc_unprotect_then_save_writes_a_plain_file() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("protect");
    let mut auto = session(pdfium.get());
    let path = dir.0.join("enc.pdf");
    std::fs::copy(crate::encrypted_fixture_path(), &path).unwrap();
    let doc = ok(&mut auto, "doc_open", json!({ "path": s(&path), "password": crate::ENCRYPTED_FIXTURE_PASSWORD }))["doc"]
        .as_u64()
        .unwrap();
    let r = ok(&mut auto, "doc_unprotect", json!({ "doc": doc }));
    assert_eq!(r["encrypted"], false);
    ok(&mut auto, "doc_save", json!({ "doc": doc }));
    ok(&mut auto, "doc_close", json!({ "doc": doc }));
    let r = ok(&mut auto, "doc_open", json!({ "path": s(&path) }));
    assert_eq!(r["encrypted"], false);
    let m = failed_with(&mut auto, "doc_unprotect", json!({ "doc": r["doc"] }));
    assert!(m.contains("not password-protected"), "{m}");
}

/// The GUI warns before an edit breaks a signature (confirmBreakingEdit);
/// headless, every edit reports whether it did.
#[test]
fn edits_to_a_signed_document_report_invalidated_signatures() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("signed");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &copy_in(&dir, "signatures/signed-valid.pdf", "signed.pdf"));
    assert_ne!(ok(&mut auto, "sign_list", json!({ "doc": doc }))["count"], 0);
    let r = ok(&mut auto, "doc_set_info", json!({ "doc": doc, "title": "x" }));
    assert_eq!(r["invalidates_signatures"], true);
    let r = ok(&mut auto, "page_rotate", json!({ "doc": doc, "degrees": 90 }));
    assert_eq!(r["invalidates_signatures"], true);
}

// ── OCR ──────────────────────────────────────────────────────────────────────

#[test]
fn ocr_status_reports_whether_recognition_works() {
    let pdfium = crate::test_pdfium();
    let mut auto = session(pdfium.get());
    assert_eq!(ok(&mut auto, "ocr_status", json!({}))["available"], true);
    let mut auto = session_without_ocr(pdfium.get());
    let r = ok(&mut auto, "ocr_status", json!({}));
    assert_eq!(r["available"], false);
    assert!(r["message"].as_str().unwrap().contains("language pack"), "{r}");
}

#[test]
fn ocr_recognize_embeds_a_text_layer_by_default() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("ocr");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &dir.file("scan.pdf", &pages_pdf(&[(300.0, 200.0)])));
    let r = ok(&mut auto, "ocr_recognize", json!({ "doc": doc }));
    assert_eq!((r["pages_recognized"].clone(), r["pages_written"].clone()), (json!(1), json!(1)));
    assert_eq!(r["dirty"], true);
    // The words are now part of the file's own text.
    let t = ok(&mut auto, "text_extract", json!({ "doc": doc }));
    assert_eq!(t["pages"][0]["source"], "text");
    assert!(t["pages"][0]["text"].as_str().unwrap().contains("Scanned"), "{t}");
}

#[test]
fn ocr_recognize_without_embedding_leaves_the_document_unchanged() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("ocr");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &dir.file("scan.pdf", &pages_pdf(&[(300.0, 200.0), (300.0, 200.0)])));
    let before = buffer_and_dirty(&auto, doc);
    let r = ok(&mut auto, "ocr_recognize", json!({ "doc": doc, "pages": [2], "embed": false }));
    assert_eq!(r["pages_recognized"], 1);
    assert_eq!(buffer_and_dirty(&auto, doc), before);
    let t = ok(&mut auto, "text_extract", json!({ "doc": doc }));
    assert_eq!((t["pages"][0]["source"].clone(), t["pages"][1]["source"].clone()), (json!("none"), json!("ocr")));
}

/// force re-recognizes pages that already have a (junk) text layer, and the
/// OCR words then win over it (issue #97).
#[test]
fn ocr_recognize_force_overrides_an_existing_layer() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("ocr");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let r = ok(&mut auto, "ocr_recognize", json!({ "doc": doc, "embed": false }));
    assert_eq!(r["pages_recognized"], 0, "a page with text is skipped without force");
    ok(&mut auto, "ocr_recognize", json!({ "doc": doc, "embed": false, "force": true }));
    let t = ok(&mut auto, "text_extract", json!({ "doc": doc }));
    assert_eq!((t["pages"][0]["source"].clone(), t["pages"][0]["text"].clone()), (json!("ocr"), json!("Scanned")));
}

#[test]
fn ocr_recognize_fails_clearly_without_a_language_pack() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("ocr");
    let mut auto = session_without_ocr(pdfium.get());
    let doc = open(&mut auto, &dir.file("scan.pdf", &pages_pdf(&[(300.0, 200.0)])));
    let m = failed_with(&mut auto, "ocr_recognize", json!({ "doc": doc }));
    assert!(m.contains("language pack"), "{m}");
    assert!(!buffer_and_dirty(&auto, doc).1);
}

#[test]
fn long_tools_report_progress_and_stop_when_cancelled() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("ocr");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &dir.file("scan.pdf", &pages_pdf(&[(300.0, 200.0), (300.0, 200.0)])));

    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let progress: Progress = Arc::new(move |done, total| record.lock().unwrap().push((done, total)));
    let args = json!({ "doc": doc, "embed": false });
    auto.call_with("ocr_recognize", &args, progress, Arc::new(AtomicBool::new(false))).expect("ocr");
    let seen = seen.lock().unwrap().clone();
    assert!(seen.contains(&(1, 2)) && seen.contains(&(2, 2)), "{seen:?}");

    // A cancelled edit applies nothing.
    let doc2 = open(&mut auto, &dir.file("scan2.pdf", &pages_pdf(&[(300.0, 200.0)])));
    for (tool, args) in [
        ("ocr_recognize", json!({ "doc": doc2 })),
        ("doc_optimize", json!({ "doc": doc2 })),
        ("margins_expand", json!({ "doc": doc2 })),
        ("doc_export_text", json!({ "doc": doc2, "path": s(&dir.0.join("t.txt")) })),
    ] {
        match auto.call_with(tool, &args, no_progress(), cancelled()) {
            Err(ToolError::Failed(m)) => assert!(m.contains("cancelled"), "{tool}: {m}"),
            other => panic!("{tool}: expected a cancellation, got {other:?}"),
        }
        assert!(!buffer_and_dirty(&auto, doc2).1, "{tool} applied a cancelled edit");
    }
    assert!(!dir.0.join("t.txt").exists());
}

// ── Compression ──────────────────────────────────────────────────────────────

#[test]
fn doc_audit_images_lists_images() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("images");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    assert_eq!(ok(&mut auto, "doc_audit_images", json!({ "doc": doc }))["images"], json!([]));
}

#[test]
fn doc_optimize_runs_the_lossless_steps_by_default() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("optimize");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let r = ok(&mut auto, "doc_optimize", json!({ "doc": doc }));
    let steps: Vec<&str> = r["results"].as_array().unwrap().iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["recompress_streams", "prune_unused", "delete_zero_length", "strip_extras"]);
    assert_eq!(r["dirty"], true);
    assert_eq!(r["breaks_conformance"], json!([]));

    let r = ok(&mut auto, "doc_optimize", json!({ "doc": doc, "steps": ["recompress_images"], "dpi": 100, "quality": 60 }));
    assert_eq!(r["results"][0]["step"], "recompress_images");
    invalid(&mut auto, "doc_optimize", json!({ "doc": doc, "steps": ["shrink"] }));
    invalid(&mut auto, "doc_optimize", json!({ "doc": doc, "quality": 0 }));
}

/// The Compress panel warns that optimizing voids a declared PDF/A or PDF/X
/// claim; headless, the result says which claims it broke.
#[test]
fn doc_optimize_reports_the_conformance_it_breaks() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("optimize");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &copy_in(&dir, "conformance/pdfa-2b.pdf", "a.pdf"));
    let r = ok(&mut auto, "doc_optimize", json!({ "doc": doc }));
    assert_eq!(r["breaks_conformance"], json!(["PDF/A-2b"]));
}

// ── Text export, linearize ───────────────────────────────────────────────────

#[test]
fn doc_export_text_writes_a_text_file() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("export");
    let mut auto = rooted(pdfium.get(), root.path());
    fixture_copy(&root, "a.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let r = ok(&mut auto, "doc_export_text", json!({ "doc": 1, "path": "a.txt" }));
    assert_eq!(r["pages"], 1);
    assert_eq!(r["path"], s(&root.0.join("a.txt")));
    let text = std::fs::read_to_string(root.0.join("a.txt")).unwrap();
    assert!(text.contains("--- Page 1 ---") && text.contains("Test Fixture"), "{text}");
    failed_with(&mut auto, "doc_export_text", json!({ "doc": 1, "path": r"..\a.txt" }));
}

#[test]
fn doc_linearize_writes_a_copy_and_leaves_the_document() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("linearize");
    let mut auto = rooted(pdfium.get(), root.path());
    fixture_copy(&root, "a.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let r = ok(&mut auto, "doc_linearize", json!({ "doc": 1, "path": "web.pdf" }));
    assert_eq!(r["path"], s(&root.0.join("web.pdf")));
    assert!(r["linearized_size"].as_u64().unwrap() > 0);
    assert!(root.0.join("web.pdf").exists());
    assert!(!buffer_and_dirty(&auto, 1).1);
    let m = failed_with(&mut auto, "doc_linearize", json!({ "doc": 1, "path": "a.pdf" }));
    assert!(m.contains("currently open"), "{m}");
}

// ── Expand Margins ───────────────────────────────────────────────────────────

#[test]
fn margins_analyze_reports_ink_boxes_and_the_scale() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("margins");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let r = ok(&mut auto, "margins_analyze", json!({ "doc": doc }));
    assert!(r["pages"][0]["bbox"].is_object(), "{r}");
    assert!(r["scale"].as_f64().unwrap() > 1.0, "{r}");
    assert!(!buffer_and_dirty(&auto, doc).1);
}

#[test]
fn margins_expand_scales_the_content() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("margins");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let r = ok(&mut auto, "margins_expand", json!({ "doc": doc, "padding": 10 }));
    assert!(r["scale"].as_f64().unwrap() > 1.0, "{r}");
    assert_eq!(r["dirty"], true);

    let blank = open(&mut auto, &dir.file("blank.pdf", &pages_pdf(&[(300.0, 200.0)])));
    let m = failed_with(&mut auto, "margins_expand", json!({ "doc": blank }));
    assert!(m.contains("No page content"), "{m}");
}

// ── Coverage: every GUI command has a CLI decision ───────────────────────────

/// The `#[tauri::command]`s the GUI registers, read from `generate_handler!`
/// in lib.rs.
fn registered_commands() -> Vec<String> {
    let lib = include_str!("../lib.rs");
    let start = lib.find("generate_handler![").expect("generate_handler! in lib.rs");
    let end = start + lib[start..].find(']').expect("closing bracket");
    lib[start..end]
        .split(',')
        .filter_map(|item| item.trim().rsplit("::").next().map(str::trim).map(str::to_string))
        .filter(|name| !name.is_empty() && !name.contains('['))
        .collect()
}

/// A new GUI command fails this test until someone decides its CLI fate: a
/// tool that names it in `commands`, or an entry in `NOT_AUTOMATED` with the
/// reason.
#[test]
fn every_gui_command_is_automated_or_excluded() {
    let registered = registered_commands();
    assert!(registered.len() > 40, "parsed too few commands: {registered:?}");
    let automated: Vec<&str> = tools().iter().flat_map(|t| t.commands.iter().copied()).collect();
    let excluded: Vec<&str> = tools::NOT_AUTOMATED.iter().map(|(name, _)| *name).collect();
    for name in &registered {
        let (a, e) = (automated.contains(&name.as_str()), excluded.contains(&name.as_str()));
        assert!(a || e, "GUI command {name} has no tool and no NOT_AUTOMATED entry");
        assert!(!(a && e), "GUI command {name} is both automated and excluded");
    }
    for name in automated.iter().chain(&excluded) {
        assert!(registered.iter().any(|r| r == name), "{name} is not a registered GUI command");
    }
    for (name, reason) in tools::NOT_AUTOMATED {
        assert!(!reason.trim().is_empty(), "{name} needs a reason");
    }
}

#[test]
fn registered_commands_parses_lib_rs() {
    let names = registered_commands();
    assert!(names.contains(&"open_document".to_string()));
    assert!(names.contains(&"get_app_version".to_string()));
    assert!(names.iter().all(|n| n.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())), "{names:?}");
}


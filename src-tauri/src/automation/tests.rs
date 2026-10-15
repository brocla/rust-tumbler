//! Tool tests. Each goes through [`Automation::call`], the path both front
//! doors take, so one test covers CLI and MCP behavior.

use super::paths::tests::TempDir;
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

// ── Fakes for the injected dependencies ──────────────────────────────────────

/// Recognizes a fixed list of words on every page.
pub(super) struct FakeOcr(pub(super) Vec<OcrWord>);

impl OcrEngine for FakeOcr {
    fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
        Ok(self.0.clone())
    }
}

/// Copies the source unchanged, standing in for qpdf.
pub(super) struct FakeLinearizer;

impl Linearizer for FakeLinearizer {
    fn linearize(&self, src: &Path, dest: &Path) -> Result<(), AppError> {
        std::fs::copy(src, dest).map(|_| ()).map_err(|e| AppError::io("fake linearize", e))
    }
}

pub(super) fn word(text: &str, x: f32, y: f32) -> OcrWord {
    OcrWord { text: text.to_string(), rect: TextRect { x, y, width: 30.0, height: 10.0 } }
}

/// A session over `pdfium` with fake OCR and linearizer, no root.
pub(super) fn session(pdfium: &'static Pdfium) -> Automation {
    let state = AppState::new(pdfium, None)
        .with_ocr_engine(Arc::new(FakeOcr(vec![word("Scanned", 10.0, 150.0)])))
        .with_linearizer(Arc::new(FakeLinearizer));
    Automation::new(state)
}

pub(super) fn rooted(pdfium: &'static Pdfium, root: &Path) -> Automation {
    session(pdfium).with_root(root).expect("root")
}

/// The single JSON result of a call that must succeed.
pub(super) fn ok(auto: &mut Automation, tool: &str, args: Value) -> Value {
    match auto.call(tool, &args) {
        Ok(content) => match content.as_slice() {
            [Content::Json(v)] => v.clone(),
            other => panic!("{tool}: expected one JSON result, got {other:?}"),
        },
        Err(e) => panic!("{tool} {args}: {e:?}"),
    }
}

pub(super) fn failed_with(auto: &mut Automation, tool: &str, args: Value) -> String {
    match auto.call(tool, &args) {
        Err(ToolError::Failed(m)) => m,
        other => panic!("{tool} {args}: expected Failed, got {other:?}"),
    }
}

pub(super) fn invalid(auto: &mut Automation, tool: &str, args: Value) -> String {
    match auto.call(tool, &args) {
        Err(ToolError::InvalidArgs(m)) => m,
        other => panic!("{tool} {args}: expected InvalidArgs, got {other:?}"),
    }
}

pub(super) fn fixture_copy(dir: &TempDir, name: &str) -> PathBuf {
    let p = dir.0.join(name);
    std::fs::copy(crate::fixture_path(), &p).expect("copy fixture");
    p
}

pub(super) fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Opens `path` and returns its doc id.
pub(super) fn open(auto: &mut Automation, path: &Path) -> u64 {
    ok(auto, "doc_open", json!({ "path": s(path) }))["doc"].as_u64().expect("doc id")
}

pub(super) fn buffer_and_dirty(auto: &Automation, doc: u64) -> (Vec<u8>, bool) {
    let entry = auto.state.get_document(&doc.to_string()).expect("doc");
    let entry = entry.lock().expect("lock");
    (entry.buffer.clone(), entry.dirty)
}

/// Marks a document dirty without a v1 edit tool: re-applies its own bytes.
pub(super) fn make_dirty(auto: &Automation, doc: u64) {
    let (bytes, _) = buffer_and_dirty(auto, doc);
    auto.state.set_buffer_and_refresh(&doc.to_string(), bytes).expect("refresh");
}

// ── Table invariants ─────────────────────────────────────────────────────────

#[test]
fn tool_names_are_unique_and_lowercase_snake() {
    let mut names: Vec<&str> = tools().iter().map(|t| t.name).collect();
    for n in &names {
        assert!(!n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "bad name {n:?}");
    }
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count, "duplicate tool names");
}

/// Every schema is a closed object schema an MCP client can use as-is: no
/// `$ref` (subschemas are inlined), `required` names real properties, unknown
/// properties are rejected, and every property says what it is.
#[test]
fn every_schema_is_a_closed_self_contained_object() {
    fn no_refs(v: &Value, tool: &str) {
        match v {
            Value::Object(m) => {
                assert!(!m.contains_key("$ref"), "{tool}: $ref in schema");
                m.values().for_each(|v| no_refs(v, tool));
            }
            Value::Array(a) => a.iter().for_each(|v| no_refs(v, tool)),
            _ => {}
        }
    }
    for t in tools() {
        let schema = &t.input_schema;
        assert_eq!(schema["type"], "object", "{}", t.name);
        assert_eq!(schema["additionalProperties"], false, "{}", t.name);
        let props = schema["properties"].as_object().unwrap_or_else(|| panic!("{}: no properties", t.name));
        for r in schema["required"].as_array().into_iter().flatten() {
            assert!(props.contains_key(r.as_str().unwrap()), "{}: required {r} is not a property", t.name);
        }
        for (name, prop) in props {
            let d = prop["description"].as_str().unwrap_or("");
            assert!(!d.trim().is_empty(), "{}: property {name} has no description", t.name);
        }
        no_refs(schema, t.name);
        assert!(!t.title.is_empty() && !t.description.is_empty(), "{}: title/description", t.name);
        assert!(!(t.read_only && t.destructive), "{}: both read_only and destructive", t.name);
    }
}

/// Args for every read-only tool, against an open single-page fixture as doc 1.
fn read_only_samples() -> Vec<(&'static str, Value)> {
    vec![
        ("doc_list", json!({})),
        ("doc_info", json!({ "doc": 1 })),
        ("page_render", json!({ "doc": 1, "page": 1, "dpi": 36 })),
        ("text_extract", json!({ "doc": 1 })),
        ("text_find", json!({ "doc": 1, "query": "Fixture" })),
        ("sign_list", json!({ "doc": 1 })),
        ("doc_conformance", json!({ "doc": 1 })),
        ("ocr_status", json!({})),
        ("doc_audit_images", json!({ "doc": 1 })),
        ("margins_analyze", json!({ "doc": 1 })),
    ]
}

/// A tool marked read-only must not change the document: run each against a
/// fixture and compare the buffer byte for byte, and the dirty flag.
#[test]
fn read_only_tools_leave_the_document_unchanged() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("ro");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let before = buffer_and_dirty(&auto, doc);

    let samples = read_only_samples();
    let mut sampled: Vec<&str> = samples.iter().map(|(n, _)| *n).collect();
    for (tool, args) in samples {
        auto.call(tool, &args).unwrap_or_else(|e| panic!("{tool}: {e:?}"));
        assert_eq!(buffer_and_dirty(&auto, doc), before, "{tool} changed the document");
    }

    // Every read-only tool must be sampled, so a new one can't skip this check.
    let mut read_only: Vec<&str> = tools().iter().filter(|t| t.read_only).map(|t| t.name).collect();
    read_only.sort_unstable();
    sampled.sort_unstable();
    assert_eq!(sampled, read_only, "read_only_samples must list exactly the read-only tools");
}

// ── Dispatch and argument errors ─────────────────────────────────────────────

#[test]
fn unknown_tool_unknown_arg_and_missing_arg_are_reported() {
    let pdfium = crate::test_pdfium();
    let mut auto = session(pdfium.get());
    assert!(matches!(auto.call("nope", &json!({})), Err(ToolError::UnknownTool(_))));

    let m = invalid(&mut auto, "doc_open", json!({ "path": "x.pdf", "pasword": "typo" }));
    assert!(m.contains("pasword"), "{m}");
    let m = invalid(&mut auto, "doc_open", json!({}));
    assert!(m.contains("path"), "{m}");
    let m = invalid(&mut auto, "doc_info", json!({ "doc": "one" }));
    assert!(m.contains("doc"), "{m}");
    invalid(&mut auto, "doc_list", json!([1, 2]));
    // `null` means "no arguments".
    ok(&mut auto, "doc_list", Value::Null);
}

#[test]
fn unknown_document_id_fails_clearly() {
    let pdfium = crate::test_pdfium();
    let mut auto = session(pdfium.get());
    let m = failed_with(&mut auto, "doc_info", json!({ "doc": 7 }));
    assert!(m.contains("7"), "{m}");
    invalid(&mut auto, "doc_info", json!({ "doc": 0 }));
}

// ── doc_open / doc_list / doc_close ──────────────────────────────────────────

#[test]
fn doc_open_reports_pages_and_numbers_documents_from_one() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("open");
    let mut auto = session(pdfium.get());
    let a = fixture_copy(&dir, "a.pdf");
    let r = ok(&mut auto, "doc_open", json!({ "path": s(&a) }));
    assert_eq!(r["doc"], 1);
    assert_eq!(r["path"], s(&a));
    assert_eq!(r["page_count"], 1);
    assert_eq!(r["pages"], json!([{ "width": 200.0, "height": 200.0 }]));
    assert_eq!(r["encrypted"], false);
    assert_eq!(r["dirty"], false);

    let b = fixture_copy(&dir, "b.pdf");
    assert_eq!(open(&mut auto, &b), 2);
}

/// One open document per file, as in the GUI: Save and the open-elsewhere
/// checks rely on it.
#[test]
fn doc_open_refuses_a_file_that_is_already_open() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("open");
    let mut auto = session(pdfium.get());
    let a = fixture_copy(&dir, "a.pdf");
    open(&mut auto, &a);
    // A different spelling of the same file is still the same file.
    let other_spelling = format!(r"{}\.\A.PDF", dir.0.display());
    let m = failed_with(&mut auto, "doc_open", json!({ "path": other_spelling }));
    assert!(m.contains("already open") && m.contains('1'), "{m}");
}

#[test]
fn doc_open_password_flow() {
    let pdfium = crate::test_pdfium();
    let mut auto = session(pdfium.get());
    let path = s(&crate::encrypted_fixture_path());
    let m = failed_with(&mut auto, "doc_open", json!({ "path": path }));
    assert!(m.contains("password"), "{m}");
    let m = failed_with(&mut auto, "doc_open", json!({ "path": path, "password": "wrong" }));
    assert!(m.contains("password"), "{m}");
    let r = ok(
        &mut auto,
        "doc_open",
        json!({ "path": path, "password": crate::ENCRYPTED_FIXTURE_PASSWORD }),
    );
    assert_eq!(r["encrypted"], true);
}

#[test]
fn doc_open_is_confined_to_the_root() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("root");
    let outside = TempDir::new("outside");
    let mut auto = rooted(pdfium.get(), root.path());
    fixture_copy(&root, "in.pdf");
    let out = fixture_copy(&outside, "out.pdf");
    assert_eq!(ok(&mut auto, "doc_open", json!({ "path": "in.pdf" }))["doc"], 1);
    let m = failed_with(&mut auto, "doc_open", json!({ "path": s(&out) }));
    assert!(m.contains("outside"), "{m}");
}

#[test]
fn with_root_requires_an_existing_folder() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("root");
    let file = dir.file("f.txt", b"x");
    assert!(session(pdfium.get()).with_root(&file).is_err());
    assert!(session(pdfium.get()).with_root(dir.0.join("missing")).is_err());
}

#[test]
fn doc_list_and_doc_close() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("list");
    let mut auto = session(pdfium.get());
    assert_eq!(ok(&mut auto, "doc_list", json!({})), json!({ "documents": [] }));
    let a = fixture_copy(&dir, "a.pdf");
    let b = fixture_copy(&dir, "b.pdf");
    open(&mut auto, &a);
    open(&mut auto, &b);

    let list = ok(&mut auto, "doc_list", json!({}));
    let docs = list["documents"].as_array().expect("array");
    assert_eq!(docs.len(), 2);
    assert_eq!((docs[0]["doc"].clone(), docs[0]["path"].clone()), (json!(1), json!(s(&a))));
    assert_eq!(docs[1]["doc"], 2);
    assert_eq!(docs[1]["page_count"], 1);
    assert_eq!(docs[1]["dirty"], false);

    assert_eq!(ok(&mut auto, "doc_close", json!({ "doc": 1 })), json!({ "closed": 1 }));
    let list = ok(&mut auto, "doc_list", json!({}));
    assert_eq!(list["documents"].as_array().unwrap().len(), 1);
    failed_with(&mut auto, "doc_close", json!({ "doc": 1 }));
    // Closing frees the file to be opened again, under a fresh id.
    assert_eq!(open(&mut auto, &a), 3);
}

/// The GUI asks before closing a document with unsaved changes; headless, the
/// tool refuses unless told to discard them.
#[test]
fn doc_close_refuses_unsaved_changes_unless_discarding() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("close");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    make_dirty(&auto, doc);
    let m = failed_with(&mut auto, "doc_close", json!({ "doc": doc }));
    assert!(m.contains("unsaved") && m.contains("discard_changes"), "{m}");
    ok(&mut auto, "doc_close", json!({ "doc": doc, "discard_changes": true }));
}

/// Closing drops the document's OCR words with it, as the GUI does.
#[test]
fn doc_close_clears_the_ocr_cache() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("close");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    auto.state.set_ocr_words(&doc.to_string(), 1, vec![word("x", 0.0, 0.0)]);
    ok(&mut auto, "doc_close", json!({ "doc": doc }));
    assert!(auto.state.get_ocr_words(&doc.to_string(), 1).is_none());
}

// ── doc_info ─────────────────────────────────────────────────────────────────

#[test]
fn doc_info_summarizes_the_document() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("info");
    let mut auto = session(pdfium.get());
    let path = fixture_copy(&dir, "a.pdf");
    let doc = open(&mut auto, &path);
    let r = ok(&mut auto, "doc_info", json!({ "doc": doc }));
    assert_eq!(r["doc"], doc);
    assert_eq!(r["path"], s(&path));
    assert_eq!(r["page_count"], 1);
    assert_eq!(r["pages"], json!([{ "width": 200.0, "height": 200.0 }]));
    assert!(r["metadata"].is_object() && r["metadata"].get("title").is_some(), "{r}");
    assert!(r["metadata"].get("mod_date").is_some(), "keys are snake_case: {r}");
    assert_eq!(r["conformance"], json!([]));
    assert_eq!(r["signatures"]["count"], 0);
    assert_eq!(r["has_form"], false);
    assert_eq!(r["pages_without_text"], json!([]));
    assert_eq!(r["pages_ocr_only"], json!([]));
}

/// An agent learns from doc_info which pages need OCR, and which are covered
/// only by this session's OCR (searchable, but not saved in the file).
#[test]
fn doc_info_reports_pages_without_a_text_layer() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("info");
    let mut auto = session(pdfium.get());
    let blank = dir.file("blank.pdf", &crate::geometry_page_bytes(300.0, 200.0, 0, None));
    let doc = open(&mut auto, &blank);
    let r = ok(&mut auto, "doc_info", json!({ "doc": doc }));
    assert_eq!(r["pages_without_text"], json!([1]));
    assert_eq!(r["pages_ocr_only"], json!([]));

    auto.state.set_ocr_words(&doc.to_string(), 1, vec![word("Scanned", 10.0, 150.0)]);
    let r = ok(&mut auto, "doc_info", json!({ "doc": doc }));
    assert_eq!(r["pages_without_text"], json!([]));
    assert_eq!(r["pages_ocr_only"], json!([1]));
}

// ── doc_save ─────────────────────────────────────────────────────────────────

#[test]
fn doc_save_in_place_clears_dirty() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("save");
    let mut auto = session(pdfium.get());
    let path = fixture_copy(&dir, "a.pdf");
    let doc = open(&mut auto, &path);
    make_dirty(&auto, doc);
    let r = ok(&mut auto, "doc_save", json!({ "doc": doc }));
    assert_eq!(r["path"], s(&path));
    assert_eq!(r["dirty"], false);
    assert_eq!(r["bytes"], std::fs::metadata(&path).unwrap().len());
    assert!(!buffer_and_dirty(&auto, doc).1);
}

#[test]
fn doc_save_as_writes_a_new_file_and_retargets_the_document() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("save");
    let mut auto = rooted(pdfium.get(), root.path());
    fixture_copy(&root, "a.pdf");
    let doc = ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }))["doc"].as_u64().unwrap();
    let r = ok(&mut auto, "doc_save", json!({ "doc": doc, "path": "copy.pdf" }));
    let copy = root.0.join("copy.pdf");
    assert_eq!(r["path"], s(&copy));
    assert!(copy.exists());
    assert_eq!(ok(&mut auto, "doc_info", json!({ "doc": doc }))["path"], s(&copy));
}

#[test]
fn doc_save_as_is_confined_to_the_root() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("save");
    let outside = TempDir::new("outside");
    let mut auto = rooted(pdfium.get(), root.path());
    fixture_copy(&root, "a.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let target = outside.0.join("x.pdf");
    let m = failed_with(&mut auto, "doc_save", json!({ "doc": 1, "path": s(&target) }));
    assert!(m.contains("outside"), "{m}");
    assert!(!target.exists());
}

/// Saving over a file another open document holds would leave that document
/// stale; the underlying Save As refuses, and the tool passes that on.
#[test]
fn doc_save_as_refuses_a_file_open_as_another_document() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("save");
    let mut auto = session(pdfium.get());
    let a = fixture_copy(&dir, "a.pdf");
    let b = fixture_copy(&dir, "b.pdf");
    open(&mut auto, &a);
    open(&mut auto, &b);
    let m = failed_with(&mut auto, "doc_save", json!({ "doc": 1, "path": s(&b) }));
    assert!(m.contains("open"), "{m}");
}

// ── page_render ──────────────────────────────────────────────────────────────

pub(super) fn png_size(data: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory_with_format(data, image::ImageFormat::Png).expect("valid PNG");
    (img.width(), img.height())
}

#[test]
fn page_render_returns_a_png_at_the_requested_dpi() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("render");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    // 200 pt at the 96 dpi default is 267 px.
    let content = auto.call("page_render", &json!({ "doc": doc, "page": 1 })).expect("render");
    let [Content::Png { data, width, height }] = content.as_slice() else { panic!("{content:?}") };
    assert_eq!((*width, *height), (267, 267));
    assert_eq!(png_size(data), (267, 267));

    let content = auto.call("page_render", &json!({ "doc": doc, "page": 1, "dpi": 72 })).expect("render");
    let [Content::Png { width, .. }] = content.as_slice() else { panic!() };
    assert_eq!(*width, 200);
}

#[test]
fn page_render_validates_page_and_dpi() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("render");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let m = invalid(&mut auto, "page_render", json!({ "doc": doc, "page": 2 }));
    assert!(m.contains("page 2") && m.contains('1'), "{m}");
    invalid(&mut auto, "page_render", json!({ "doc": doc, "page": 0 }));
    invalid(&mut auto, "page_render", json!({ "doc": doc, "page": 1, "dpi": 601 }));
    invalid(&mut auto, "page_render", json!({ "doc": doc, "page": 1, "dpi": 0 }));
}

/// page_render returns the image only: writing it is the caller's business
/// (the CLI's `render --out`), through `write_output`, which is confined and
/// atomic. A read-only tool with a file argument could overwrite any file in
/// the root without the prompt a destructive tool gets.
#[test]
fn page_render_has_no_file_output_and_write_output_is_confined() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("render");
    let mut auto = rooted(pdfium.get(), root.path());
    fixture_copy(&root, "a.pdf");
    ok(&mut auto, "doc_open", json!({ "path": "a.pdf" }));
    let m = invalid(&mut auto, "page_render", json!({ "doc": 1, "page": 1, "out": "p1.png" }));
    assert!(m.contains("`out`"), "{m}");

    let written = auto.write_output("p1.png", b"png bytes").expect("write");
    assert_eq!(written, root.0.join("p1.png"));
    assert_eq!(std::fs::read(&written).unwrap(), b"png bytes");
    assert!(matches!(auto.write_output(r"..\p1.png", b"x"), Err(ToolError::Failed(_))));
}

/// Page sizes are in render space: on a /Rotate 90 page the width and height
/// are swapped relative to user space, and match the rendered image. Uses a
/// non-square page, where a mix-up cannot cancel out.
#[test]
fn page_sizes_and_renders_agree_on_a_rotated_page() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("render");
    let mut auto = session(pdfium.get());
    let rotated = dir.file("rot.pdf", &crate::geometry_page_bytes(300.0, 200.0, 90, None));
    let r = ok(&mut auto, "doc_open", json!({ "path": s(&rotated) }));
    assert_eq!(r["pages"], json!([{ "width": 200.0, "height": 300.0 }]));
    let content = auto.call("page_render", &json!({ "doc": 1, "page": 1, "dpi": 72 })).expect("render");
    let [Content::Png { width, height, .. }] = content.as_slice() else { panic!() };
    assert_eq!((*width, *height), (200, 300));
}

// ── text_extract / text_find ─────────────────────────────────────────────────

#[test]
fn text_extract_returns_page_text() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("text");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let r = ok(&mut auto, "text_extract", json!({ "doc": doc }));
    let pages = r["pages"].as_array().expect("pages");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["page"], 1);
    assert_eq!(pages[0]["source"], "text");
    assert!(pages[0]["text"].as_str().unwrap().contains("Test Fixture"), "{r}");
}

/// A page with no text layer falls back to this session's OCR words, and says
/// so; with none, it is reported as empty rather than skipped.
#[test]
fn text_extract_falls_back_to_cached_ocr() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("text");
    let mut auto = session(pdfium.get());
    let blank = dir.file("blank.pdf", &crate::geometry_page_bytes(300.0, 200.0, 0, None));
    let doc = open(&mut auto, &blank);
    let r = ok(&mut auto, "text_extract", json!({ "doc": doc, "pages": [1] }));
    assert_eq!(r["pages"][0], json!({ "page": 1, "text": "", "source": "none" }));

    auto.state.set_ocr_words(&doc.to_string(), 1, vec![word("Scanned", 10.0, 150.0)]);
    let r = ok(&mut auto, "text_extract", json!({ "doc": doc }));
    assert_eq!(r["pages"][0]["source"], "ocr");
    assert_eq!(r["pages"][0]["text"], "Scanned");
}

/// A force-re-OCR'd page (issue #97) serves its OCR words over the native
/// layer, as search and the GUI do.
#[test]
fn text_extract_prefers_ocr_on_force_reocrd_pages() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("text");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    auto.state.set_ocr_words(&doc.to_string(), 1, vec![word("Replaced", 10.0, 150.0)]);
    auto.state.set_ocr_override(&doc.to_string(), 1);
    let r = ok(&mut auto, "text_extract", json!({ "doc": doc }));
    assert_eq!((r["pages"][0]["source"].clone(), r["pages"][0]["text"].clone()), (json!("ocr"), json!("Replaced")));
}

#[test]
fn text_extract_validates_pages() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("text");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    invalid(&mut auto, "text_extract", json!({ "doc": doc, "pages": [2] }));
    invalid(&mut auto, "text_extract", json!({ "doc": doc, "pages": [] }));
}

#[test]
fn text_find_reports_matches_with_render_space_rects() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("find");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let r = ok(&mut auto, "text_find", json!({ "doc": doc, "query": "fixture" }));
    assert_eq!(r["total"], 1);
    let m = &r["results"][0];
    assert_eq!(m["page"], 1);
    let rect = &m["matches"][0]["rects"][0];
    // The fixture's text sits near the top-left: a small y in render space.
    let (x, y) = (rect["x"].as_f64().unwrap(), rect["y"].as_f64().unwrap());
    assert!((0.0..200.0).contains(&x) && (0.0..100.0).contains(&y), "{rect}");

    assert_eq!(ok(&mut auto, "text_find", json!({ "doc": doc, "query": "fixture", "match_case": true }))["total"], 0);
    assert_eq!(ok(&mut auto, "text_find", json!({ "doc": doc, "query": "Fix\\w+", "regex": true }))["total"], 1);
    assert_eq!(ok(&mut auto, "text_find", json!({ "doc": doc, "query": "Fix", "whole_word": true }))["total"], 0);
    invalid(&mut auto, "text_find", json!({ "doc": doc, "query": "" }));
}

// ── sign_list / doc_conformance ──────────────────────────────────────────────

#[test]
fn sign_list_and_conformance_on_an_unsigned_plain_file() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("sig");
    let mut auto = session(pdfium.get());
    let doc = open(&mut auto, &fixture_copy(&dir, "a.pdf"));
    let r = ok(&mut auto, "sign_list", json!({ "doc": doc }));
    assert_eq!(r["count"], 0);
    assert_eq!(r["signatures"], json!([]));
    assert!(r.get("status").is_some(), "{r}");
    assert_eq!(ok(&mut auto, "doc_conformance", json!({ "doc": doc })), json!({ "declared": [] }));
}

/// The bounds written on the argument structs reach the schema agents see,
/// optional arguments are left out of `required`, and nothing is typed as
/// `T | null`.
#[test]
fn schemas_carry_bounds_and_optional_arguments() {
    let render = &tools::find("page_render").unwrap().input_schema;
    assert_eq!(render["properties"]["dpi"]["type"], "number");
    assert_eq!(render["properties"]["dpi"]["minimum"], 1.0);
    assert_eq!(render["properties"]["dpi"]["maximum"], 600.0);
    assert_eq!(render["properties"]["page"]["minimum"], 1);
    assert_eq!(render["required"], json!(["doc", "page"]));
    assert!(render["properties"]["dpi"].get("default").is_none(), "no default: null");
    // doc_save's path has a doc comment wrapped over two lines.
    let save = &tools::find("doc_save").unwrap().input_schema;
    let path = save["properties"]["path"]["description"].as_str().unwrap();
    assert!(path.contains("Omit to overwrite"), "{path:?}");
    assert!(!path.contains('\n'), "wrapped doc comment kept its line break: {path:?}");

    let extract = &tools::find("text_extract").unwrap().input_schema;
    assert_eq!(extract["properties"]["pages"]["type"], "array");
    assert_eq!(extract["properties"]["pages"]["minItems"], 1);
    assert_eq!(extract["properties"]["pages"]["items"]["minimum"], 1);

    assert_eq!(tools::find("doc_list").unwrap().input_schema["properties"], json!({}));
}

/// A type error names the argument, not a serde position.
#[test]
fn type_errors_name_the_argument() {
    let pdfium = crate::test_pdfium();
    let mut auto = session(pdfium.get());
    let m = invalid(&mut auto, "doc_info", json!({ "doc": "one" }));
    assert!(m.contains("`doc`") && m.contains("integer"), "{m}");
    let m = invalid(&mut auto, "text_extract", json!({ "doc": 1, "pages": [1, 0] }));
    assert!(m.contains("`pages`"), "{m}");
}

//! Vector 2 — rendered-image OCR (`RenderedOcr`), the opt-in pixel pass.
//!
//! Every other vector reads text or structure the file *declares*. This one
//! renders each page to a bitmap and runs OCR over the pixels, so a secret that
//! survives only as an *image of text* — a scan, a flattened redaction box that
//! left the words underneath, glyphs drawn by a Type3 font or tiling pattern —
//! is still recovered (spec §4-A/§4-K, §14.8). Findings are tagged `page N
//! (OCR)`.
//!
//! It is **opt-in and feature-gated**. The portable default build omits the
//! `ocr` Cargo feature, so this stays a `NotImplemented` stub and the crate
//! never links Tumbler or the Windows OCR APIs. A build compiled with
//! `--features ocr` links `tumbler_lib`'s OCR seam (`OcrEngine`,
//! `WindowsOcrEngine`, `render_page_for_ocr`) — the one place Subtext calls real
//! Tumbler code (§6.1) — and the four-state skip logic of §14.2 applies:
//!
//! | build / flags                         | outcome           |
//! |---------------------------------------|-------------------|
//! | `ocr` not compiled (default)          | `NotImplemented`  |
//! | `ocr` compiled, `--ocr` not passed    | `NotRequested`    |
//! | `ocr` compiled, `--ocr`, no lang pack | `Unavailable`     |
//! | `ocr` compiled, `--ocr`, engine ready | **runs**          |

use crate::extract::{CheckOutcome, DocContext, VectorCheck};
use crate::query::Query;
use crate::report::Vector;

#[cfg(feature = "ocr")]
use crate::extract::findings_in;
#[cfg(feature = "ocr")]
use crate::report::Finding;
#[cfg(feature = "ocr")]
use pdfium_render::prelude::PdfDocument;
#[cfg(feature = "ocr")]
use tumbler_lib::ocr_api::{render_page_for_ocr, OcrEngine, OcrWord};

pub struct RenderedOcr;

impl VectorCheck for RenderedOcr {
    fn id(&self) -> &'static str {
        "rendered_ocr"
    }
    fn label(&self) -> &'static str {
        "Rendered-image OCR"
    }
    fn vector(&self) -> Vector {
        Vector::RenderedOcr
    }
    fn method(&self) -> &'static str {
        "render each page + OCR engine (feature \"ocr\")"
    }

    /// Portable build: the pass isn't compiled in. Honest `NotImplemented`
    /// (scored low), never a false clean.
    #[cfg(not(feature = "ocr"))]
    fn run(&self, _ctx: &DocContext, _query: &Query) -> CheckOutcome {
        CheckOutcome::not_implemented("this build has no OCR support (compile with --features ocr)")
    }

    /// `ocr` build: opt-out → `NotRequested`; requested but no pdfium view →
    /// `Unavailable`; requested with a view → render + recognize (§14.2).
    #[cfg(feature = "ocr")]
    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        if !ctx.ocr_requested {
            return CheckOutcome::not_requested("OCR pass not run; pass --ocr");
        }
        let Some(doc) = ctx.pdfium else {
            return ctx.pdfium_unavailable();
        };
        let engine = tumbler_lib::ocr_api::WindowsOcrEngine::new();
        run_ocr(doc, &engine, query)
    }
}

/// Renders every page and OCRs it, running `query` over the recognized text.
/// Engine-injected so it can be exercised with a fake (the `WindowsOcrEngine`
/// needs a language pack the CI box lacks). If the engine fails on *every* page
/// it attempted — the no-language-pack case — the whole pass is `Unavailable`
/// (a disclosed blind spot, not a silent empty run); a page that merely fails
/// to render is skipped, since that is not a query blind spot.
#[cfg(feature = "ocr")]
fn run_ocr(doc: &PdfDocument, engine: &dyn OcrEngine, query: &Query) -> CheckOutcome {
    let page_count = doc.pages().len() as u32;
    let mut findings: Vec<Finding> = Vec::new();
    let mut attempted = 0u32;
    let mut errored = 0u32;
    let mut last_err = String::new();

    for page in 1..=page_count {
        let Ok((rgba, w, h, _pw, _ph)) = render_page_for_ocr(doc, page) else {
            continue;
        };
        attempted += 1;
        match engine.recognize(&rgba, w, h) {
            Ok(words) => findings_from_words(&words, page, query, &mut findings),
            Err(e) => {
                errored += 1;
                last_err = e.to_string();
            }
        }
    }

    if attempted > 0 && errored == attempted {
        return CheckOutcome::unavailable(format!("OCR engine unavailable — {last_err}"));
    }
    CheckOutcome::ran(findings)
}

/// Joins one page's recognized words into reading-ordered text and runs the
/// query over it. Words are space-joined so a term split across recognized
/// words (`"Zan" "zibar"`) does not reassemble into a false positive, while a
/// term the engine read as one word matches normally.
#[cfg(feature = "ocr")]
fn findings_from_words(words: &[OcrWord], page: u32, query: &Query, out: &mut Vec<Finding>) {
    let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    findings_in(
        &text,
        query,
        Vector::RenderedOcr,
        &format!("page {page} (OCR)"),
        Some(page),
        out,
    );
}

/// Portable-build behavior: the stub reports `NotImplemented` (§14.2) whatever
/// the flags, so the vector is disclosed as "not built", never clean.
#[cfg(all(test, not(feature = "ocr")))]
mod stub_tests {
    use super::*;
    use crate::report::SkipKind;

    #[test]
    fn stub_reports_not_implemented() {
        let ctx = DocContext::new(&[], None, None);
        let q = Query::literal(["x".to_string()], false, false).unwrap();
        match RenderedOcr.run(&ctx, &q) {
            CheckOutcome::Skipped { kind, .. } => assert_eq!(kind, SkipKind::NotImplemented),
            CheckOutcome::Ran { .. } => panic!("portable build must not run OCR"),
        }
    }
}

#[cfg(all(test, feature = "ocr"))]
mod tests {
    use super::*;
    use crate::report::SkipKind;
    use pdfium_render::prelude::Pdfium;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use tumbler_lib::ocr_api::{AppError, TextRect};

    fn word(text: &str) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 },
        }
    }

    /// An injected fake standing in for `WindowsOcrEngine` (which needs a
    /// language pack the test box lacks): returns canned words, or errors on
    /// every page to exercise the no-engine path.
    struct FakeEngine {
        words: Vec<OcrWord>,
        fail: bool,
    }

    impl OcrEngine for FakeEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            if self.fail {
                Err(AppError::Other("no OCR language pack installed".to_string()))
            } else {
                Ok(self.words.clone())
            }
        }
    }

    fn pdfium() -> &'static Pdfium {
        static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
        PDFIUM.get_or_init(|| {
            let dll = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("src-tauri")
                .join("resources")
                .join(Pdfium::pdfium_platform_library_name_at_path("./"));
            Pdfium::new(Pdfium::bind_to_library(&dll).expect("bind pdfium"))
        })
    }

    fn sample_doc() -> pdfium_render::prelude::PdfDocument<'static> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("src-tauri")
            .join("tests")
            .join("fixtures")
            .join("sample.pdf");
        let bytes = std::fs::read(&path).expect("read sample.pdf");
        pdfium().load_pdf_from_byte_vec(bytes, None).expect("load sample.pdf")
    }

    #[test]
    fn matches_a_term_across_recognized_words() {
        let words = [word("the"), word("Zanzibar"), word("archive")];
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let mut out = Vec::new();
        findings_from_words(&words, 4, &q, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].matched_text, "Zanzibar");
        assert_eq!(out[0].location, "page 4 (OCR)");
        assert_eq!(out[0].page, Some(4));
    }

    #[test]
    fn space_join_does_not_fuse_adjacent_words_into_a_false_positive() {
        // Two separately-recognized words must not concatenate into the secret.
        let words = [word("Zan"), word("zibar")];
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let mut out = Vec::new();
        findings_from_words(&words, 1, &q, &mut out);
        assert!(out.is_empty(), "adjacent words must not fuse: {out:?}");
    }

    #[test]
    fn not_requested_when_ocr_flag_absent() {
        let ctx = DocContext::new(&[], None, None); // ocr_requested defaults false
        let q = Query::literal(["x".to_string()], false, false).unwrap();
        match RenderedOcr.run(&ctx, &q) {
            CheckOutcome::Skipped { kind, reason } => {
                assert_eq!(kind, SkipKind::NotRequested);
                assert!(reason.contains("--ocr"), "{reason}");
            }
            CheckOutcome::Ran { .. } => panic!("must not run without --ocr"),
        }
    }

    #[test]
    fn unavailable_when_requested_but_no_pdfium_view() {
        let mut ctx = DocContext::new(&[], None, None);
        ctx.ocr_requested = true; // requested, but pdfium view is None
        let q = Query::literal(["x".to_string()], false, false).unwrap();
        match RenderedOcr.run(&ctx, &q) {
            CheckOutcome::Skipped { kind, .. } => assert_eq!(kind, SkipKind::Unavailable),
            CheckOutcome::Ran { .. } => panic!("no pdfium view must be Unavailable"),
        }
    }

    #[test]
    fn run_ocr_with_a_working_engine_finds_the_secret() {
        // The fake ignores the rendered pixels and returns the secret, proving
        // the render→recognize→match path produces a `page N (OCR)` finding.
        let doc = sample_doc();
        let engine = FakeEngine { words: vec![word("Zanzibar")], fail: false };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        match run_ocr(&doc, &engine, &q) {
            CheckOutcome::Ran { findings, .. } => {
                assert!(
                    findings.iter().any(|f| f.matched_text == "Zanzibar" && f.location == "page 1 (OCR)"),
                    "{findings:?}"
                );
            }
            CheckOutcome::Skipped { reason, .. } => panic!("skip: {reason}"),
        }
    }

    #[test]
    fn run_ocr_reports_unavailable_when_the_engine_fails_on_every_page() {
        let doc = sample_doc();
        let engine = FakeEngine { words: vec![], fail: true };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        match run_ocr(&doc, &engine, &q) {
            CheckOutcome::Skipped { kind, .. } => assert_eq!(kind, SkipKind::Unavailable),
            CheckOutcome::Ran { .. } => panic!("an engine that fails everywhere must be Unavailable"),
        }
    }
}

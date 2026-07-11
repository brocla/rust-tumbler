//! The pen-test corpus test suite (spec §8).
//!
//! The 13 adversarial PDFs in `src-tauri/tests/fixtures/redaction/pentest/`
//! each hide the secret word `Zanzibar` via a different technique. They were
//! built to prove Tumbler *removes* the secret; for Subtext they invert — each
//! attack file must eventually yield ≥ 1 finding, and a clean control must
//! yield 0.
//!
//! Phase 1 implements only the page-text extractor, so this suite asserts:
//!   - the 11 page-text-reachable attacks are detected NOW;
//!   - a clean control yields 0 findings;
//!   - the 2 attacks whose secret lives outside page content
//!     (`annotation-appearance` → Phase 2 annotations; `incremental-update-cover`
//!     → Phase 3 revisions) are NOT yet detected — pinning the current boundary
//!     so it visibly flips when those extractors land.
//!
//! pdfium can be bound only once per process, so the binding is shared via a
//! `OnceLock`. Run with `--test-threads=1`: pdfium-render's `thread_safe`
//! feature serializes individual calls, but concurrent document teardown can
//! still crash the process at exit (STATUS_HEAP_CORRUPTION) — the same known
//! behavior as Tumbler's backend tests.

use std::path::PathBuf;
use std::sync::OnceLock;

use pdfium_render::prelude::Pdfium;
use subtext::report::RiskTone;
use subtext::Query;

const SECRET: &str = "Zanzibar";

/// Every attack file page-text extraction reaches in Phase 1: the secret is in
/// the page content stream (even if visually hidden), so pdfium extracts it.
const PAGE_TEXT_DETECTABLE: &[&str] = &[
    "hidden-text-black-box",
    "invisible-render-mode",
    "tiny-white-text",
    "offpage-text",
    "form-xobject",
    "tounicode-spoof",
    "masked-image-cover",
    "optional-content-hidden",
    "embedded-and-document-vectors", // page 1 carries visible "Zanzibar visible"
    "corrupted-xref",                // pdfium recovers and renders the secret
];

/// Attacks whose secret is NOT in current-revision page content, so page-text
/// extraction alone cannot see them yet. Each names the phase that will detect it.
const PHASE1_PENDING: &[(&str, &str)] = &[
    ("annotation-appearance", "Phase 2 — annotations (/AP, /Contents)"),
    ("incremental-update-cover", "Phase 3 — superseded revisions"),
    // Its on-page secrets are all in revision 1, which a second incremental
    // revision *covers* with innocuous text; the doc-level copies need the
    // Phase 2 extractors and the on-page ones the Phase 3 revision pass.
    ("all-vectors-combined", "Phase 2 (doc-level) / Phase 3 (revisions)"),
];

fn pdfium() -> &'static Pdfium {
    static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
    PDFIUM.get_or_init(|| {
        let dll = manifest_relative(&["..", "..", "src-tauri", "resources"]).join(
            Pdfium::pdfium_platform_library_name_at_path("./"),
        );
        let bindings = Pdfium::bind_to_library(&dll)
            .unwrap_or_else(|e| panic!("bind pdfium at {}: {e}", dll.display()));
        Pdfium::new(bindings)
    })
}

fn manifest_relative(parts: &[&str]) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for part in parts {
        p.push(part);
    }
    p
}

fn pentest_bytes(name: &str) -> Vec<u8> {
    let path = manifest_relative(&["..", "..", "src-tauri", "tests", "fixtures", "redaction", "pentest"])
        .join(format!("{name}.pdf"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn zanzibar_query() -> Query {
    Query::literal([SECRET.to_string()], false, false).expect("build query")
}

#[test]
fn page_text_attacks_are_detected() {
    let query = zanzibar_query();
    for name in PAGE_TEXT_DETECTABLE {
        let bytes = pentest_bytes(name);
        let report = subtext::check_pdf(pdfium(), &bytes, &format!("{name}.pdf"), &query);
        assert!(
            !report.findings.is_empty(),
            "[{name}] expected ≥1 finding for '{SECRET}', got none"
        );
        assert_eq!(
            report.risk_tone,
            RiskTone::Leak,
            "[{name}] a detected secret must be a Leak"
        );
        // Every finding in Phase 1 comes from the page-text vector.
        assert!(
            report.findings.iter().any(|f| f.matched_text.eq_ignore_ascii_case(SECRET)),
            "[{name}] no finding matched the secret text"
        );
    }
}

#[test]
fn clean_control_yields_no_findings() {
    // sample.pdf ("Test Fixture") contains no `Zanzibar` — the false-positive
    // gate. It is not in the pentest dir; use Tumbler's checked-in fixture.
    let path = manifest_relative(&["..", "..", "src-tauri", "tests", "fixtures"]).join("sample.pdf");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let report = subtext::check_pdf(pdfium(), &bytes, "sample.pdf", &zanzibar_query());
    assert!(
        report.findings.is_empty(),
        "clean control must yield no findings, got {:?}",
        report.findings
    );
    assert_ne!(
        report.risk_tone,
        RiskTone::Leak,
        "clean control must not be a Leak"
    );
}

#[test]
fn phase1_pending_attacks_not_yet_detected() {
    // Pins the current boundary: these need Phase 2/3 extractors. When one lands,
    // this assertion flips — a visible, intentional signal to update the suite.
    let query = zanzibar_query();
    for (name, phase) in PHASE1_PENDING {
        let bytes = pentest_bytes(name);
        let report = subtext::check_pdf(pdfium(), &bytes, &format!("{name}.pdf"), &query);
        assert!(
            report.findings.is_empty(),
            "[{name}] unexpectedly detected in Phase 1 — this attack was expected to need {phase}. \
             If a new extractor now catches it, move it to PAGE_TEXT_DETECTABLE."
        );
    }
}

#[test]
fn every_check_appears_in_the_report() {
    // The honesty guarantee: the checks list is generated from the registry, so
    // all 21 vectors are always present (spec §1, §4.1).
    let bytes = pentest_bytes("hidden-text-black-box");
    let report = subtext::check_pdf(pdfium(), &bytes, "x.pdf", &zanzibar_query());
    assert_eq!(report.checks.len(), 21, "all registered vectors must be listed");
}

//! The pen-test corpus test suite (spec §8).
//!
//! The 13 adversarial PDFs in `src-tauri/tests/fixtures/redaction/pentest/`
//! each hide the secret word `Zanzibar` via a different technique. They were
//! built to prove Tumbler *removes* the secret; for Subtext they invert — each
//! attack file must eventually yield ≥ 1 finding, and a clean control must
//! yield 0.
//!
//! After Phase 2 (the document-level extractors) this suite asserts:
//!   - 12 of the 13 attacks are detected NOW — the page-text-reachable ones plus
//!     the document-level ones (annotation, metadata, structure, outlines, form/
//!     XFA, attachments, scripts, OCG);
//!   - a clean control yields 0 findings;
//!   - only `incremental-update-cover` is NOT yet detected — its sole secret
//!     lives in a superseded revision the covering update hides, which needs the
//!     Phase 3 revision pass. Pinning it flips visibly when that lands.
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

/// Every attack detected by Phase 1 (page text) or Phase 2 (document-level
/// vectors). `all-vectors-combined` is here because its document-level vectors
/// (metadata/struct/outlines/form/XFA/attachments/scripts/OCG) remain reachable
/// in the newest revision — only its *on-page* copies are hidden under the
/// covering update — so the doc-level extractors still find it.
const MUST_DETECT: &[&str] = &[
    // Page-text reachable (Phase 1).
    "hidden-text-black-box",
    "invisible-render-mode",
    "tiny-white-text",
    "offpage-text",
    "form-xobject",
    "tounicode-spoof",
    "masked-image-cover",
    "optional-content-hidden",
    "embedded-and-document-vectors",
    "corrupted-xref",
    // Document-level reachable (Phase 2).
    "annotation-appearance", // /AP appearance stream + /Contents
    "all-vectors-combined",  // doc-level vectors survive in the newest revision
    // Superseded-revision reachable (Phase 3).
    "incremental-update-cover", // secret lives only in the prior revision
];

/// Every corpus attack is now detected; the PENDING set is empty.
const PENDING: &[(&str, &str)] = &[];

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
fn attacks_are_detected() {
    let query = zanzibar_query();
    for name in MUST_DETECT {
        let bytes = pentest_bytes(name);
        let report = subtext::check_pdf(pdfium(), &bytes, &format!("{name}.pdf"), &query, &Default::default());
        assert!(
            !report.findings.is_empty(),
            "[{name}] expected ≥1 finding for '{SECRET}', got none"
        );
        assert_eq!(
            report.risk_tone,
            RiskTone::Leak,
            "[{name}] a detected secret must be a Leak"
        );
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
    let report = subtext::check_pdf(pdfium(), &bytes, "sample.pdf", &zanzibar_query(), &Default::default());
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
fn pending_attacks_not_yet_detected() {
    // Pins the current boundary: this needs the Phase 3 revision pass. When it
    // lands, this assertion flips — a visible, intentional signal to update the
    // suite (move the entry to MUST_DETECT).
    let query = zanzibar_query();
    for (name, phase) in PENDING {
        let bytes = pentest_bytes(name);
        let report = subtext::check_pdf(pdfium(), &bytes, &format!("{name}.pdf"), &query, &Default::default());
        assert!(
            report.findings.is_empty(),
            "[{name}] unexpectedly detected — this attack was expected to need {phase}. \
             If a new extractor now catches it, move it to MUST_DETECT."
        );
    }
}

/// The user-password-protected copy of `sample.pdf` (AESv3/256-bit) checked in
/// for Tumbler's encrypted-open tests. Its user password is `open-sesame`.
fn encrypted_fixture_bytes() -> Vec<u8> {
    let path = manifest_relative(&["..", "..", "src-tauri", "tests", "fixtures"])
        .join("sample-encrypted.pdf");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn fixture_query() -> Query {
    // sample.pdf's page text is "Test Fixture".
    Query::literal(["Fixture".to_string()], false, false).expect("build query")
}

#[test]
fn encrypted_without_password_is_a_warning_not_a_failure() {
    // §14.9: no password → content vectors report Skipped(Unavailable,
    // "encrypted — supply --password"); never a hard failure, never a
    // false-clean.
    let bytes = encrypted_fixture_bytes();
    let report = subtext::check_pdf(
        pdfium(),
        &bytes,
        "sample-encrypted.pdf",
        &fixture_query(),
        &Default::default(),
    );
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    assert_eq!(report.risk_tone, RiskTone::Warning);
    assert!(
        report.checks.iter().any(|c| c.detail.contains("--password")),
        "a skip should tell the user to supply --password: {:#?}",
        report.checks.iter().map(|c| &c.detail).collect::<Vec<_>>()
    );
}

#[test]
fn encrypted_with_password_finds_the_page_text() {
    let bytes = encrypted_fixture_bytes();
    let options = subtext::CheckOptions {
        password: Some("open-sesame".to_string()),
        ..Default::default()
    };
    let report = subtext::check_pdf(
        pdfium(),
        &bytes,
        "sample-encrypted.pdf",
        &fixture_query(),
        &options,
    );
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.matched_text.eq_ignore_ascii_case("Fixture")),
        "expected the page text hit through the decrypted views: {:?}",
        report.findings
    );
    // The raw-byte passes stay honestly skipped: the bytes are ciphertext
    // even though the parsed views were decrypted.
    assert!(
        report.checks.iter().any(|c| c.detail.contains("ciphertext")),
        "revision/orphan scans over an encrypted file must disclose the blind spot"
    );
}

#[test]
fn wrong_password_reports_the_password_did_not_unlock() {
    let bytes = encrypted_fixture_bytes();
    let options = subtext::CheckOptions {
        password: Some("wrong".to_string()),
        ..Default::default()
    };
    let report = subtext::check_pdf(
        pdfium(),
        &bytes,
        "sample-encrypted.pdf",
        &fixture_query(),
        &options,
    );
    assert!(report.findings.is_empty());
    assert!(
        report.checks.iter().any(|c| c.detail.contains("did not unlock")),
        "{:#?}",
        report.checks.iter().map(|c| &c.detail).collect::<Vec<_>>()
    );
}

#[test]
fn every_check_appears_in_the_report() {
    // The honesty guarantee: the checks list is generated from the registry, so
    // all 21 vectors are always present (spec §1, §4.1).
    let bytes = pentest_bytes("hidden-text-black-box");
    let report = subtext::check_pdf(pdfium(), &bytes, "x.pdf", &zanzibar_query(), &Default::default());
    assert_eq!(report.checks.len(), 21, "all registered vectors must be listed");
}

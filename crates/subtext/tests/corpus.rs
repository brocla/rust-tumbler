//! The pen-test corpus test suite (spec §8): the 13 adversarial PDFs in
//! `src-tauri/tests/fixtures/redaction/pentest/` each hide the secret word
//! `Zanzibar`; each attack file must yield ≥ 1 finding, and a clean control
//! must yield 0. Wired up once `check_pdf` exists (Phase 1 skeleton).

#[test]
fn scaffold_compiles() {
    // Placeholder so the scaffold commit has a green `cargo test`.
}

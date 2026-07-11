//! Vector 14 — attachments: `/Names/EmbeddedFiles` and PDF 2.0 Associated Files
//! (`/AF`, catalog *and* page level). A filespec's `/F` `/UF` `/Desc` and the
//! embedded stream contents can all carry redacted text (spec §4-F). Inverts
//! Tumbler's `/EmbeddedFiles` + `/AF` scrub. Embedded stream bytes are scanned
//! as text (recursion into embedded *PDFs* is `--recurse-embedded`, Phase 3+).

use crate::extract::{findings_in, scan_dict_keys, CheckOutcome, DocContext, VectorCheck};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Vector};
use lopdf::{Dictionary, Document};

pub struct Attachments;

const FILESPEC_KEYS: &[&[u8]] = &[b"F", b"UF", b"Desc"];

impl VectorCheck for Attachments {
    fn id(&self) -> &'static str {
        "attachments"
    }
    fn label(&self) -> &'static str {
        "Attachments"
    }
    fn vector(&self) -> Vector {
        Vector::Attachments
    }
    fn method(&self) -> &'static str {
        "/EmbeddedFiles + /AF filespecs + stream bytes"
    }

    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome {
        let Some(doc) = ctx.lopdf else {
            return CheckOutcome::unavailable("lopdf could not parse this document");
        };
        let mut findings = Vec::new();
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::unavailable("document catalog could not be read");
        };

        // /Names /EmbeddedFiles name tree → filespecs.
        if let Some(names) = pdf::get_dict(doc, catalog, b"Names") {
            if let Some(ef) = pdf::get_dict(doc, names, b"EmbeddedFiles") {
                pdf::walk_name_tree(doc, ef, |_name, value| {
                    if let Ok(fs) = value.as_dict() {
                        scan_filespec(doc, fs, query, "EmbeddedFiles", &mut findings);
                    }
                });
            }
        }

        // Catalog-level /AF associated files.
        scan_af_array(doc, catalog, query, "catalog /AF", &mut findings);

        // Page-level /AF associated files.
        for (page_id, page_num) in pdf::page_numbers(doc) {
            if let Ok(page) = doc.get_dictionary(page_id) {
                scan_af_array(doc, page, query, &format!("page {page_num} /AF"), &mut findings);
            }
        }

        CheckOutcome::ran(findings)
    }
}

fn scan_af_array(doc: &Document, dict: &Dictionary, query: &Query, where_: &str, findings: &mut Vec<Finding>) {
    if let Some(af) = pdf::get_array(doc, dict, b"AF") {
        for entry in af {
            if let Some(fs) = pdf::resolve(doc, entry).and_then(|o| o.as_dict().ok()) {
                scan_filespec(doc, fs, query, where_, findings);
            }
        }
    }
}

/// Scans one filespec: its `/F` `/UF` `/Desc` strings and the bytes of its
/// embedded file stream(s) under `/EF`.
fn scan_filespec(doc: &Document, fs: &Dictionary, query: &Query, where_: &str, findings: &mut Vec<Finding>) {
    scan_dict_keys(doc, fs, FILESPEC_KEYS, query, Vector::Attachments, None, |k| format!("{where_} filespec /{k}"), findings);
    if let Some(ef) = pdf::get_dict(doc, fs, b"EF") {
        for stream_key in [b"F".as_slice(), b"UF"] {
            if let Some(id) = ef.get(stream_key).ok().and_then(|o| o.as_reference().ok()) {
                if let Some(bytes) = pdf::stream_bytes(doc, id) {
                    // BOM-aware decode so a UTF-16 embedded text file is scanned,
                    // not turned into NUL-interleaved noise by a lossy UTF-8 read.
                    let text = pdf::decode_pdf_text(&bytes);
                    findings_in(&text, query, Vector::Attachments, &format!("{where_} embedded file contents"), None, findings);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::CheckOutcome;
    use lopdf::{dictionary, Object, Stream};

    #[test]
    fn finds_secret_in_embedded_stream() {
        let mut doc = Document::with_version("1.5");
        let file_stream = doc.add_object(Stream::new(dictionary! { "Type" => "EmbeddedFile" }, b"Zanzibar embedded payload".to_vec()));
        let filespec = doc.add_object(dictionary! {
            "Type" => "Filespec", "F" => Object::string_literal("leak.txt"),
            "EF" => dictionary! { "F" => Object::Reference(file_stream) },
        });
        let names = doc.add_object(dictionary! {
            "EmbeddedFiles" => dictionary! { "Names" => vec![Object::string_literal("leak.txt"), Object::Reference(filespec)] },
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Names" => Object::Reference(names) });
        doc.trailer.set("Root", catalog);

        let ctx = DocContext { bytes: &[], lopdf: Some(&doc), pdfium: None };
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Attachments.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert!(f.iter().any(|x| x.location.contains("embedded file contents")), "{f:?}");
    }
}

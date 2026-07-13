//! Vector 14 — attachments: `/Names/EmbeddedFiles` and PDF 2.0 Associated Files
//! (`/AF`, catalog *and* page level). A filespec's `/F` `/UF` `/Desc` and the
//! embedded stream contents can all carry redacted text (spec §4-F). Inverts
//! Tumbler's `/EmbeddedFiles` + `/AF` scrub. Embedded stream bytes are scanned
//! as text; an embedded stream that is itself a PDF is additionally re-scanned
//! with the full non-recursive vector set under `--recurse-embedded` (§14.10),
//! depth-capped and cycle-guarded. With recursion off, embedded PDFs are
//! disclosed as a `NotRequested` skip so declined coverage is never silent.

use crate::extract::{
    findings_in, load_lopdf_view, non_recursive_checks, run_checks, scan_dict_keys,
    stamp_container, sub_document_signal, CheckOutcome, DocContext, Recurse, VectorCheck,
};
use crate::pdf;
use crate::query::Query;
use crate::report::{Finding, Signal, Vector};
use lopdf::{Dictionary, Document};
use std::hash::{Hash, Hasher};

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
            return ctx.lopdf_unavailable();
        };
        let mut findings = Vec::new();
        let mut embedded_pdfs: Vec<(String, Vec<u8>)> = Vec::new();
        let Some(catalog) = pdf::catalog(doc) else {
            return CheckOutcome::unavailable("document catalog could not be read");
        };

        // /Names /EmbeddedFiles name tree → filespecs.
        if let Some(names) = pdf::get_dict(doc, catalog, b"Names") {
            if let Some(ef) = pdf::get_dict(doc, names, b"EmbeddedFiles") {
                pdf::walk_name_tree(doc, ef, |_name, value| {
                    if let Ok(fs) = value.as_dict() {
                        scan_filespec(doc, fs, query, "EmbeddedFiles", &mut findings, &mut embedded_pdfs);
                    }
                });
            }
        }

        // Catalog-level /AF associated files.
        scan_af_array(doc, catalog, query, "catalog /AF", &mut findings, &mut embedded_pdfs);

        // Page-level /AF associated files.
        for (page_id, page_num) in pdf::page_numbers(doc) {
            if let Ok(page) = doc.get_dictionary(page_id) {
                scan_af_array(doc, page, query, &format!("page {page_num} /AF"), &mut findings, &mut embedded_pdfs);
            }
        }

        // A filespec's /EF /F and /UF may alias the same payload — count and
        // recurse each distinct payload once.
        let mut seen = std::collections::HashSet::new();
        embedded_pdfs.retain(|(_, b)| seen.insert(bytes_hash(b)));

        let mut signals: Vec<Signal> = Vec::new();
        match ctx.recurse {
            Some(rec) => recurse_embedded(ctx, rec, &embedded_pdfs, query, &mut findings, &mut signals),
            // Recursion declined but there are embedded PDFs: disclose the
            // declined coverage (§14.10). Findings take precedence — a found
            // leak must never be traded for a skip note.
            None if !embedded_pdfs.is_empty() && findings.is_empty() => {
                return CheckOutcome::not_requested(format!(
                    "{} embedded PDF(s) not recursed; pass --recurse-embedded",
                    embedded_pdfs.len()
                ));
            }
            None => {}
        }

        CheckOutcome::Ran { findings, signals }
    }
}

/// Runs the non-recursive vector set over each embedded PDF (§14.10), stamping
/// every finding with `attachment:<name>`. Depth-capped at
/// [`Recurse::DEPTH_CAP`] and cycle-guarded by a hash of the embedded bytes,
/// so cyclic or duplicated attachments are scanned once. The sub-scan's signals
/// and blind-spot disclosures are carried up (tagged with the container path),
/// so a suspicion or an unreadable attachment isn't silently dropped (§14.9).
fn recurse_embedded(
    ctx: &DocContext,
    rec: Recurse,
    embedded_pdfs: &[(String, Vec<u8>)],
    query: &Query,
    findings: &mut Vec<Finding>,
    signals: &mut Vec<Signal>,
) {
    if rec.depth >= Recurse::DEPTH_CAP {
        return;
    }
    let sub_checks = non_recursive_checks();
    for (name, bytes) in embedded_pdfs {
        if !rec.visited.borrow_mut().insert(bytes_hash(bytes)) {
            continue; // already scanned this exact payload (cycle/duplicate)
        }
        let container = format!("attachment:{name}");
        // Load through the shared view loader so an encrypted attachment is
        // discarded (its objects would be ciphertext) rather than scanned and
        // reported clean — the same guard check_pdf applies to the host (§14.9).
        let view = load_lopdf_view(bytes, None);
        let pdfium_doc = ctx
            .pdfium_lib
            .and_then(|p| p.load_pdf_from_byte_vec(bytes.clone(), None).ok());
        let mut sub_ctx = DocContext::new(bytes, view.doc.as_ref(), pdfium_doc.as_ref());
        sub_ctx.pdfium_lib = ctx.pdfium_lib;
        sub_ctx.encrypted = view.encrypted;
        if view.encrypted && sub_ctx.lopdf.is_none() {
            sub_ctx.lopdf_reason = "encrypted embedded PDF — cannot inspect";
        }
        sub_ctx.recurse = Some(Recurse { depth: rec.depth + 1, visited: rec.visited });

        let mut sub = run_checks(&sub_checks, &sub_ctx, query);
        stamp_container(&mut sub.findings, &container);
        findings.append(&mut sub.findings);

        // Carry the sub-scan's own signals up, tagged with the container path.
        for mut sig in sub.signals {
            sig.location = format!("{container} · {}", sig.location);
            signals.push(sig);
        }
        // Disclose an attachment we couldn't fully inspect (encrypted/corrupt),
        // so finalize can't certify the host clean over an unread sub-document.
        if let Some(sig) = sub_document_signal(&container, &sub.checks) {
            signals.push(sig);
        }
    }
}

fn bytes_hash(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// True when an embedded file's decoded bytes are themselves a PDF (the
/// `%PDF-` header, allowed within the first 1024 bytes per Annex C).
fn is_pdf_bytes(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    head.windows(5).any(|w| w == b"%PDF-")
}

fn scan_af_array(
    doc: &Document,
    dict: &Dictionary,
    query: &Query,
    where_: &str,
    findings: &mut Vec<Finding>,
    embedded_pdfs: &mut Vec<(String, Vec<u8>)>,
) {
    if let Some(af) = pdf::get_array(doc, dict, b"AF") {
        for entry in af {
            if let Some(fs) = pdf::resolve(doc, entry).and_then(|o| o.as_dict().ok()) {
                scan_filespec(doc, fs, query, where_, findings, embedded_pdfs);
            }
        }
    }
}

/// Scans one filespec: its `/F` `/UF` `/Desc` strings and the bytes of its
/// embedded file stream(s) under `/EF`. Payloads that are themselves PDFs are
/// collected into `embedded_pdfs` for the `--recurse-embedded` pass.
fn scan_filespec(
    doc: &Document,
    fs: &Dictionary,
    query: &Query,
    where_: &str,
    findings: &mut Vec<Finding>,
    embedded_pdfs: &mut Vec<(String, Vec<u8>)>,
) {
    scan_dict_keys(doc, fs, FILESPEC_KEYS, query, Vector::Attachments, None, |k| format!("{where_} filespec /{k}"), findings);
    if let Some(ef) = pdf::get_dict(doc, fs, b"EF") {
        for stream_key in [b"F".as_slice(), b"UF"] {
            if let Some(id) = ef.get(stream_key).ok().and_then(|o| o.as_reference().ok()) {
                if let Some(bytes) = pdf::stream_bytes(doc, id) {
                    if is_pdf_bytes(&bytes) {
                        embedded_pdfs.push((filespec_name(doc, fs), bytes.clone()));
                    }
                    // Stream-aware decode (UTF-16 BOM → UTF-8 → PDFDocEncoding):
                    // an embedded file is an arbitrary file, usually UTF-8, and
                    // must not be forced through PDFDocEncoding (which would
                    // mangle non-ASCII) nor a lossy UTF-8 read (which would drop
                    // a UTF-16 stream).
                    let text = pdf::decode_stream_text(&bytes);
                    findings_in(&text, query, Vector::Attachments, &format!("{where_} embedded file contents"), None, findings);
                }
            }
        }
    }
}

/// The filespec's display name for the `attachment:<name>` container label —
/// the Unicode `/UF` when present, else `/F`, else a placeholder.
fn filespec_name(doc: &Document, fs: &Dictionary) -> String {
    pdf::get_string(doc, fs, b"UF")
        .or_else(|| pdf::get_string(doc, fs, b"F"))
        .unwrap_or_else(|| "(unnamed)".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
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

        let ctx = DocContext::new(&[], Some(&doc), None);
        let q = Query::literal(["Zanzibar".to_string()], false, false).unwrap();
        let f = match Attachments.run(&ctx, &q) {
            CheckOutcome::Ran { findings, .. } => findings,
            CheckOutcome::Skipped { reason: r, .. } => panic!("skip: {r}"),
        };
        assert!(f.iter().any(|x| x.location.contains("embedded file contents")), "{f:?}");
    }

    /// A minimal standalone PDF hiding `secret` in a Flate-compressed XMP
    /// metadata stream, serialized to bytes — the payload for the
    /// embedded-recursion tests. The compression matters: the secret must be
    /// invisible to the flat text scan of the attachment bytes, so only a real
    /// recursive parse (the Metadata sub-check inflating the stream) finds it.
    pub(crate) fn pdf_bytes_with_xmp_secret(secret: &str) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        // Padding makes the packet compressible — lopdf's `compress()` keeps a
        // stream plain when Flate wouldn't shrink it, and a plaintext secret
        // would defeat the point of these fixtures.
        let pad = "<rdf:li>padding padding padding</rdf:li>".repeat(50);
        let xmp = format!("<x:xmpmeta><dc:title>{secret}</dc:title>{pad}</x:xmpmeta>");
        let mut stream = Stream::new(
            dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
            xmp.into_bytes(),
        );
        stream.compress().expect("flate-compress the xmp stream");
        let meta = doc.add_object(stream);
        let catalog =
            doc.add_object(dictionary! { "Type" => "Catalog", "Metadata" => Object::Reference(meta) });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize inner pdf");
        assert!(
            !out.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "the secret must not appear in the serialized bytes"
        );
        out
    }

    /// A host document with `payload` attached under /Names /EmbeddedFiles as
    /// `name`. `extra` attaches the same payload a second time when true.
    pub(crate) fn host_with_attachment(name: &str, payload: &[u8], duplicate: bool) -> Document {
        let mut doc = Document::with_version("1.5");
        let mut names = Vec::new();
        let n = if duplicate { 2 } else { 1 };
        for i in 0..n {
            let file_stream = doc.add_object(Stream::new(
                dictionary! { "Type" => "EmbeddedFile" },
                payload.to_vec(),
            ));
            let filespec = doc.add_object(dictionary! {
                "Type" => "Filespec", "F" => Object::string_literal(name),
                "EF" => dictionary! { "F" => Object::Reference(file_stream) },
            });
            names.push(Object::string_literal(format!("{name}-{i}")));
            names.push(Object::Reference(filespec));
        }
        let names = doc.add_object(dictionary! {
            "EmbeddedFiles" => dictionary! { "Names" => names },
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Names" => Object::Reference(names) });
        doc.trailer.set("Root", catalog);
        doc
    }

    fn zanzibar() -> Query {
        Query::literal(["Zanzibar".to_string()], false, false).unwrap()
    }

    #[test]
    fn embedded_pdf_without_recursion_is_disclosed_as_not_requested() {
        // The secret lives only inside the embedded PDF's object graph; with
        // recursion off the declined coverage must be disclosed (§14.10).
        let inner = pdf_bytes_with_xmp_secret("Zanzibar");
        let doc = host_with_attachment("inner.pdf", &inner, false);
        let ctx = DocContext::new(&[], Some(&doc), None);
        match Attachments.run(&ctx, &zanzibar()) {
            CheckOutcome::Skipped { reason, kind } => {
                assert_eq!(kind, crate::report::SkipKind::NotRequested);
                assert!(reason.contains("1 embedded PDF"), "{reason}");
                assert!(reason.contains("--recurse-embedded"), "{reason}");
            }
            CheckOutcome::Ran { findings, .. } => {
                panic!("expected a NotRequested skip, ran with {findings:?}")
            }
        }
    }

    fn run_with_recursion(doc: &Document, q: &Query) -> Vec<Finding> {
        run_with_recursion_full(doc, q).0
    }

    fn run_with_recursion_full(doc: &Document, q: &Query) -> (Vec<Finding>, Vec<Signal>) {
        let visited = std::cell::RefCell::new(std::collections::HashSet::new());
        let mut ctx = DocContext::new(&[], Some(doc), None);
        ctx.recurse = Some(Recurse { depth: 0, visited: &visited });
        match Attachments.run(&ctx, q) {
            CheckOutcome::Ran { findings, signals } => (findings, signals),
            CheckOutcome::Skipped { reason, .. } => panic!("skip: {reason}"),
        }
    }

    #[test]
    fn recursion_finds_the_secret_inside_an_embedded_pdf() {
        let inner = pdf_bytes_with_xmp_secret("Zanzibar");
        let doc = host_with_attachment("inner.pdf", &inner, false);
        let f = run_with_recursion(&doc, &zanzibar());
        let hit = f
            .iter()
            .find(|x| x.container.as_deref() == Some("attachment:inner.pdf"))
            .unwrap_or_else(|| panic!("no container-stamped finding: {f:?}"));
        assert_eq!(hit.matched_text, "Zanzibar");
    }

    #[test]
    fn nested_embedded_pdfs_compose_the_container_path() {
        let innermost = pdf_bytes_with_xmp_secret("Zanzibar");
        let mut mid = host_with_attachment("b.pdf", &innermost, false);
        let mut mid_bytes = Vec::new();
        mid.save_to(&mut mid_bytes).expect("serialize mid pdf");
        let doc = host_with_attachment("a.pdf", &mid_bytes, false);
        let f = run_with_recursion(&doc, &zanzibar());
        assert!(
            f.iter().any(|x| x.container.as_deref() == Some("attachment:a.pdf › attachment:b.pdf")),
            "{f:?}"
        );
    }

    #[test]
    fn duplicate_attachments_are_scanned_once() {
        let inner = pdf_bytes_with_xmp_secret("Zanzibar");
        let doc = host_with_attachment("inner.pdf", &inner, true);
        let f = run_with_recursion(&doc, &zanzibar());
        assert_eq!(f.len(), 1, "identical payloads must be deduped: {f:?}");
    }

    #[test]
    fn encrypted_embedded_pdf_is_disclosed_not_scanned_clean() {
        // An encrypted attachment's objects are ciphertext; scanning them and
        // reporting clean is the §14.9 false-clean. The fix discards the view
        // and instead emits a SubDocumentNotInspected signal so the host can't
        // be certified over an unread sub-document.
        let inner = encrypted_looking_pdf_bytes();
        let doc = host_with_attachment("secret.pdf", &inner, false);
        let (findings, signals) = run_with_recursion_full(&doc, &zanzibar());
        assert!(findings.is_empty(), "ciphertext must not yield findings: {findings:?}");
        assert!(
            signals.iter().any(|s| {
                matches!(s.kind, crate::report::SignalKind::SubDocumentNotInspected)
                    && s.location.contains("attachment:secret.pdf")
            }),
            "an uninspectable attachment must be disclosed as a signal: {signals:?}"
        );
    }

    /// A minimal PDF whose trailer declares `/Encrypt`. lopdf either fails to
    /// authenticate (leaving `/Encrypt` in the trailer) or errors outright —
    /// either way the view must be discarded, not scanned as ciphertext.
    fn encrypted_looking_pdf_bytes() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let encrypt = doc.add_object(dictionary! {
            "Filter" => "Standard", "V" => 1, "R" => 2,
            "O" => Object::string_literal(vec![0u8; 32]),
            "U" => Object::string_literal(vec![0u8; 32]),
            "P" => Object::Integer(-44),
        });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", catalog);
        doc.trailer.set("Encrypt", Object::Reference(encrypt));
        doc.trailer.set(
            "ID",
            vec![
                Object::string_literal(vec![0u8; 16]),
                Object::string_literal(vec![0u8; 16]),
            ],
        );
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("serialize encrypted-looking pdf");
        out
    }

    #[test]
    fn redact_signal_inside_an_embedded_pdf_propagates_up() {
        // A /Redact annotation inside an attachment fires the query-independent
        // UnappliedRedactAnnotation signal in the sub-scan; that signal must
        // reach the host report (tagged with the container), not be dropped.
        let mut inner = Document::with_version("1.5");
        let pages_id = inner.new_object_id();
        let annot = inner.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Redact",
            "OverlayText" => Object::string_literal("nothing to match"),
        });
        let page_id = inner.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(200), Object::Integer(200)],
            "Annots" => vec![Object::Reference(annot)],
        });
        inner.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => Object::Integer(1),
        }));
        let catalog = inner.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        inner.trailer.set("Root", catalog);
        let mut inner_bytes = Vec::new();
        inner.save_to(&mut inner_bytes).expect("serialize inner pdf");

        let doc = host_with_attachment("form.pdf", &inner_bytes, false);
        let (_findings, signals) = run_with_recursion_full(&doc, &zanzibar());
        assert!(
            signals.iter().any(|s| {
                matches!(s.kind, crate::report::SignalKind::UnappliedRedactAnnotation)
                    && s.location.contains("attachment:form.pdf")
            }),
            "a redaction signal inside an attachment must surface: {signals:?}"
        );
    }

    #[test]
    fn recursion_stops_at_the_depth_cap() {
        // Depth cap 3: a secret four attachment-levels down stays out of reach
        // (and the scan terminates rather than unwinding a crafted chain).
        let mut payload = pdf_bytes_with_xmp_secret("Zanzibar");
        for level in (1..=4).rev() {
            let mut host = host_with_attachment(&format!("level{level}.pdf"), &payload, false);
            let mut out = Vec::new();
            host.save_to(&mut out).expect("serialize level");
            payload = out;
        }
        let doc = Document::load_mem(&payload).expect("reload outermost");
        let f = run_with_recursion(&doc, &zanzibar());
        assert!(f.is_empty(), "depth cap must stop before level 4: {f:?}");
    }
}

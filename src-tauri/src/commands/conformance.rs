//! Declared ISO sub-format conformance detection (PDF/A, PDF/X, PDF/E, PDF/UA).
//!
//! This reads what a PDF *claims*, not whether it actually complies. Each ISO
//! subset stamps an identifier into the document's XMP metadata packet (the
//! stream referenced by `/Metadata` on the Catalog); we extract that packet
//! with lopdf and scan it for the well-known identifier keys.
//!
//! It deliberately does NOT validate conformance — that is a several-hundred-rule
//! job for a dedicated preflight engine (veraPDF et al.) and is out of scope.
//! A file can carry a perfect identifier stamp and still be non-compliant, so
//! all wording here is "Declares …", never "compliant".
//!
//! The core (`conformance_from_path`) is free of `AppState` and Tauri, so it is
//! directly unit-testable and reusable from non-Tauri code (e.g. a CLI).

use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use serde::Serialize;
use tauri::State;

#[derive(Serialize, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceClaims {
    /// Honest, display-ready labels, e.g. `"PDF/A-2b"`, `"PDF/UA-1"`. Empty when
    /// the file declares no recognized conformance.
    pub declared: Vec<String>,
}

#[tauri::command]
pub fn get_conformance(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<ConformanceClaims, String> {
    get_conformance_impl(&state, doc_id).map_err(String::from)
}

fn get_conformance_impl(state: &AppState, doc_id: String) -> Result<ConformanceClaims, AppError> {
    // Resolve the on-disk path via the same locking pattern get_metadata_impl uses;
    // the file on disk is the source of truth (in-place edits write through to it).
    let file_path = {
        let entry = state.get_document(&doc_id)?;
        let entry = lock_mutex(&entry)?;
        entry.file_path.clone()
    };
    Ok(conformance_from_path(&file_path))
}

/// Load the PDF with lopdf, extract its XMP packet, and parse out the declared
/// conformance claims. `AppState`-free so it is unit-testable and reusable from
/// a CLI. A missing/unparsable file or absent metadata yields no claims rather
/// than an error — "can't tell" reads as "declares nothing".
pub fn conformance_from_path(file_path: &str) -> ConformanceClaims {
    let Ok(doc) = lopdf::Document::load(file_path) else {
        return ConformanceClaims::default();
    };
    match read_xmp(&doc) {
        Some(xmp) => ConformanceClaims {
            declared: parse_claims(&xmp),
        },
        None => ConformanceClaims::default(),
    }
}

/// Locate the Catalog's `/Metadata` stream and return its (inflated) XMP bytes
/// as a string. The XMP packet is XML; we scan it textually rather than building
/// a full RDF parser.
fn read_xmp(doc: &lopdf::Document) -> Option<String> {
    let meta_id = doc
        .catalog()
        .ok()?
        .get(b"Metadata")
        .ok()?
        .as_reference()
        .ok()?;
    let stream = doc.get_object(meta_id).ok()?.as_stream().ok()?;
    // XMP is usually stored uncompressed, but tolerate FlateDecode just in case.
    let bytes = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Textual scan of the XMP for the identifier schemas, returning honest,
/// display-ready labels. Order: A, X, E, UA.
fn parse_claims(xmp: &str) -> Vec<String> {
    let mut out = Vec::new();

    // PDF/A — pdfaid:part (1/2/3/4) + optional pdfaid:conformance (A/B/U).
    if let Some(part) = xmp_value(xmp, "pdfaid:part") {
        let conf = xmp_value(xmp, "pdfaid:conformance")
            .unwrap_or_default()
            .to_lowercase();
        out.push(format!("PDF/A-{part}{conf}")); // e.g. "PDF/A-2b"
    }

    // PDF/X — the version string already reads "PDF/X-...".
    if let Some(ver) = xmp_value(xmp, "pdfxid:GTS_PDFXVersion") {
        out.push(ver);
    }

    // PDF/E — a part number (pdfeid:part), or just the marker namespace/flag.
    if let Some(part) = xmp_value(xmp, "pdfeid:part") {
        out.push(format!("PDF/E-{part}"));
    } else if xmp_value(xmp, "pdfeid:isPDFE").is_some() || xmp.contains("pdfe/ns/id") {
        out.push("PDF/E".to_string());
    }

    // PDF/UA — pdfuaid:part (1/2).
    if let Some(part) = xmp_value(xmp, "pdfuaid:part") {
        out.push(format!("PDF/UA-{part}"));
    }

    out
}

/// Read an XMP property given in either attribute form (`pdfaid:part="2"`) or
/// element form (`<pdfaid:part>2</pdfaid:part>`). Deliberately small and
/// dependency-free; a malformed packet simply yields `None`.
fn xmp_value(xmp: &str, key: &str) -> Option<String> {
    // Attribute form: key="value"
    let attr = format!("{key}=\"");
    if let Some(i) = xmp.find(&attr) {
        let rest = &xmp[i + attr.len()..];
        if let Some(end) = rest.find('"') {
            let value = rest[..end].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    // Element form: <key ...>value</key>
    let close = format!("</{key}>");
    let open = format!("<{key}");
    let open_at = xmp.find(&open)?;
    // Skip past the rest of the opening tag (handles attributes on the element).
    let after_open = &xmp[open_at..];
    let gt = after_open.find('>')? + 1;
    let start = open_at + gt;
    let end = xmp[start..].find(&close)? + start;
    let value = xmp[start..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PDFA_2B_ELEMENT: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
   <pdfaid:part>2</pdfaid:part>
   <pdfaid:conformance>B</pdfaid:conformance>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    const PDFX_ATTR: &str = r#"<rdf:Description rdf:about=""
      xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/"
      pdfxid:GTS_PDFXVersion="PDF/X-4"/>"#;

    const PDFUA_1_ELEMENT: &str = r#"<rdf:Description rdf:about="" xmlns:pdfuaid="http://www.aiim.org/pdfua/ns/id/">
   <pdfuaid:part>1</pdfuaid:part>
  </rdf:Description>"#;

    #[test]
    fn parses_pdfa_element_form() {
        assert_eq!(parse_claims(PDFA_2B_ELEMENT), vec!["PDF/A-2b".to_string()]);
    }

    #[test]
    fn parses_pdfx_attribute_form() {
        assert_eq!(parse_claims(PDFX_ATTR), vec!["PDF/X-4".to_string()]);
    }

    #[test]
    fn parses_pdfua_element_form() {
        assert_eq!(parse_claims(PDFUA_1_ELEMENT), vec!["PDF/UA-1".to_string()]);
    }

    #[test]
    fn parses_multiple_claims_in_order() {
        // A file can declare more than one standard (e.g. PDF/A + PDF/UA).
        let xmp = format!("{PDFA_2B_ELEMENT}\n{PDFUA_1_ELEMENT}");
        assert_eq!(
            parse_claims(&xmp),
            vec!["PDF/A-2b".to_string(), "PDF/UA-1".to_string()]
        );
    }

    #[test]
    fn no_claims_for_plain_xmp() {
        let xmp = r#"<rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
          <dc:title>Just a title</dc:title></rdf:Description>"#;
        assert!(parse_claims(xmp).is_empty());
    }

    #[test]
    fn xmp_value_reads_both_forms() {
        assert_eq!(xmp_value(r#"<a:b>7</a:b>"#, "a:b").as_deref(), Some("7"));
        assert_eq!(xmp_value(r#"x a:b="7" y"#, "a:b").as_deref(), Some("7"));
        assert_eq!(xmp_value("nothing here", "a:b"), None);
    }

    /// The plain fixture declares no conformance, so detection on a real file
    /// without an identifier stamp yields an empty list (not an error).
    #[test]
    fn plain_fixture_declares_nothing() {
        let claims = conformance_from_path(crate::fixture_path().to_str().unwrap());
        assert!(
            claims.declared.is_empty(),
            "sample.pdf should declare no conformance, got {:?}",
            claims.declared
        );
    }

    /// A missing file is reported as "declares nothing", never an error.
    #[test]
    fn missing_file_declares_nothing() {
        let claims = conformance_from_path("does-not-exist-xyz.pdf");
        assert!(claims.declared.is_empty());
    }

    /// End-to-end against a generated example PDF: the XMP `/Metadata` stream is
    /// read back through lopdf and the declared claim is detected. The fixture is
    /// produced by `cargo run --example gen_conformance_fixtures`.
    #[test]
    fn detects_declared_conformance_in_generated_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/conformance/pdfa-2b.pdf"
        );
        let claims = conformance_from_path(path);
        assert_eq!(claims.declared, vec!["PDF/A-2b".to_string()]);
    }

    /// A file can declare more than one standard; both are detected from the
    /// real PDF, in A-then-UA order.
    #[test]
    fn detects_multiple_claims_in_generated_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/conformance/pdfa-2b-and-ua-1.pdf"
        );
        let claims = conformance_from_path(path);
        assert_eq!(
            claims.declared,
            vec!["PDF/A-2b".to_string(), "PDF/UA-1".to_string()]
        );
    }
}

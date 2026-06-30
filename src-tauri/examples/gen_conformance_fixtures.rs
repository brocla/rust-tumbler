//! Generate example PDFs that *declare* ISO sub-format conformance, for testing
//! and demonstrating the declared-conformance detection (issue #16).
//!
//! Run from the `src-tauri` directory:
//!
//! ```sh
//! cargo run --example gen_conformance_fixtures
//! ```
//!
//! Output lands in `tests/fixtures/conformance/`. Each file is a minimal,
//! openable one-page PDF carrying the XMP identifier stamp for one (or more)
//! standard.
//!
//! IMPORTANT: these files only *declare* conformance via their XMP metadata —
//! they are NOT validated/compliant PDF/A·X·E·UA files (that needs a preflight
//! engine such as veraPDF). They exist solely to exercise the detector, which
//! itself only reads the declared claim. See the generated README.

use lopdf::{dictionary, Document, Stream};
use std::path::Path;

fn main() {
    // Resolve relative to the crate, not the current working directory, so the
    // output always lands in src-tauri regardless of where `cargo run` is invoked.
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance");
    std::fs::create_dir_all(&out_dir).expect("create output dir");

    // (filename, heading text, XMP identifier block(s))
    let files = [
        (
            "pdfa-2b.pdf",
            "PDF/A-2b",
            pdfa_xmp(2, "B"),
        ),
        (
            "pdfx-4.pdf",
            "PDF/X-4",
            pdfx_xmp("PDF/X-4"),
        ),
        (
            "pdfua-1.pdf",
            "PDF/UA-1",
            pdfua_xmp(1),
        ),
        (
            "pdfe-1.pdf",
            "PDF/E-1",
            pdfe_xmp(1),
        ),
        (
            "pdfa-2b-and-ua-1.pdf",
            "PDF/A-2b + PDF/UA-1",
            format!("{}\n{}", pdfa_block(2, "B"), pdfua_block(1)),
        ),
        (
            // A family Tumbler doesn't model, following the `…/<token>/ns/id/`
            // identifier-schema convention. Exercises the unrecognized-schema
            // fallback (issue #23).
            "unknown-standard.pdf",
            "Unknown PDF standard",
            unknown_block("pdfz", 1),
        ),
    ];

    for (name, heading, id_block) in files {
        let path = out_dir.join(name);
        build_pdf(heading, &wrap_xmp(&id_block))
            .save(&path)
            .unwrap_or_else(|e| panic!("save {name}: {e}"));
        println!("wrote {}", path.display());
    }

    std::fs::write(out_dir.join("README.md"), README).expect("write README");
    println!("wrote {}", out_dir.join("README.md").display());
}

/// Build a minimal one-page PDF with a visible heading and an XMP `/Metadata`
/// stream on the catalog.
fn build_pdf(heading: &str, xmp: &str) -> Document {
    let mut doc = Document::with_version("1.7");

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let content = format!(
        "BT /F1 20 Tf 36 250 Td ({}) Tj ET\n\
         BT /F1 10 Tf 36 215 Td (Declared-conformance example for Tumbler.) Tj ET\n\
         BT /F1 10 Tf 36 200 Td (Carries the XMP identifier only - not validated.) Tj ET",
        escape_pdf_string(heading)
    );
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));

    let metadata_id = doc.add_object(Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        xmp.as_bytes().to_vec(),
    ));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "MediaBox" => vec![0.into(), 0.into(), 380.into(), 300.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
    });

    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
    });

    // Link the page to its parent.
    if let Ok(page) = doc.get_dictionary_mut(page_id) {
        page.set("Parent", pages_id);
    }

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "Metadata" => metadata_id,
    });
    doc.trailer.set("Root", catalog_id);

    doc
}

/// Wrap one or more rdf:Description identifier blocks in a complete XMP packet.
fn wrap_xmp(id_blocks: &str) -> String {
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
{id_blocks}
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
    )
}

fn pdfa_block(part: u8, conformance: &str) -> String {
    format!(
        r#"  <rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
   <pdfaid:part>{part}</pdfaid:part>
   <pdfaid:conformance>{conformance}</pdfaid:conformance>
  </rdf:Description>"#
    )
}

fn pdfa_xmp(part: u8, conformance: &str) -> String {
    pdfa_block(part, conformance)
}

/// PDF/X uses the attribute form here, to exercise the parser's other branch.
fn pdfx_xmp(version: &str) -> String {
    format!(
        r#"  <rdf:Description rdf:about=""
      xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/"
      pdfxid:GTS_PDFXVersion="{version}"/>"#
    )
}

fn pdfua_block(part: u8) -> String {
    format!(
        r#"  <rdf:Description rdf:about="" xmlns:pdfuaid="http://www.aiim.org/pdfua/ns/id/">
   <pdfuaid:part>{part}</pdfuaid:part>
  </rdf:Description>"#
    )
}

fn pdfua_xmp(part: u8) -> String {
    pdfua_block(part)
}

fn pdfe_xmp(part: u8) -> String {
    format!(
        r#"  <rdf:Description rdf:about="" xmlns:pdfeid="http://www.aiim.org/pdfe/ns/id/">
   <pdfeid:part>{part}</pdfeid:part>
  </rdf:Description>"#
    )
}

/// A made-up identifier schema for a family the detector doesn't know, using the
/// conventional `…/<token>/ns/id/` namespace shape and a `part` property.
fn unknown_block(token: &str, part: u8) -> String {
    format!(
        r#"  <rdf:Description rdf:about="" xmlns:{token}id="http://www.example.org/{token}/ns/id/">
   <{token}id:part>{part}</{token}id:part>
  </rdf:Description>"#
    )
}

/// Escape characters that are special inside a PDF literal string.
fn escape_pdf_string(s: &str) -> String {
    s.replace('\\', r"\\").replace('(', r"\(").replace(')', r"\)")
}

const README: &str = r#"# Conformance example PDFs

Generated by `cargo run --example gen_conformance_fixtures` (from `src-tauri`).

Each file is a minimal, openable one-page PDF that **declares** conformance with
an ISO PDF sub-format via its XMP `/Metadata` packet:

| File | Declares |
|---|---|
| `pdfa-2b.pdf` | PDF/A-2b (element form, with conformance level) |
| `pdfx-4.pdf` | PDF/X-4 (attribute form) |
| `pdfua-1.pdf` | PDF/UA-1 (element form) |
| `pdfe-1.pdf` | PDF/E-1 |
| `pdfa-2b-and-ua-1.pdf` | PDF/A-2b **and** PDF/UA-1 (multiple claims) |
| `unknown-standard.pdf` | An unrecognized identifier schema (`…/pdfz/ns/id/`) — exercises the new-family fallback |

## Important: declared, not validated

These files carry only the XMP **identifier stamp** — they are **not** validated
or actually-compliant PDF/A·X·E·UA files. Producing genuinely compliant files
requires a preflight engine (e.g. veraPDF). They exist to exercise Tumbler's
declared-conformance *detection*, which by design reads only the claim, never
verifies it. The UI wording is "Declares PDF/A-2b", never "PDF/A compliant".
"#;

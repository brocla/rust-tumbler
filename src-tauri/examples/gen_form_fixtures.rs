//! Generate a minimal, deterministic **pure-AcroForm** PDF for exercising the
//! form-filling feature (issue #2). Run from the `src-tauri` directory:
//!
//! ```sh
//! cargo run --example gen_form_fixtures
//! ```
//!
//! Output: `tests/fixtures/forms/acroform_basic.pdf`. One 612x792 page with a
//! full AcroForm and no `/XFA`, carrying one widget of every type the feature
//! supports:
//!
//! | Field       | /FT | Notes |
//! |-------------|-----|-------|
//! | `fullName`  | Tx  | single-line text |
//! | `comments`  | Tx  | multiline (Ff bit 13) |
//! | `subscribe` | Btn | checkbox, on-state `/Yes` |
//! | `color`     | Btn | radio group (Ff bit 16), kids `Red`/`Blue` |
//! | `country`   | Ch  | combo box (Ff bit 18), /Opt USA/Canada/Mexico |
//!
//! The real-world fixture `f8946.pdf` is a *hybrid* AcroForm+XFA form; it hits
//! the XFA-only-vs-hybrid path, not this happy path, so we generate our own
//! pure-AcroForm document here for deterministic discovery/persistence tests.

use lopdf::{dictionary, Document, Object, Stream, StringFormat};
use std::path::Path;

// AcroForm field flags (PDF 32000-1, table 226/227/228). Bit numbers are
// 1-based in the spec; the value is 1 << (bit - 1).
const FF_MULTILINE: i64 = 1 << 12; // Tx, bit 13
const FF_RADIO: i64 = 1 << 15; // Btn, bit 16
const FF_COMBO: i64 = 1 << 17; // Ch, bit 18

/// A PDF literal text string.
fn text(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forms");
    std::fs::create_dir_all(&out_dir).expect("create output dir");

    let mut doc = Document::with_version("1.7");

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    // Reserve the page id so widgets can reference it via /P before the page
    // dict itself is built.
    let page_id = doc.new_object_id();

    // A tiny visible label so the page isn't blank when opened.
    let content = b"BT /F1 14 Tf 50 740 Td (Tumbler AcroForm test fixture) Tj ET".to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    // --- Text field: single line -------------------------------------------
    let full_name_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "T" => text("fullName"),
        "V" => text(""),
        "Rect" => vec![50.into(), 700.into(), 300.into(), 720.into()],
        "P" => page_id,
        "F" => 4,
        "DA" => text("/F1 12 Tf 0 g"),
    });

    // --- Text field: multiline ---------------------------------------------
    let comments_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "Ff" => FF_MULTILINE,
        "T" => text("comments"),
        "V" => text(""),
        "Rect" => vec![50.into(), 600.into(), 300.into(), 680.into()],
        "P" => page_id,
        "F" => 4,
        "DA" => text("/F1 12 Tf 0 g"),
    });

    // --- Checkbox (on-state /Yes) ------------------------------------------
    let subscribe_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Btn",
        "T" => text("subscribe"),
        "V" => Object::Name(b"Off".to_vec()),
        "AS" => Object::Name(b"Off".to_vec()),
        "Rect" => vec![50.into(), 560.into(), 65.into(), 575.into()],
        "P" => page_id,
        "F" => 4,
        "AP" => dictionary! {
            "N" => dictionary! {
                "Yes" => Object::Reference(content_id), // any stream ref; value unused for discovery
                "Off" => Object::Reference(content_id),
            },
        },
    });

    // --- Radio group: parent + two kids ------------------------------------
    let color_id = doc.new_object_id();
    let red_kid_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "Parent" => color_id,
        "AS" => Object::Name(b"Off".to_vec()),
        "Rect" => vec![50.into(), 520.into(), 65.into(), 535.into()],
        "P" => page_id,
        "F" => 4,
        "AP" => dictionary! {
            "N" => dictionary! {
                "Red" => Object::Reference(content_id),
                "Off" => Object::Reference(content_id),
            },
        },
    });
    let blue_kid_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "Parent" => color_id,
        "AS" => Object::Name(b"Off".to_vec()),
        "Rect" => vec![80.into(), 520.into(), 95.into(), 535.into()],
        "P" => page_id,
        "F" => 4,
        "AP" => dictionary! {
            "N" => dictionary! {
                "Blue" => Object::Reference(content_id),
                "Off" => Object::Reference(content_id),
            },
        },
    });
    doc.set_object(
        color_id,
        dictionary! {
            "FT" => "Btn",
            "Ff" => FF_RADIO,
            "T" => text("color"),
            "V" => Object::Name(b"Off".to_vec()),
            "Kids" => vec![red_kid_id.into(), blue_kid_id.into()],
        },
    );

    // --- Dropdown (combo box) ----------------------------------------------
    let country_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Ch",
        "Ff" => FF_COMBO,
        "T" => text("country"),
        "V" => text("USA"),
        "Opt" => vec![text("USA"), text("Canada"), text("Mexico")],
        "Rect" => vec![50.into(), 480.into(), 200.into(), 500.into()],
        "P" => page_id,
        "F" => 4,
        "DA" => text("/F1 12 Tf 0 g"),
    });

    // --- Page ---------------------------------------------------------------
    doc.set_object(
        page_id,
        dictionary! {
            "Type" => "Page",
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Annots" => vec![
                full_name_id.into(),
                comments_id.into(),
                subscribe_id.into(),
                red_kid_id.into(),
                blue_kid_id.into(),
                country_id.into(),
            ],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        },
    );

    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
    });
    if let Ok(page) = doc.get_dictionary_mut(page_id) {
        page.set("Parent", pages_id);
    }

    // --- AcroForm (pure: no /XFA) ------------------------------------------
    let acroform_id = doc.add_object(dictionary! {
        "Fields" => vec![
            full_name_id.into(),
            comments_id.into(),
            subscribe_id.into(),
            color_id.into(),
            country_id.into(),
        ],
        "DA" => text("/F1 12 Tf 0 g"),
        "DR" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
    });

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);

    let path = out_dir.join("acroform_basic.pdf");
    doc.save(&path).unwrap_or_else(|e| panic!("save fixture: {e}"));
    println!("wrote {}", path.display());

    let sig_path = out_dir.join("acroform_signature.pdf");
    build_signature_fixture()
        .save(&sig_path)
        .unwrap_or_else(|e| panic!("save signature fixture: {e}"));
    println!("wrote {}", sig_path.display());
}

/// A one-page PDF with a single *genuine* signature field — an empty (unsigned)
/// `/FT /Sig` widget — plus the `/SigFlags` the spec expects. Nothing signs it;
/// it exists so signature-field discovery/placement and (future) signing work
/// has a real `/Sig` widget to develop against, distinct from the DocuSign
/// text-placeholder "signatures" seen in real-world sample forms.
fn build_signature_fixture() -> Document {
    let mut doc = Document::with_version("1.7");

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let page_id = doc.new_object_id();
    let content = b"BT /F1 14 Tf 50 740 Td (Tumbler signature-field test fixture) Tj ET\n\
                    BT /F1 12 Tf 50 545 Td (Signature:) Tj ET"
        .to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    // An empty signature field: /FT /Sig, a widget with a /Rect, and no /V
    // (i.e. not yet signed).
    let sig_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Sig",
        "T" => text("signature1"),
        "Rect" => vec![120.into(), 535.into(), 320.into(), 565.into()],
        "P" => page_id,
        "F" => 4,
    });

    doc.set_object(
        page_id,
        dictionary! {
            "Type" => "Page",
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Annots" => vec![sig_id.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
        },
    );

    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
    });
    if let Ok(page) = doc.get_dictionary_mut(page_id) {
        page.set("Parent", pages_id);
    }

    let acroform_id = doc.add_object(dictionary! {
        "Fields" => vec![sig_id.into()],
        // Bit 1 = the document contains signature fields; bit 2 = append-only.
        "SigFlags" => 3,
        "DA" => text("/F1 12 Tf 0 g"),
        "DR" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
    });

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc
}

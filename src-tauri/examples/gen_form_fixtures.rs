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
const FF_COMB: i64 = 1 << 24; // Tx, bit 25 (spread chars into /MaxLen cells)

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
    // Title plus a description drawn beside each field so a dev can see what
    // type each one is. Each BT/ET resets the text matrix, so Td is an absolute
    // page position. Avoid literal parentheses (they'd need PDF escaping).
    let content = b"\
        BT /F1 13 Tf 50 740 Td (Tumbler AcroForm test fixture) Tj ET\n\
        BT /F1 9 Tf 310 706 Td (fullName: single-line text field) Tj ET\n\
        BT /F1 9 Tf 310 668 Td (comments: multiline text field) Tj ET\n\
        BT /F1 9 Tf 75 563 Td (subscribe: checkbox, on-state Yes) Tj ET\n\
        BT /F1 9 Tf 105 523 Td (color: radio group, options Red / Blue) Tj ET\n\
        BT /F1 9 Tf 210 484 Td (country: dropdown, options USA / Canada / Mexico) Tj ET\n\
        BT /F1 9 Tf 260 444 Td (ssn: comb text field, /MaxLen 9 - caps input at 9 chars) Tj ET"
        .to_vec();
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

    // --- Comb text field: SSN-style, /MaxLen 9 -----------------------------
    // A border makes pdfium draw the comb cell dividers (self-contained grid);
    // real forms often omit it and rely on printed page artwork instead.
    let ssn_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "Ff" => FF_COMB,
        "MaxLen" => 9,
        "T" => text("ssn"),
        "V" => text(""),
        "Rect" => vec![50.into(), 438.into(), 230.into(), 462.into()],
        "P" => page_id,
        "F" => 4,
        "DA" => text("/F1 12 Tf 0 g"),
        "MK" => dictionary! { "BC" => vec![0.into()] },
        "BS" => dictionary! { "W" => 1, "S" => "S" },
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
                ssn_id.into(),
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
            ssn_id.into(),
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

    let reset_path = out_dir.join("acroform_reset.pdf");
    build_reset_fixture()
        .save(&reset_path)
        .unwrap_or_else(|e| panic!("save reset fixture: {e}"));
    println!("wrote {}", reset_path.display());
}

/// A one-page PDF for exercising form *actions* (issue: form buttons):
///
/// | Field       | /FT | Notes |
/// |-------------|-----|-------|
/// | `hasDefault`| Tx  | current value "typed", `/DV "Default"` — reset restores it |
/// | `noDefault` | Tx  | current value "stuff", no `/DV` — reset clears it |
/// | `agree`     | Btn | checkbox, current `/Yes`, `/DV /Off` |
/// | `resetBtn`  | Btn | pushbutton with `/A << /S /ResetForm >>` (all fields) |
/// | `jsBtn`     | Btn | pushbutton with `/A << /S /JavaScript >>` (unsupported) |
fn build_reset_fixture() -> Document {
    let mut doc = Document::with_version("1.7");

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    let page_id = doc.new_object_id();
    // Descriptions drawn on the page, one per row beside its field, so a dev
    // knows each field's intent. Fields are pre-filled so Clear/Reset visibly
    // acts on them. Each BT/ET resets the text matrix, so Td is an absolute
    // page position. Avoid literal parentheses (they'd need PDF escaping).
    let content = b"\
        BT /F1 13 Tf 50 762 Td (Tumbler form-actions test fixture) Tj ET\n\
        BT /F1 9 Tf 50 746 Td (Fields are pre-filled so you can watch Clear/Reset act on them.) Tj ET\n\
        BT /F1 9 Tf 260 710 Td (hasDefault: has /DV Default -> Clear/Reset RESTORES Default, not empty) Tj ET\n\
        BT /F1 9 Tf 260 676 Td (noDefault: no /DV -> Clear/Reset EMPTIES it) Tj ET\n\
        BT /F1 9 Tf 76 642 Td (agree: checkbox /DV Off -> Clear/Reset UNCHECKS it) Tj ET\n\
        BT /F1 9 Tf 160 610 Td (Reset button: /S /ResetForm action -> WORKS, clears the form) Tj ET\n\
        BT /F1 9 Tf 160 574 Td (Clear button: JavaScript action -> NOT supported, shows a toast only) Tj ET"
        .to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    let has_default_id = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx",
        "T" => text("hasDefault"),
        "V" => text("typed"),
        "DV" => text("Default"),
        "Rect" => vec![50.into(), 704.into(), 250.into(), 722.into()],
        "P" => page_id, "F" => 4, "DA" => text("/F1 12 Tf 0 g"),
    });
    let no_default_id = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx",
        "T" => text("noDefault"),
        "V" => text("stuff"),
        "Rect" => vec![50.into(), 670.into(), 250.into(), 688.into()],
        "P" => page_id, "F" => 4, "DA" => text("/F1 12 Tf 0 g"),
    });
    let agree_id = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "FT" => "Btn",
        "T" => text("agree"),
        "V" => Object::Name(b"Yes".to_vec()),
        "AS" => Object::Name(b"Yes".to_vec()),
        "DV" => Object::Name(b"Off".to_vec()),
        "Rect" => vec![50.into(), 638.into(), 66.into(), 654.into()],
        "P" => page_id, "F" => 4,
        "AP" => dictionary! { "N" => dictionary! {
            "Yes" => Object::Reference(content_id),
            "Off" => Object::Reference(content_id),
        } },
    });

    // Pushbutton with a standard ResetForm action (no /Fields → reset all).
    let reset_btn_id = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "FT" => "Btn",
        "Ff" => 1i64 << 16, // pushbutton
        "T" => text("resetBtn"),
        "Rect" => vec![50.into(), 602.into(), 150.into(), 622.into()],
        "P" => page_id, "F" => 4,
        "MK" => dictionary! { "CA" => text("Reset") },
        "A" => dictionary! { "S" => "ResetForm" },
    });
    // Pushbutton whose action is JavaScript → unsupported.
    let js_btn_id = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "FT" => "Btn",
        "Ff" => 1i64 << 16,
        "T" => text("jsBtn"),
        "Rect" => vec![50.into(), 566.into(), 150.into(), 586.into()],
        "P" => page_id, "F" => 4,
        "MK" => dictionary! { "CA" => text("Clear") },
        "A" => dictionary! {
            "S" => "JavaScript",
            "JS" => text("this.resetForm();"),
        },
    });

    doc.set_object(page_id, dictionary! {
        "Type" => "Page",
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Annots" => vec![
            has_default_id.into(), no_default_id.into(), agree_id.into(),
            reset_btn_id.into(), js_btn_id.into(),
        ],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
    });
    if let Ok(page) = doc.get_dictionary_mut(page_id) {
        page.set("Parent", pages_id);
    }
    let acroform_id = doc.add_object(dictionary! {
        "Fields" => vec![
            has_default_id.into(), no_default_id.into(), agree_id.into(),
            reset_btn_id.into(), js_btn_id.into(),
        ],
        "DA" => text("/F1 12 Tf 0 g"),
        "DR" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id, "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc
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

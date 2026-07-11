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

/// Baked-in creation date for every fixture, so regenerating produces
/// deterministic bytes. PDF date string (PDF 32000-1 §7.9.4).
const FIXTURE_DATE: &str = "D:20260710000000Z";

/// Stamps the self-documenting Info-dictionary metadata every Tumbler test
/// fixture carries (issue #73): `Creator` names the generator so the file
/// records the tool that made it; `CreationDate` is fixed for determinism.
fn set_fixture_metadata(doc: &mut Document, keywords: &str) {
    let info_id = doc.add_object(dictionary! {
        "Title" => text("Tumbler Test Fixture"),
        "Author" => text("Claude"),
        "Keywords" => text(keywords),
        "Creator" => text("gen_form_fixtures.rs (lopdf)"),
        "CreationDate" => text(FIXTURE_DATE),
    });
    doc.trailer.set("Info", info_id);
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
        BT /F1 9 Tf 260 444 Td (ssn: comb text field, /MaxLen 9 - caps input at 9 chars) Tj ET\n\
        BT /F1 9 Tf 50 90 Td (Live test: open in Tumbler, fill each field, Save, reopen - values persist.) Tj ET\n\
        BT /F1 8 Tf 50 66 Td (Regenerate: cargo run --example gen_form_fixtures) Tj ET"
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

    set_fixture_metadata(
        &mut doc,
        "forms, acroform, text, checkbox, radio, combo, comb, test-fixture",
    );
    let path = out_dir.join("acroform_basic.pdf");
    doc.save(&path).unwrap_or_else(|e| panic!("save fixture: {e}"));
    println!("wrote {}", path.display());

    let mut sig_doc = build_signature_fixture();
    set_fixture_metadata(&mut sig_doc, "forms, acroform, signature-field, test-fixture");
    let sig_path = out_dir.join("acroform_signature.pdf");
    sig_doc
        .save(&sig_path)
        .unwrap_or_else(|e| panic!("save signature fixture: {e}"));
    println!("wrote {}", sig_path.display());

    let mut reset_doc = build_reset_fixture();
    set_fixture_metadata(&mut reset_doc, "forms, acroform, reset, form-actions, test-fixture");
    let reset_path = out_dir.join("acroform_reset.pdf");
    reset_doc
        .save(&reset_path)
        .unwrap_or_else(|e| panic!("save reset fixture: {e}"));
    println!("wrote {}", reset_path.display());

    let mut styling_doc = build_styling_fixture();
    set_fixture_metadata(&mut styling_doc, "forms, acroform, text-styling, DA, test-fixture");
    let styling_path = out_dir.join("acroform_styling.pdf");
    styling_doc
        .save(&styling_path)
        .unwrap_or_else(|e| panic!("save styling fixture: {e}"));
    println!("wrote {}", styling_path.display());
}

/// A one-page PDF exercising text styling from `/DA` + `/Q`: alignment, size,
/// color, and auto-size. Each text field is pre-filled and labeled with its
/// expected styling so a live test is self-describing.
fn build_styling_fixture() -> Document {
    let mut doc = Document::with_version("1.7");
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let page_id = doc.new_object_id();
    let content = b"\
        BT /F1 13 Tf 50 762 Td (Tumbler field-text-styling test fixture) Tj ET\n\
        BT /F1 9 Tf 360 716 Td (left, 12pt, black) Tj ET\n\
        BT /F1 9 Tf 360 681 Td (center, 12pt, red) Tj ET\n\
        BT /F1 9 Tf 360 646 Td (right, 10pt, blue) Tj ET\n\
        BT /F1 9 Tf 360 606 Td (left, 20pt, black) Tj ET\n\
        BT /F1 9 Tf 360 561 Td (auto-size: Tf 0 fills the box) Tj ET\n\
        BT /F1 8 Tf 50 66 Td (Regenerate: cargo run --example gen_form_fixtures) Tj ET"
        .to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    // (name, value, rect, /Q, /DA)
    let specs: [(&str, &str, [i64; 4], i64, &str); 5] = [
        ("leftBlack", "Left aligned", [50, 710, 350, 730], 0, "/F1 12 Tf 0 g"),
        ("centerRed", "Centered", [50, 675, 350, 695], 1, "/F1 12 Tf 1 0 0 rg"),
        ("rightBlue", "Right", [50, 640, 350, 660], 2, "/F1 10 Tf 0 0 1 rg"),
        ("bigText", "Big", [50, 595, 350, 625], 0, "/F1 20 Tf 0 g"),
        ("autoSize", "Auto-sized to fit this tall box", [50, 545, 350, 585], 0, "/F1 0 Tf 0 g"),
    ];
    let mut field_ids = Vec::new();
    for (name, value, r, q, da) in specs {
        let id = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx",
            "T" => text(name),
            "V" => text(value),
            "Q" => q,
            "Rect" => vec![r[0].into(), r[1].into(), r[2].into(), r[3].into()],
            "P" => page_id, "F" => 4,
            "DA" => text(da),
            "MK" => dictionary! { "BC" => vec![0.into()] },
            "BS" => dictionary! { "W" => 1, "S" => "S" },
        });
        field_ids.push(id);
    }

    let annots: Vec<Object> = field_ids.iter().map(|id| (*id).into()).collect();
    doc.set_object(page_id, dictionary! {
        "Type" => "Page",
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Annots" => annots.clone(),
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    let pages_id = doc.add_object(dictionary! {
        "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
    });
    if let Ok(p) = doc.get_dictionary_mut(page_id) { p.set("Parent", pages_id); }

    let acroform_id = doc.add_object(dictionary! {
        "Fields" => annots,
        "DA" => text("/F1 12 Tf 0 g"),
        "DR" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
    });
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id, "AcroForm" => acroform_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc
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
        BT /F1 9 Tf 160 574 Td (Clear button: JavaScript action -> NOT supported, shows a toast only) Tj ET\n\
        BT /F1 8 Tf 50 66 Td (Regenerate: cargo run --example gen_form_fixtures) Tj ET"
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
    let content = b"\
        BT /F1 14 Tf 50 740 Td (Tumbler signature-field test fixture) Tj ET\n\
        BT /F1 10 Tf 120 578 Td (Draw your signature in the box below - mouse, pen, touch, or trackpad:) Tj ET\n\
        BT /F1 8 Tf 50 66 Td (Regenerate: cargo run --example gen_form_fixtures) Tj ET"
        .to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    // An empty signature field: /FT /Sig, a widget with a /Rect, and no /V
    // (i.e. not yet signed). A visible border marks the draw area.
    let sig_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Sig",
        "T" => text("signature1"),
        "Rect" => vec![120.into(), 500.into(), 440.into(), 570.into()],
        "P" => page_id,
        "F" => 4,
        "MK" => dictionary! { "BC" => vec![0.into()] },
        "BS" => dictionary! { "W" => 1, "S" => "S" },
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

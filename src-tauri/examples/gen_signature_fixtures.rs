//! Generate signed-PDF fixtures for digital-signature verification (issue #17):
//! one that should verify, and two that should fail in the two distinct ways the
//! feature distinguishes.
//!
//! Run from anywhere (resolves output via CARGO_MANIFEST_DIR):
//!
//! ```sh
//! cargo run --example gen_signature_fixtures
//! ```
//!
//! Requires **openssl** on PATH at regenerate time (used to mint a self-signed
//! test certificate and produce the detached CMS/PKCS#7 signature). The verifier
//! itself is pure Rust, so using a different implementation here doubles as an
//! independent cross-check. Output lands in `tests/fixtures/signatures/`:
//!
//! | File | Expected status |
//! |---|---|
//! | `signed-valid.pdf` | Verified |
//! | `signed-modified-after.pdf` | ModifiedAfter (bytes appended after signing) |
//! | `signed-tampered.pdf` | Invalid (a signed byte changed) |
//!
//! These are self-signed test certificates — "Verified" means the signature is
//! cryptographically intact, NOT that the signer is trusted.

use std::path::{Path, PathBuf};
use std::process::Command;

// Reserved hex length for the /Contents placeholder. The CMS blob (RSA-2048,
// one cert, signed attrs) is ~1.5 KB; 8 KB of hex (4 KB) is comfortable headroom,
// zero-padded after the real signature.
const CONTENTS_HEX_LEN: usize = 8192;
const BYTE_RANGE_PLACEHOLDER: &str = "/ByteRange[0 0000000000 0000000000 0000000000]";
// A sentinel inside the page text, in the signed region, that the tampered
// fixture mutates by one byte.
const SENTINEL: &str = "SIGNED-DEMO";

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signatures");
    std::fs::create_dir_all(&out_dir).expect("create output dir");

    let tmp = std::env::temp_dir().join(format!("tumbler-siggen-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create temp dir");
    let cert = tmp.join("cert.pem");
    let key = tmp.join("key.pem");
    make_self_signed(&cert, &key);

    // 1) Valid signed PDF.
    let valid = build_signed_pdf(&cert, &key, &tmp);
    write(&out_dir.join("signed-valid.pdf"), &valid);

    // 2) Modified after signing: append an incremental-update-style trailer past
    //    the signed range. Signed bytes are untouched (still intact), but the
    //    file now extends beyond the signature's coverage.
    let mut modified = valid.clone();
    modified.extend_from_slice(b"\n% appended after signing (incremental update)\n");
    write(&out_dir.join("signed-modified-after.pdf"), &modified);

    // 3) Tampered: flip one byte inside the signed region (the page-text
    //    sentinel) so the recomputed digest no longer matches the sealed one.
    let mut tampered = valid.clone();
    let at = find(&tampered, SENTINEL.as_bytes()).expect("sentinel present");
    tampered[at] = b'X'; // 'S' -> 'X', same length, still loadable
    write(&out_dir.join("signed-tampered.pdf"), &tampered);

    write_readme(&out_dir.join("README.md"));
    let _ = std::fs::remove_dir_all(&tmp);
    println!("done");
}

fn make_self_signed(cert: &Path, key: &Path) {
    let status = Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-days", "3650", "-nodes",
            "-subj", "/CN=Tumbler Test Signer",
            "-keyout",
        ])
        .arg(key)
        .arg("-out")
        .arg(cert)
        .status()
        .expect("run openssl req (is openssl on PATH?)");
    assert!(status.success(), "openssl req failed");
}

/// Build a one-page PDF with a signature field, patch its ByteRange to cover the
/// whole file except the /Contents hole, sign those bytes with openssl, and embed
/// the detached CMS.
fn build_signed_pdf(cert: &Path, key: &Path, tmp: &Path) -> Vec<u8> {
    let mut pdf = assemble_with_placeholders();

    // Locate the /Contents hole.
    let contents_marker = b"/Contents <";
    let cm = find(&pdf, contents_marker).expect("contents marker");
    let lt = cm + contents_marker.len() - 1; // index of '<'
    let gt = lt + 1 + CONTENTS_HEX_LEN; // index of '>'
    let (a, b) = (0usize, lt); // range1 = [0, lt)
    let c = gt + 1; // range2 starts after '>'
    let d = pdf.len() - c; // to EOF

    // Patch ByteRange (fixed width, so no offsets shift).
    let br = format!("/ByteRange[{a} {b} {c} {d}]");
    let br_at = find(&pdf, BYTE_RANGE_PLACEHOLDER.as_bytes()).expect("byterange placeholder");
    let padded = format!("{br:<width$}", width = BYTE_RANGE_PLACEHOLDER.len());
    pdf[br_at..br_at + padded.len()].copy_from_slice(padded.as_bytes());

    // Sign the two covered spans (everything but the hole).
    let mut signed_bytes = Vec::new();
    signed_bytes.extend_from_slice(&pdf[a..b]);
    signed_bytes.extend_from_slice(&pdf[c..c + d]);
    let der = openssl_cms_sign(&signed_bytes, cert, key, tmp);

    // Embed the hex, zero-padded to the reserved length.
    let mut hex = hex_encode(&der);
    assert!(hex.len() <= CONTENTS_HEX_LEN, "signature larger than reserved /Contents");
    hex.extend(std::iter::repeat('0').take(CONTENTS_HEX_LEN - hex.len()));
    pdf[lt + 1..lt + 1 + CONTENTS_HEX_LEN].copy_from_slice(hex.as_bytes());

    pdf
}

/// Detached CMS/PKCS#7 over `data`, DER-encoded. Detached (eContent absent) with
/// signed attributes (incl. messageDigest) — the default for `openssl cms -sign`.
fn openssl_cms_sign(data: &[u8], cert: &Path, key: &Path, tmp: &Path) -> Vec<u8> {
    let data_path = tmp.join("signed-bytes.bin");
    let out_path = tmp.join("sig.der");
    std::fs::write(&data_path, data).expect("write data");
    let status = Command::new("openssl")
        .args(["cms", "-sign", "-binary", "-md", "sha256", "-outform", "DER", "-nosmimecap"])
        .arg("-in")
        .arg(&data_path)
        .arg("-signer")
        .arg(cert)
        .arg("-inkey")
        .arg(key)
        .arg("-out")
        .arg(&out_path)
        .status()
        .expect("run openssl cms");
    assert!(status.success(), "openssl cms -sign failed");
    std::fs::read(&out_path).expect("read sig.der")
}

/// Assemble the PDF body + xref + trailer with the ByteRange and Contents
/// placeholders in object 5 (the signature dictionary).
fn assemble_with_placeholders() -> Vec<u8> {
    let zeros = "0".repeat(CONTENTS_HEX_LEN);
    let content = format!(
        "BT /F1 18 Tf 36 250 Td (Tumbler signed-PDF fixture) Tj ET\n\
         BT /F1 11 Tf 36 220 Td (Sentinel: {SENTINEL}) Tj ET"
    );

    let objects: Vec<String> = vec![
        "<</Type/Catalog/Pages 2 0 R/AcroForm 8 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 380 300]/Annots[4 0 R]\
         /Contents 6 0 R/Resources<</Font<</F1 7 0 R>>>>>>"
            .to_string(),
        "<</Type/Annot/Subtype/Widget/FT/Sig/T(Signature1)/Rect[36 36 220 90]/V 5 0 R/P 3 0 R>>"
            .to_string(),
        format!(
            "<</Type/Sig/Filter/Adobe.PPKLite/SubFilter/adbe.pkcs7.detached\
             /Name(Tumbler Test Signer)/Reason(Demonstration)/Location(Tumbler tests)\
             /M(D:20240101000000Z){BYTE_RANGE_PLACEHOLDER}/Contents <{zeros}>>>"
        ),
        format!("<</Length {}>>\nstream\n{content}\nendstream", content.len() + 1),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        "<</Fields[4 0 R]/SigFlags 3>>".to_string(),
    ];

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");

    let mut offsets = vec![0usize; objects.len() + 1];
    for (i, body) in objects.iter().enumerate() {
        let num = i + 1;
        offsets[num] = out.len();
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_at = out.len();
    let size = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for num in 1..size {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[num]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<</Size {size}/Root 1 0 R>>\nstartxref\n{xref_at}\n%%EOF").as_bytes(),
    );

    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    println!("wrote {}", path.display());
}

fn write_readme(path: &PathBuf) {
    let body = r#"# Signed-PDF fixtures

Generated by `cargo run --example gen_signature_fixtures` (needs **openssl** on
PATH). For digital-signature *verification* tests (issue #17).

| File | Expected status | How |
|---|---|---|
| `signed-valid.pdf` | **Verified** | Self-signed detached CMS over the full ByteRange |
| `signed-modified-after.pdf` | **ModifiedAfter** | `signed-valid.pdf` + bytes appended past the signature's coverage (signed bytes untouched) |
| `signed-tampered.pdf` | **Invalid** | `signed-valid.pdf` with one byte flipped inside the signed region |

## Important: intact, not trusted

The certificate is a throwaway self-signed test cert. A **Verified** result means
the signature is cryptographically *intact* (the signed bytes are unchanged) — it
does **not** mean the signer's identity is trusted. Trust-chain/revocation
validation is intentionally out of scope for this slice.
"#;
    std::fs::write(path, body).expect("write README");
    println!("wrote {}", path.display());
}

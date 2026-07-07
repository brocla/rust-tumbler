//! Generate signed-PDF fixtures for digital-signature verification (issues #17
//! and #39): signatures that should verify — across digest algorithms and both
//! CMS encodings (DER and Adobe-style BER indefinite-length) — and ones that
//! should fail in the two distinct ways the feature distinguishes.
//!
//! Run from anywhere (resolves output via CARGO_MANIFEST_DIR):
//!
//! ```sh
//! cargo run --example gen_signature_fixtures
//! ```
//!
//! Requires **openssl** on PATH at regenerate time (used to mint a self-signed
//! test certificate and produce the detached CMS/PKCS#7 signatures). The
//! verifier itself is Windows CryptoAPI + RustCrypto hashing, so using openssl
//! here doubles as an independent cross-check. Output lands in
//! `tests/fixtures/signatures/`:
//!
//! | File | Expected status |
//! |---|---|
//! | `signed-valid.pdf` | Verified (SHA-256, DER) |
//! | `signed-sha1.pdf` | Verified (SHA-1) |
//! | `signed-sha384.pdf` | Verified (SHA-384) |
//! | `signed-sha512.pdf` | Verified (SHA-512) |
//! | `signed-ber.pdf` | Verified (SHA-256, BER indefinite-length via `-indef`) |
//! | `signed-ber-tampered.pdf` | Invalid (a signed byte changed, BER CMS) |
//! | `signed-modified-after.pdf` | ModifiedAfter (bytes appended after signing) |
//! | `signed-tampered.pdf` | Invalid (a signed byte changed) |
//! | `live-test-verified.pdf` | Verified — self-documenting fixture for manual (live) testing; its page text says what badge to expect |
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
// fixtures mutate by one byte.
const SENTINEL: &str = "SIGNED-DEMO";

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signatures");
    std::fs::create_dir_all(&out_dir).expect("create output dir");

    let tmp = std::env::temp_dir().join(format!("tumbler-siggen-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("create temp dir");
    let cert = tmp.join("cert.pem");
    let key = tmp.join("key.pem");
    make_self_signed(&cert, &key);

    let default_content = page_lines(&[
        ("18", "Tumbler signed-PDF fixture"),
        ("11", &format!("Sentinel: {SENTINEL}")),
    ]);

    // 1) Valid signed PDF (SHA-256, DER — the baseline from issue #17).
    let valid = build_signed_pdf(&cert, &key, &tmp, &default_content, "sha256", false);
    write(&out_dir.join("signed-valid.pdf"), &valid);

    // 2) Modified after signing: append an incremental-update-style trailer past
    //    the signed range. Signed bytes are untouched (still intact), but the
    //    file now extends beyond the signature's coverage.
    let mut modified = valid.clone();
    modified.extend_from_slice(b"\n% appended after signing (incremental update)\n");
    write(&out_dir.join("signed-modified-after.pdf"), &modified);

    // 3) Tampered: flip one byte inside the signed region (the page-text
    //    sentinel) so the recomputed digest no longer matches the sealed one.
    write(&out_dir.join("signed-tampered.pdf"), &tamper(&valid));

    // 4) Non-SHA-256 digests (issue #39): SHA-1 / SHA-384 / SHA-512.
    for md in ["sha1", "sha384", "sha512"] {
        let pdf = build_signed_pdf(&cert, &key, &tmp, &default_content, md, false);
        write(&out_dir.join(format!("signed-{md}.pdf")), &pdf);
    }

    // 5) BER indefinite-length CMS (issue #39): `openssl cms -indef` emits the
    //    same `30 80 … 00 00` framing Adobe uses. One valid, one tampered — the
    //    tampered one is the guard that BER support didn't widen "parses now"
    //    into "verifies always".
    let ber = build_signed_pdf(&cert, &key, &tmp, &default_content, "sha256", true);
    write(&out_dir.join("signed-ber.pdf"), &ber);
    write(&out_dir.join("signed-ber-tampered.pdf"), &tamper(&ber));

    // 6) Live-test fixture for manual verification in the running app. Its own
    //    page text documents what it is, what Tumbler should show, and how to
    //    regenerate it (project convention: fixtures are self-documenting).
    let live_content = page_lines(&[
        ("16", "Tumbler live-test fixture: verified digital signature"),
        ("10", "What this is: a PDF signed with a throwaway self-signed"),
        ("10", "certificate (CN=Tumbler Test Signer), SHA-256, detached CMS."),
        ("10", "Expected in Tumbler: the status bar shows the badge"),
        ("12", "\"Verified Signed Document\""),
        ("10", "(intact signature - it does NOT mean the signer is trusted)."),
        ("10", "Any edit + save must invalidate the signature."),
        ("10", "Regenerate (needs openssl on PATH):"),
        ("10", "  cargo run --example gen_signature_fixtures"),
        ("10", &format!("Sentinel: {SENTINEL}")),
    ]);
    let live = build_signed_pdf(&cert, &key, &tmp, &live_content, "sha256", false);
    write(&out_dir.join("live-test-verified.pdf"), &live);

    write_readme(&out_dir.join("README.md"));
    let _ = std::fs::remove_dir_all(&tmp);
    println!("done");
}

/// Flip one byte inside the signed region (the page-text sentinel) so the
/// recomputed digest no longer matches the sealed one.
fn tamper(pdf: &[u8]) -> Vec<u8> {
    let mut tampered = pdf.to_vec();
    let at = find(&tampered, SENTINEL.as_bytes()).expect("sentinel present");
    tampered[at] = b'X'; // 'S' -> 'X', same length, still loadable
    tampered
}

/// Content-stream text: one `(size, text)` pair per line, top-down from y=270.
fn page_lines(lines: &[(&str, &str)]) -> String {
    let mut y = 270;
    let mut out = Vec::new();
    for (size, text) in lines {
        out.push(format!("BT /F1 {size} Tf 24 {y} Td ({text}) Tj ET"));
        y -= 22;
    }
    out.join("\n")
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
/// the detached CMS. `md` picks the digest (sha1|sha256|sha384|sha512); `indef`
/// asks openssl for BER indefinite-length framing (what Adobe emits) instead of
/// definite-length DER.
fn build_signed_pdf(
    cert: &Path,
    key: &Path,
    tmp: &Path,
    content: &str,
    md: &str,
    indef: bool,
) -> Vec<u8> {
    let mut pdf = assemble_with_placeholders(content);

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
    let der = openssl_cms_sign(&signed_bytes, cert, key, tmp, md, indef);

    // Embed the hex, zero-padded to the reserved length.
    let mut hex = hex_encode(&der);
    assert!(hex.len() <= CONTENTS_HEX_LEN, "signature larger than reserved /Contents");
    hex.extend(std::iter::repeat('0').take(CONTENTS_HEX_LEN - hex.len()));
    pdf[lt + 1..lt + 1 + CONTENTS_HEX_LEN].copy_from_slice(hex.as_bytes());

    pdf
}

/// Detached CMS/PKCS#7 over `data`. Detached (eContent absent) with signed
/// attributes (incl. messageDigest) — the default for `openssl cms -sign`.
/// `-indef` switches the output from definite-length DER to BER
/// indefinite-length streaming encoding (`30 80 … 00 00`).
fn openssl_cms_sign(
    data: &[u8],
    cert: &Path,
    key: &Path,
    tmp: &Path,
    md: &str,
    indef: bool,
) -> Vec<u8> {
    let data_path = tmp.join("signed-bytes.bin");
    let out_path = tmp.join("sig.der");
    std::fs::write(&data_path, data).expect("write data");
    let mut cmd = Command::new("openssl");
    cmd.args(["cms", "-sign", "-binary", "-md", md, "-outform", "DER", "-nosmimecap"]);
    if indef {
        cmd.arg("-indef");
    }
    let status = cmd
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
fn assemble_with_placeholders(content: &str) -> Vec<u8> {
    let zeros = "0".repeat(CONTENTS_HEX_LEN);

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
PATH). For digital-signature *verification* tests (issues #17 and #39).

| File | Expected status | How |
|---|---|---|
| `signed-valid.pdf` | **Verified** | Self-signed detached CMS over the full ByteRange (SHA-256, DER) |
| `signed-sha1.pdf` | **Verified** | Same, `openssl cms -md sha1` |
| `signed-sha384.pdf` | **Verified** | Same, `openssl cms -md sha384` |
| `signed-sha512.pdf` | **Verified** | Same, `openssl cms -md sha512` |
| `signed-ber.pdf` | **Verified** | Same as `signed-valid.pdf` but BER indefinite-length CMS (`openssl cms -indef` — the framing Adobe emits) |
| `signed-ber-tampered.pdf` | **Invalid** | `signed-ber.pdf` with one byte flipped inside the signed region |
| `signed-modified-after.pdf` | **ModifiedAfter** | `signed-valid.pdf` + bytes appended past the signature's coverage (signed bytes untouched) |
| `signed-tampered.pdf` | **Invalid** | `signed-valid.pdf` with one byte flipped inside the signed region |
| `live-test-verified.pdf` | **Verified** | For manual (live) testing — open it in Tumbler; its page text states the expected badge and the regeneration command |

## Important: intact, not trusted

The certificate is a throwaway self-signed test cert. A **Verified** result means
the signature is cryptographically *intact* (the signed bytes are unchanged) — it
does **not** mean the signer's identity is trusted. Trust-chain/revocation
validation is intentionally out of scope for this slice.
"#;
    std::fs::write(path, body).expect("write README");
    println!("wrote {}", path.display());
}

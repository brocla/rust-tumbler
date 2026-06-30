//! Digital-signature verification (read-only "verify-and-warn" slice, issue #17).
//!
//! Detects signature fields in a PDF and verifies their **integrity** — that the
//! bytes covered by each signature's `/ByteRange` still hash to the digest sealed
//! in its CMS/PKCS#7 blob, and that nothing was appended after a signature's
//! coverage. This proves *the signed bytes are unchanged*.
//!
//! It deliberately does NOT validate **trust** (certificate chain, revocation,
//! validity dates) — that's a larger follow-up. So a "verified" result means the
//! signature is cryptographically *intact*, not that we trust who signed it. All
//! wording must respect that distinction.
//!
//! `verify_signatures_from_path` is free of `AppState`/Tauri so it is
//! unit-testable and reusable from non-Tauri code (e.g. a future CLI, #5).

use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use der::{Decode, Encode};
use lopdf::{Document, Object};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::State;

#[derive(Serialize, Default, PartialEq, Eq, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub enum SignatureStatus {
    /// No signature fields — no badge.
    #[default]
    Unsigned,
    /// >= 1 signature, all intact, none modified after signing.
    Verified,
    /// Signed, signed bytes intact, but bytes were appended after signing.
    ModifiedAfter,
    /// A signature failed its integrity check or couldn't be parsed.
    Invalid,
}

#[derive(Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SignatureEntry {
    /// Signer common name from the CMS certificate, falling back to the sig
    /// dict's `/Name`; "" if neither is present.
    pub signer_name: String,
    pub reason: String,
    pub location: String,
    /// `/M` signing time, display-ready (leading "D:" stripped).
    pub signing_time: String,
    /// True if this signature's `/ByteRange` digest matches the CMS digest.
    pub integrity_ok: bool,
    /// True if bytes were appended after this signature's coverage.
    pub modified_after: bool,
}

#[derive(Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInfo {
    pub count: u32,
    pub signatures: Vec<SignatureEntry>,
    pub status: SignatureStatus,
}

#[tauri::command]
pub fn get_signature_info(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<SignatureInfo, String> {
    get_signature_info_impl(&state, doc_id).map_err(String::from)
}

fn get_signature_info_impl(state: &AppState, doc_id: String) -> Result<SignatureInfo, AppError> {
    let file_path = {
        let entry = state.get_document(&doc_id)?;
        let entry = lock_mutex(&entry)?;
        entry.file_path.clone()
    };
    Ok(verify_signatures_from_path(&file_path))
}

/// Read the file and verify every signature it declares. Pure and infallible at
/// the boundary — an unreadable/unparsable file reports as `Unsigned` (we can't
/// see any signatures), never an error.
pub fn verify_signatures_from_path(file_path: &str) -> SignatureInfo {
    match std::fs::read(file_path) {
        Ok(bytes) => verify_signatures(&bytes),
        Err(_) => SignatureInfo::default(),
    }
}

fn verify_signatures(bytes: &[u8]) -> SignatureInfo {
    let Ok(doc) = Document::load_mem(bytes) else {
        return SignatureInfo::default();
    };

    let sig_dicts = collect_signature_dicts(&doc);
    let signatures: Vec<SignatureEntry> = sig_dicts
        .iter()
        .map(|dict| verify_one(bytes, &doc, dict))
        .collect();

    let status = aggregate_status(&signatures);
    SignatureInfo {
        count: signatures.len() as u32,
        signatures,
        status,
    }
}

fn aggregate_status(sigs: &[SignatureEntry]) -> SignatureStatus {
    if sigs.is_empty() {
        SignatureStatus::Unsigned
    } else if sigs.iter().any(|s| !s.integrity_ok) {
        SignatureStatus::Invalid
    } else if sigs.iter().any(|s| s.modified_after) {
        SignatureStatus::ModifiedAfter
    } else {
        SignatureStatus::Verified
    }
}

/// Resolve the signature dictionaries from the AcroForm: each `/Fields` entry
/// that is a signature field (`/FT /Sig`) and has a value (`/V`) whose
/// dictionary carries a `/ByteRange`.
fn collect_signature_dicts(doc: &Document) -> Vec<lopdf::Dictionary> {
    let mut out = Vec::new();

    let acro = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
        .and_then(|o| resolve_dict(doc, o));
    let Some(acro) = acro else { return out };

    let Ok(fields) = acro.get(b"Fields").and_then(|o| match o {
        Object::Array(a) => Ok(a.clone()),
        Object::Reference(id) => doc
            .get_object(*id)
            .and_then(|o| o.as_array().cloned()),
        _ => Ok(Vec::new()),
    }) else {
        return out;
    };

    for field_ref in fields {
        let Some(field) = resolve_dict(doc, &field_ref) else { continue };
        let is_sig = field
            .get(b"FT")
            .ok()
            .and_then(|o| o.as_name_str().ok())
            == Some("Sig");
        if !is_sig {
            continue;
        }
        if let Some(v) = field.get(b"V").ok().and_then(|o| resolve_dict(doc, o)) {
            if v.has(b"ByteRange") {
                out.push(v);
            }
        }
    }

    out
}

fn resolve_dict(doc: &Document, obj: &Object) -> Option<lopdf::Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d.clone()),
        Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_dict().ok().cloned()),
        _ => None,
    }
}

fn verify_one(bytes: &[u8], _doc: &Document, sig: &lopdf::Dictionary) -> SignatureEntry {
    let reason = dict_text(sig, b"Reason");
    let location = dict_text(sig, b"Location");
    let signing_time = strip_date_prefix(&dict_text(sig, b"M"));

    let byte_range = read_byte_range(sig);
    let contents = sig
        .get(b"Contents")
        .ok()
        .and_then(|o| match o {
            Object::String(b, _) => Some(b.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let integrity_ok = match &byte_range {
        Some(br) => match (digest_byte_range(bytes, br), cms_message_digest(&contents)) {
            (Some(computed), Some(sealed)) => computed == sealed,
            _ => false,
        },
        None => false,
    };

    // Bytes after a signature's coverage are a post-signing incremental update.
    let modified_after = byte_range
        .as_ref()
        .map(|br| coverage_end(br) < bytes.len())
        .unwrap_or(false);

    let signer_name = cms_signer_cn(&contents).unwrap_or_else(|| dict_text(sig, b"Name"));

    SignatureEntry {
        signer_name,
        reason,
        location,
        signing_time,
        integrity_ok,
        modified_after,
    }
}

fn read_byte_range(sig: &lopdf::Dictionary) -> Option<Vec<i64>> {
    let arr = sig.get(b"ByteRange").ok()?.as_array().ok()?;
    let nums: Vec<i64> = arr.iter().filter_map(|o| o.as_i64().ok()).collect();
    if nums.len() == 4 && nums.iter().all(|&n| n >= 0) {
        Some(nums)
    } else {
        None
    }
}

fn coverage_end(br: &[i64]) -> usize {
    (br[2] + br[3]).max(0) as usize
}

/// SHA-256 over the two spans a `/ByteRange [a b c d]` covers: `[a, a+b)` and
/// `[c, c+d)` — everything except the `/Contents` hole.
fn digest_byte_range(bytes: &[u8], br: &[i64]) -> Option<Vec<u8>> {
    let (a, b, c, d) = (br[0] as usize, br[1] as usize, br[2] as usize, br[3] as usize);
    let end1 = a.checked_add(b)?;
    let end2 = c.checked_add(d)?;
    if end1 > bytes.len() || end2 > bytes.len() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(&bytes[a..end1]);
    h.update(&bytes[c..end2]);
    Some(h.finalize().to_vec())
}

const MESSAGE_DIGEST_OID: const_oid::ObjectIdentifier =
    const_oid::ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const COMMON_NAME_OID: const_oid::ObjectIdentifier =
    const_oid::ObjectIdentifier::new_unwrap("2.5.4.3");

/// The total length of the first DER TLV in `b`, so a zero-padded `/Contents`
/// placeholder (the signature is sized smaller than its reserved space) can be
/// trimmed to exactly the CMS blob before strict DER parsing.
fn der_total_len(b: &[u8]) -> Option<usize> {
    if b.len() < 2 {
        return None;
    }
    let len_byte = b[1];
    if len_byte < 0x80 {
        Some(2 + len_byte as usize)
    } else {
        let n = (len_byte & 0x7f) as usize;
        if n == 0 || n > 4 || b.len() < 2 + n {
            return None;
        }
        let mut len = 0usize;
        for &byte in &b[2..2 + n] {
            len = (len << 8) | byte as usize;
        }
        Some(2 + n + len)
    }
}

fn parse_signed_data(contents: &[u8]) -> Option<cms::signed_data::SignedData> {
    let total = der_total_len(contents)?;
    let der = contents.get(..total)?;
    let ci = cms::content_info::ContentInfo::from_der(der).ok()?;
    ci.content.decode_as::<cms::signed_data::SignedData>().ok()
}

/// The `messageDigest` signed attribute from the first SignerInfo — the digest
/// the signer sealed over the document's signed bytes.
fn cms_message_digest(contents: &[u8]) -> Option<Vec<u8>> {
    let sd = parse_signed_data(contents)?;
    let signer = sd.signer_infos.0.iter().next()?;
    let attrs = signer.signed_attrs.as_ref()?;
    for attr in attrs.iter() {
        if attr.oid == MESSAGE_DIGEST_OID {
            let value = attr.values.iter().next()?;
            let octet = value.decode_as::<der::asn1::OctetString>().ok()?;
            return Some(octet.as_bytes().to_vec());
        }
    }
    None
}

/// Best-effort signer common name: the CN of the first certificate in the CMS.
/// (Trust/identity validation is out of scope; this is for display only.)
fn cms_signer_cn(contents: &[u8]) -> Option<String> {
    let sd = parse_signed_data(contents)?;
    let certs = sd.certificates.as_ref()?;
    for choice in certs.0.iter() {
        if let cms::cert::CertificateChoices::Certificate(cert) = choice {
            if let Some(cn) = first_common_name(&cert.tbs_certificate.subject) {
                return Some(cn);
            }
        }
    }
    None
}

fn first_common_name(name: &x509_cert::name::Name) -> Option<String> {
    for rdn in name.0.iter() {
        for atv in rdn.0.iter() {
            if atv.oid == COMMON_NAME_OID {
                if let Ok(der) = atv.value.to_der() {
                    // The value is a DirectoryString (UTF8String/PrintableString…);
                    // decode generically and keep the printable tail.
                    if let Ok(s) = der::asn1::Utf8StringRef::from_der(&der) {
                        return Some(s.as_str().to_string());
                    }
                    if let Ok(s) = der::asn1::PrintableStringRef::from_der(&der) {
                        return Some(s.as_str().to_string());
                    }
                }
            }
        }
    }
    None
}

fn dict_text(dict: &lopdf::Dictionary, key: &[u8]) -> String {
    dict.get(key)
        .ok()
        .and_then(|o| match o {
            Object::String(b, _) => Some(String::from_utf8_lossy(b).into_owned()),
            Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Turn a PDF date `D:YYYYMMDDHHMMSS+ZZ'zz'` into something a bit friendlier for
/// display by dropping the `D:` prefix. (Full date formatting is left to later.)
fn strip_date_prefix(s: &str) -> String {
    s.strip_prefix("D:").unwrap_or(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!(
            "{}/tests/fixtures/signatures/{name}",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    /// The valid fixture verifies: one intact signature, not modified after.
    #[test]
    fn valid_signature_is_verified() {
        let info = verify_signatures_from_path(&fixture("signed-valid.pdf"));
        assert_eq!(info.count, 1);
        assert_eq!(info.status, SignatureStatus::Verified);
        let sig = &info.signatures[0];
        assert!(sig.integrity_ok);
        assert!(!sig.modified_after);
        assert_eq!(sig.signer_name, "Tumbler Test Signer");
        assert_eq!(sig.reason, "Demonstration");
    }

    /// Bytes appended after signing: signed bytes still intact, but coverage no
    /// longer reaches EOF.
    #[test]
    fn appended_bytes_report_modified_after() {
        let info = verify_signatures_from_path(&fixture("signed-modified-after.pdf"));
        assert_eq!(info.count, 1);
        assert_eq!(info.status, SignatureStatus::ModifiedAfter);
        assert!(info.signatures[0].integrity_ok);
        assert!(info.signatures[0].modified_after);
    }

    /// A flipped byte inside the signed region breaks the digest match.
    #[test]
    fn tampered_signed_bytes_report_invalid() {
        let info = verify_signatures_from_path(&fixture("signed-tampered.pdf"));
        assert_eq!(info.count, 1);
        assert_eq!(info.status, SignatureStatus::Invalid);
        assert!(!info.signatures[0].integrity_ok);
    }

    /// The plain unsigned fixture has no signature fields.
    #[test]
    fn unsigned_document_reports_unsigned() {
        let info = verify_signatures_from_path(crate::fixture_path().to_str().unwrap());
        assert_eq!(info.count, 0);
        assert_eq!(info.status, SignatureStatus::Unsigned);
        assert!(info.signatures.is_empty());
    }

    /// A missing/unreadable file reports Unsigned, never an error.
    #[test]
    fn missing_file_reports_unsigned() {
        let info = verify_signatures_from_path("does-not-exist-xyz.pdf");
        assert_eq!(info.status, SignatureStatus::Unsigned);
    }

    #[test]
    fn aggregate_status_precedence() {
        let intact = |modified| SignatureEntry {
            integrity_ok: true,
            modified_after: modified,
            ..Default::default()
        };
        let broken = SignatureEntry { integrity_ok: false, ..Default::default() };

        assert_eq!(aggregate_status(&[]), SignatureStatus::Unsigned);
        assert_eq!(aggregate_status(&[intact(false)]), SignatureStatus::Verified);
        assert_eq!(aggregate_status(&[intact(true)]), SignatureStatus::ModifiedAfter);
        // Invalid wins over modified-after.
        assert_eq!(
            aggregate_status(&[intact(true), broken]),
            SignatureStatus::Invalid
        );
    }

    #[test]
    fn der_total_len_short_and_long_form() {
        // Short form: SEQUENCE, length 3 -> total 5.
        assert_eq!(der_total_len(&[0x30, 0x03, 1, 2, 3]), Some(5));
        // Long form: length encoded in 2 bytes (0x0102 = 258) -> total 4 + 258.
        assert_eq!(der_total_len(&[0x30, 0x82, 0x01, 0x02]), Some(4 + 258));
        assert_eq!(der_total_len(&[0x30]), None);
    }

    #[test]
    fn strip_date_prefix_drops_d_colon() {
        assert_eq!(strip_date_prefix("D:20240101000000Z"), "20240101000000Z");
        assert_eq!(strip_date_prefix("no prefix"), "no prefix");
    }
}

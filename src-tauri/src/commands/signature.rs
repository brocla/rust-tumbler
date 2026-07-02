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
use std::collections::HashSet;
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
    /// A signature's signed bytes were altered (digest mismatch).
    Invalid,
    /// >= 1 signature detected, but its integrity could not be checked in this
    /// build (e.g. a BER indefinite-length CMS blob or a digest algorithm we
    /// don't yet support). Honest "we see a signature but can't vouch for it" —
    /// deliberately distinct from `Invalid`, which means tampering.
    Unknown,
}

/// Outcome of one signature's integrity check.
#[derive(Serialize, Default, PartialEq, Eq, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Integrity {
    /// The signed bytes hash to the digest sealed in the CMS.
    Ok,
    /// The digest did not match — the signed bytes were altered.
    Failed,
    /// Could not be checked (unsupported CMS encoding/algorithm, missing data).
    #[default]
    Unknown,
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
    /// Whether this signature's `/ByteRange` digest matches the sealed CMS
    /// digest (`Ok`), doesn't (`Failed`), or couldn't be checked (`Unknown`).
    pub integrity: Integrity,
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
    // Verify the in-memory buffer, not the file on disk: with non-destructive
    // editing (issue #31) an unsaved edit must already report as breaking the
    // signature before the user saves.
    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;
    Ok(verify_signatures(&entry.buffer))
}

/// Read the file and verify every signature it declares. Pure and infallible at
/// the boundary — an unreadable/unparsable file reports as `Unsigned` (we can't
/// see any signatures), never an error.
/// Production verifies the in-memory buffer via `verify_signatures`; this path
/// form is kept for tests and CLI reuse.
#[allow(dead_code)]
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
    } else if sigs.iter().any(|s| s.integrity == Integrity::Failed) {
        // A genuine tamper detection outranks everything.
        SignatureStatus::Invalid
    } else if sigs.iter().any(|s| s.integrity == Integrity::Unknown) {
        // We saw a signature but couldn't fully check it — don't claim Verified.
        SignatureStatus::Unknown
    } else if sigs.iter().any(|s| s.modified_after) {
        SignatureStatus::ModifiedAfter
    } else {
        SignatureStatus::Verified
    }
}

/// Collect the signature *value* dictionaries in the document — any dict that
/// carries a `/ByteRange`. Real-world signatures show up in two places, both
/// handled here:
///
/// - The **AcroForm field tree**, where the signature field is frequently nested
///   under a parent's `/Kids` (and `/FT` may be on the parent, or absent), so we
///   recurse rather than assuming a flat `/FT /Sig` at the top of `/Fields`.
/// - The Catalog **`/Perms`** entries — usage-rights (`/UR3`) and certification
///   (`/DocMDP`) signatures live here, outside `/Fields` entirely.
///
/// Keying off "has a `/ByteRange`" (rather than `/FT`) reliably identifies a
/// signature value dict in both cases. References are de-duplicated so a
/// signature reachable from both a field and `/Perms` is reported once.
fn collect_signature_dicts(doc: &Document) -> Vec<lopdf::Dictionary> {
    let mut out = Vec::new();
    let mut seen: HashSet<lopdf::ObjectId> = HashSet::new();

    // AcroForm field tree (iterative, recursing /Kids, with a cycle guard).
    if let Some(acro) = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
        .and_then(|o| resolve_dict(doc, o))
    {
        let mut stack = resolve_array(doc, acro.get(b"Fields").ok());
        let mut guard = 0;
        while let Some(node) = stack.pop() {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            let Some(field) = resolve_dict(doc, &node) else { continue };
            // The field's value (/V) is usually the signature dict; occasionally
            // the field node itself carries the ByteRange.
            if let Ok(v) = field.get(b"V") {
                push_if_sig(doc, v, &mut out, &mut seen);
            }
            if field.has(b"ByteRange") {
                push_if_sig(doc, &node, &mut out, &mut seen);
            }
            stack.extend(resolve_array(doc, field.get(b"Kids").ok()));
        }
    }

    // Catalog /Perms: /UR3 (usage rights), /DocMDP (certification), etc.
    if let Some(perms) = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Perms").ok())
        .and_then(|o| resolve_dict(doc, o))
    {
        for (_key, v) in perms.iter() {
            push_if_sig(doc, v, &mut out, &mut seen);
        }
    }

    out
}

/// If `obj` resolves to a dict with a `/ByteRange` (and hasn't been seen before,
/// by reference), append it.
fn push_if_sig(
    doc: &Document,
    obj: &Object,
    out: &mut Vec<lopdf::Dictionary>,
    seen: &mut HashSet<lopdf::ObjectId>,
) {
    if let Ok(id) = obj.as_reference() {
        if !seen.insert(id) {
            return;
        }
    }
    if let Some(dict) = resolve_dict(doc, obj) {
        if dict.has(b"ByteRange") {
            out.push(dict);
        }
    }
}

/// Resolve an optional object to a Vec of its elements, following a reference to
/// an array. Returns empty for anything that isn't an array (or ref to one).
fn resolve_array(doc: &Document, obj: Option<&Object>) -> Vec<Object> {
    match obj {
        Some(Object::Array(a)) => a.clone(),
        Some(Object::Reference(id)) => doc
            .get_object(*id)
            .and_then(|o| o.as_array().cloned())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
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

    let integrity = check_integrity(bytes, byte_range.as_deref(), &contents);

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
        integrity,
        modified_after,
    }
}

/// Check one signature's integrity: does the CMS's sealed message digest match
/// a fresh hash of the `/ByteRange`? Returns `Unknown` (not `Failed`) whenever we
/// can't perform the check — no byte range, an unparsable CMS (e.g. Adobe's BER
/// indefinite-length encoding, which the strict-DER parser rejects), or a digest
/// algorithm this build doesn't compute. Only a real digest mismatch is `Failed`.
fn check_integrity(bytes: &[u8], byte_range: Option<&[i64]>, contents: &[u8]) -> Integrity {
    let Some(br) = byte_range else {
        return Integrity::Unknown;
    };
    let Some(sd) = parse_signed_data(contents) else {
        return Integrity::Unknown;
    };
    let Some(signer) = sd.signer_infos.0.iter().next() else {
        return Integrity::Unknown;
    };
    // Only SHA-256 message digests are computed here; anything else is reported
    // honestly as unverified rather than guessed at.
    if signer.digest_alg.oid != SHA256_OID {
        return Integrity::Unknown;
    }
    let (Some(sealed), Some(computed)) =
        (message_digest_attr(signer), digest_byte_range(bytes, br))
    else {
        return Integrity::Unknown;
    };
    if computed == sealed {
        Integrity::Ok
    } else {
        Integrity::Failed
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
const SHA256_OID: const_oid::ObjectIdentifier =
    const_oid::ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");

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

/// The `messageDigest` signed attribute of a SignerInfo — the digest the signer
/// sealed over the document's signed bytes.
fn message_digest_attr(signer: &cms::signed_data::SignerInfo) -> Option<Vec<u8>> {
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
        assert_eq!(sig.integrity, Integrity::Ok);
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
        assert_eq!(info.signatures[0].integrity, Integrity::Ok);
        assert!(info.signatures[0].modified_after);
    }

    /// A flipped byte inside the signed region breaks the digest match.
    #[test]
    fn tampered_signed_bytes_report_invalid() {
        let info = verify_signatures_from_path(&fixture("signed-tampered.pdf"));
        assert_eq!(info.count, 1);
        assert_eq!(info.status, SignatureStatus::Invalid);
        assert_eq!(info.signatures[0].integrity, Integrity::Failed);
    }

    /// A real-world Adobe Reader-enabled form (IRS f8946): the signature is
    /// nested under a parent field's /Kids and lives in the Catalog /Perms /UR3,
    /// and its CMS uses BER indefinite-length encoding the strict-DER parser
    /// can't read. We must still DETECT it (so the badge shows) and report it
    /// honestly as Unknown — signed but not verifiable here — never Unsigned and
    /// never Invalid (which would falsely imply tampering).
    #[test]
    fn adobe_ur3_form_is_detected_but_unknown() {
        let path = format!("{}/tests/fixtures/forms/f8946.pdf", env!("CARGO_MANIFEST_DIR"));
        let info = verify_signatures_from_path(&path);
        assert_eq!(info.count, 1, "signature should be detected");
        assert_eq!(info.status, SignatureStatus::Unknown);
        assert_eq!(info.signatures[0].integrity, Integrity::Unknown);
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
        let entry = |integrity, modified| SignatureEntry {
            integrity,
            modified_after: modified,
            ..Default::default()
        };
        let ok = |modified| entry(Integrity::Ok, modified);
        let failed = || entry(Integrity::Failed, false);
        let unknown = || entry(Integrity::Unknown, false);

        assert_eq!(aggregate_status(&[]), SignatureStatus::Unsigned);
        assert_eq!(aggregate_status(&[ok(false)]), SignatureStatus::Verified);
        assert_eq!(aggregate_status(&[ok(true)]), SignatureStatus::ModifiedAfter);
        assert_eq!(aggregate_status(&[unknown()]), SignatureStatus::Unknown);
        // Failed (real tamper) outranks Unknown; Unknown outranks modified-after.
        assert_eq!(aggregate_status(&[unknown(), failed()]), SignatureStatus::Invalid);
        assert_eq!(aggregate_status(&[ok(true), unknown()]), SignatureStatus::Unknown);
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

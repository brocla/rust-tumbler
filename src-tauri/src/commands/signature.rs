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
use lopdf::{Document, Object};
use serde::Serialize;
use sha2::Digest;
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
    /// build (e.g. an unparsable CMS blob, a digest algorithm we don't compute,
    /// or a signature without signed attributes). Honest "we see a signature
    /// but can't vouch for it" — deliberately distinct from `Invalid`, which
    /// means tampering.
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

    // One CryptoAPI parse serves both the integrity check and the display name.
    let cms = parse_cms(&contents);
    let integrity = check_integrity(bytes, byte_range.as_deref(), cms.as_ref());

    // Bytes after a signature's coverage are a post-signing incremental update.
    let modified_after = byte_range
        .as_ref()
        .map(|br| coverage_end(br) < bytes.len())
        .unwrap_or(false);

    let signer_name = cms
        .and_then(|c| c.signer_cn)
        .unwrap_or_else(|| dict_text(sig, b"Name"));

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
/// can't perform the check — no byte range, an unparsable CMS, a digest
/// algorithm this build doesn't compute, or a signature without signed
/// attributes (no sealed messageDigest to compare against). Only a real digest
/// mismatch is `Failed`: a false `Verified` is worse than an honest `Unknown`,
/// and a false `Invalid` (implying tampering) is worst of all.
fn check_integrity(bytes: &[u8], byte_range: Option<&[i64]>, cms: Option<&CmsSignerData>) -> Integrity {
    let Some(br) = byte_range else {
        return Integrity::Unknown;
    };
    let Some(cms) = cms else {
        return Integrity::Unknown;
    };
    let Some(alg) = digest_alg_from_oid(&cms.digest_alg_oid) else {
        return Integrity::Unknown;
    };
    let (Some(sealed), Some(computed)) =
        (cms.message_digest.as_deref(), digest_byte_range(bytes, br, alg))
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

/// The digest algorithms this build can recompute over the `/ByteRange`.
/// Anything else (e.g. MD5) reports honestly as `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DigestAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

/// Map a SignerInfo digestAlgorithm OID to a hash we compute. Some signers put
/// the *signature* algorithm OID (sha…WithRSAEncryption) in the digest slot, so
/// those are accepted as aliases for the hash they name.
fn digest_alg_from_oid(oid: &str) -> Option<DigestAlg> {
    match oid {
        "1.3.14.3.2.26" | "1.2.840.113549.1.1.5" => Some(DigestAlg::Sha1),
        "2.16.840.1.101.3.4.2.1" | "1.2.840.113549.1.1.11" => Some(DigestAlg::Sha256),
        "2.16.840.1.101.3.4.2.2" | "1.2.840.113549.1.1.12" => Some(DigestAlg::Sha384),
        "2.16.840.1.101.3.4.2.3" | "1.2.840.113549.1.1.13" => Some(DigestAlg::Sha512),
        _ => None,
    }
}

/// Hash of the two spans a `/ByteRange [a b c d]` covers: `[a, a+b)` and
/// `[c, c+d)` — everything except the `/Contents` hole.
fn digest_byte_range(bytes: &[u8], br: &[i64], alg: DigestAlg) -> Option<Vec<u8>> {
    let (a, b, c, d) = (br[0] as usize, br[1] as usize, br[2] as usize, br[3] as usize);
    let end1 = a.checked_add(b)?;
    let end2 = c.checked_add(d)?;
    if end1 > bytes.len() || end2 > bytes.len() {
        return None;
    }
    fn hash<D: Digest>(s1: &[u8], s2: &[u8]) -> Vec<u8> {
        let mut h = D::new();
        h.update(s1);
        h.update(s2);
        h.finalize().to_vec()
    }
    let (s1, s2) = (&bytes[a..end1], &bytes[c..end2]);
    Some(match alg {
        DigestAlg::Sha1 => hash::<sha1::Sha1>(s1, s2),
        DigestAlg::Sha256 => hash::<sha2::Sha256>(s1, s2),
        DigestAlg::Sha384 => hash::<sha2::Sha384>(s1, s2),
        DigestAlg::Sha512 => hash::<sha2::Sha512>(s1, s2),
    })
}

// ── CMS parsing via Windows CryptoAPI (issue #39) ───────────────────────────
//
// The CMS/PKCS#7 blob is parsed with CryptMsg* rather than a strict-DER Rust
// parser because real-world signatures — Adobe's in particular — use BER
// indefinite-length framing (`30 80 … 00 00`), which strict DER rejects.
// CryptoAPI decodes BER natively and hands back the SignerInfo (digest
// algorithm + sealed messageDigest attribute) and the embedded certificates
// generically, whatever the encoding. Windows-only, like the rest of Tumbler.

/// What the integrity check needs from a parsed CMS blob.
struct CmsSignerData {
    /// The SignerInfo digestAlgorithm, as a dotted OID string.
    digest_alg_oid: String,
    /// The sealed `messageDigest` signed attribute — the digest the signer
    /// computed over the `/ByteRange` bytes. Absent when the signature carries
    /// no signed attributes.
    message_digest: Option<Vec<u8>>,
    /// Display-only common name from the first embedded certificate with one.
    signer_cn: Option<String>,
}

use windows::Win32::Security::Cryptography::{
    CertCreateCertificateContext, CertFreeCertificateContext, CertGetNameStringW,
    CryptMsgClose, CryptMsgGetParam, CryptMsgOpenToDecode, CryptMsgUpdate,
    CERT_NAME_ATTR_TYPE, CERT_QUERY_ENCODING_TYPE, CMSG_CERT_COUNT_PARAM, CMSG_CERT_PARAM,
    CMSG_SIGNER_INFO, CMSG_SIGNER_INFO_PARAM, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};

const ENCODING: u32 = PKCS_7_ASN_ENCODING.0 | X509_ASN_ENCODING.0;
const MESSAGE_DIGEST_OID: &str = "1.2.840.113549.1.9.4";

/// Closes the CryptMsg handle on drop, so every early `?` return still frees it.
struct MsgGuard(*mut core::ffi::c_void);

impl Drop for MsgGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CryptMsgClose(Some(self.0));
        }
    }
}

/// Parse the `/Contents` CMS blob (DER or BER) and extract the SignerInfo
/// facts the integrity check needs. Returns `None` for anything unparsable —
/// the caller degrades that to `Integrity::Unknown`, never `Failed`.
fn parse_cms(contents: &[u8]) -> Option<CmsSignerData> {
    // Trim the zero padding after the blob (the signature is written smaller
    // than its reserved /Contents space) to exactly one BER/DER TLV.
    let total = ber_total_len(contents)?;
    let blob = contents.get(..total)?;
    unsafe {
        let msg = CryptMsgOpenToDecode(ENCODING, 0, 0, None, None, None);
        if msg.is_null() {
            return None;
        }
        let _guard = MsgGuard(msg);
        CryptMsgUpdate(msg, Some(blob), true).ok()?;

        let signer_buf = get_msg_param(msg, CMSG_SIGNER_INFO_PARAM, 0)?;
        // The buffer is a CMSG_SIGNER_INFO whose internal pointers point back
        // into the same allocation; get_msg_param over-aligns it for this.
        let info = &*(signer_buf.as_ptr() as *const CMSG_SIGNER_INFO);
        let digest_alg_oid = pstr_to_string(info.HashAlgorithm.pszObjId)?;
        let message_digest =
            find_auth_attr(info, MESSAGE_DIGEST_OID).and_then(|v| der_octet_string(&v));
        let signer_cn = first_cert_cn(msg);

        Some(CmsSignerData {
            digest_alg_oid,
            message_digest,
            signer_cn,
        })
    }
}

/// Two-call CryptMsgGetParam (size, then data). The buffer is backed by a
/// `u64` allocation so structured params (CMSG_SIGNER_INFO) are properly
/// aligned when the caller casts into them.
unsafe fn get_msg_param(
    msg: *const core::ffi::c_void,
    param: u32,
    index: u32,
) -> Option<Vec<u64>> {
    let mut size = 0u32;
    CryptMsgGetParam(msg, param, index, None, &mut size).ok()?;
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    CryptMsgGetParam(
        msg,
        param,
        index,
        Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
        &mut size,
    )
    .ok()?;
    Some(buf)
}

/// Bytes-view of a `get_msg_param` buffer (for byte-oriented params).
unsafe fn param_bytes(buf: &[u64], size: usize) -> &[u8] {
    std::slice::from_raw_parts(buf.as_ptr() as *const u8, size.min(buf.len() * 8))
}

fn pstr_to_string(p: windows::core::PSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe { std::ffi::CStr::from_ptr(p.0 as *const i8) }
        .to_str()
        .ok()
        .map(String::from)
}

/// The raw (DER-encoded) value of the first authenticated attribute with the
/// given OID, if present.
unsafe fn find_auth_attr(info: &CMSG_SIGNER_INFO, oid: &str) -> Option<Vec<u8>> {
    if info.AuthAttrs.rgAttr.is_null() {
        return None;
    }
    let attrs = std::slice::from_raw_parts(info.AuthAttrs.rgAttr, info.AuthAttrs.cAttr as usize);
    for attr in attrs {
        if pstr_to_string(attr.pszObjId).as_deref() == Some(oid)
            && attr.cValue > 0
            && !attr.rgValue.is_null()
        {
            let blob = &*attr.rgValue;
            if blob.pbData.is_null() {
                return None;
            }
            return Some(std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec());
        }
    }
    None
}

/// Display-only signer common name: the CN of the first embedded certificate
/// that has one (parity with the previous RustCrypto behaviour). Trust and
/// identity validation remain out of scope.
unsafe fn first_cert_cn(msg: *const core::ffi::c_void) -> Option<String> {
    let count_buf = get_msg_param(msg, CMSG_CERT_COUNT_PARAM, 0)?;
    let count_bytes = param_bytes(&count_buf, 4);
    let count = u32::from_le_bytes(count_bytes.try_into().ok()?);
    for i in 0..count {
        let Some(cert_buf) = get_msg_param(msg, CMSG_CERT_PARAM, i) else {
            continue;
        };
        // CryptMsgGetParam wrote `size` bytes; the u64 buffer may be up to 7
        // bytes longer, but CertCreateCertificateContext reads the encoded
        // length from the DER itself, so the tail padding is ignored.
        let encoded = param_bytes(&cert_buf, cert_buf.len() * 8);
        let ctx = CertCreateCertificateContext(CERT_QUERY_ENCODING_TYPE(ENCODING), encoded);
        if ctx.is_null() {
            continue;
        }
        let cn = cert_common_name(ctx);
        let _ = CertFreeCertificateContext(Some(ctx));
        if cn.is_some() {
            return cn;
        }
    }
    None
}

unsafe fn cert_common_name(
    ctx: *const windows::Win32::Security::Cryptography::CERT_CONTEXT,
) -> Option<String> {
    let cn_oid = c"2.5.4.3"; // szOID_COMMON_NAME
    let type_para = Some(cn_oid.as_ptr() as *const core::ffi::c_void);
    // Two-call pattern: length (incl. NUL), then the string itself.
    let len = CertGetNameStringW(ctx, CERT_NAME_ATTR_TYPE, 0, type_para, None);
    if len <= 1 {
        return None; // no CN attribute (CertGetNameStringW returns "" as 1)
    }
    let mut buf = vec![0u16; len as usize];
    let written = CertGetNameStringW(ctx, CERT_NAME_ATTR_TYPE, 0, type_para, Some(&mut buf));
    if written <= 1 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..written as usize - 1]))
}

/// The total length of the first BER/DER TLV in `b` — including BER
/// indefinite-length framing (constructed, length byte `0x80`, terminated by a
/// `00 00` end-of-contents pair), which Adobe emits. Used to trim a zero-padded
/// `/Contents` placeholder to exactly the CMS blob.
fn ber_total_len(b: &[u8]) -> Option<usize> {
    ber_tlv_end(b, 0, 0)
}

/// Offset just past the TLV starting at `at`. Recurses only into
/// indefinite-length values (whose end is where their children's EOC is);
/// definite lengths are skipped without descending.
fn ber_tlv_end(b: &[u8], at: usize, depth: u32) -> Option<usize> {
    if depth > 64 {
        return None;
    }
    let tag = *b.get(at)?;
    let mut i = at + 1;
    if tag & 0x1f == 0x1f {
        // High-tag-number form: continue while the continuation bit is set.
        while *b.get(i)? & 0x80 != 0 {
            i += 1;
        }
        i += 1;
    }
    let len_byte = *b.get(i)?;
    i += 1;
    if len_byte < 0x80 {
        return i.checked_add(len_byte as usize);
    }
    if len_byte == 0x80 {
        // Indefinite length: children until an end-of-contents (00 00) pair.
        loop {
            if *b.get(i)? == 0x00 && *b.get(i + 1)? == 0x00 {
                return Some(i + 2);
            }
            i = ber_tlv_end(b, i, depth + 1)?;
        }
    }
    let n = (len_byte & 0x7f) as usize;
    if n > 8 {
        return None;
    }
    let mut len = 0usize;
    for &byte in b.get(i..i + n)? {
        len = len.checked_mul(256)?.checked_add(byte as usize)?;
    }
    i.checked_add(n)?.checked_add(len)
}

/// Decode a DER OCTET STRING (tag 0x04), returning its contents.
fn der_octet_string(b: &[u8]) -> Option<Vec<u8>> {
    if *b.first()? != 0x04 {
        return None;
    }
    let len_byte = *b.get(1)?;
    let (start, len) = if len_byte < 0x80 {
        (2, len_byte as usize)
    } else {
        let n = (len_byte & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for &byte in b.get(2..2 + n)? {
            len = (len << 8) | byte as usize;
        }
        (2 + n, len)
    };
    b.get(start..start + len).map(|s| s.to_vec())
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
    /// and its CMS uses BER indefinite-length encoding. With CryptoAPI parsing
    /// (issue #39) this now fully VERIFIES — the headline acceptance criterion:
    /// integrity Ok, status Verified, not the old honest-but-unhelpful Unknown.
    #[test]
    fn adobe_ur3_ber_form_verifies() {
        let path = format!("{}/tests/fixtures/forms/f8946.pdf", env!("CARGO_MANIFEST_DIR"));
        let info = verify_signatures_from_path(&path);
        assert_eq!(info.count, 1, "signature should be detected");
        assert_eq!(info.signatures[0].integrity, Integrity::Ok);
        assert_eq!(info.status, SignatureStatus::Verified);
    }

    /// SHA-1, SHA-384 and SHA-512 signatures verify (issue #39) — previously
    /// anything but SHA-256 was honestly Unknown.
    #[test]
    fn non_sha256_digests_verify() {
        for name in ["signed-sha1.pdf", "signed-sha384.pdf", "signed-sha512.pdf"] {
            let info = verify_signatures_from_path(&fixture(name));
            assert_eq!(info.count, 1, "{name}: signature should be detected");
            assert_eq!(info.status, SignatureStatus::Verified, "{name}");
            assert_eq!(info.signatures[0].integrity, Integrity::Ok, "{name}");
            assert_eq!(info.signatures[0].signer_name, "Tumbler Test Signer", "{name}");
        }
    }

    /// A BER indefinite-length CMS (openssl -indef, the framing Adobe uses)
    /// verifies like its DER twin.
    #[test]
    fn ber_indefinite_length_cms_verifies() {
        let info = verify_signatures_from_path(&fixture("signed-ber.pdf"));
        assert_eq!(info.count, 1);
        assert_eq!(info.status, SignatureStatus::Verified);
        assert_eq!(info.signatures[0].integrity, Integrity::Ok);
    }

    /// The most important test in this change: a genuinely tampered BER
    /// signature must report Invalid — BER support must not have widened
    /// "parses now" into "verifies always".
    #[test]
    fn tampered_ber_signature_reports_invalid() {
        let info = verify_signatures_from_path(&fixture("signed-ber-tampered.pdf"));
        assert_eq!(info.count, 1);
        assert_eq!(info.status, SignatureStatus::Invalid);
        assert_eq!(info.signatures[0].integrity, Integrity::Failed);
    }

    /// Unparsable CMS bytes still degrade to Unknown, never a false Invalid.
    #[test]
    fn unparsable_cms_degrades_to_unknown() {
        assert!(parse_cms(b"not a CMS blob at all").is_none());
        assert!(parse_cms(&[]).is_none());
        // A syntactically-valid TLV that isn't a CMS message.
        assert!(parse_cms(&[0x30, 0x03, 0x02, 0x01, 0x01]).is_none());
        let integrity = check_integrity(b"irrelevant", Some(&[0, 4, 6, 4]), None);
        assert_eq!(integrity, Integrity::Unknown);
    }

    /// An unsupported digest algorithm OID degrades to Unknown.
    #[test]
    fn unsupported_digest_algorithm_is_unknown() {
        let cms = CmsSignerData {
            digest_alg_oid: "1.2.840.113549.2.5".to_string(), // MD5
            message_digest: Some(vec![0; 16]),
            signer_cn: None,
        };
        let integrity = check_integrity(b"0123456789", Some(&[0, 4, 6, 4]), Some(&cms));
        assert_eq!(integrity, Integrity::Unknown);
    }

    #[test]
    fn digest_alg_oid_mapping_covers_hash_and_rsa_forms() {
        assert_eq!(digest_alg_from_oid("1.3.14.3.2.26"), Some(DigestAlg::Sha1));
        assert_eq!(digest_alg_from_oid("2.16.840.1.101.3.4.2.1"), Some(DigestAlg::Sha256));
        assert_eq!(digest_alg_from_oid("2.16.840.1.101.3.4.2.2"), Some(DigestAlg::Sha384));
        assert_eq!(digest_alg_from_oid("2.16.840.1.101.3.4.2.3"), Some(DigestAlg::Sha512));
        // sha…WithRSAEncryption aliases seen in the digest slot in the wild.
        assert_eq!(digest_alg_from_oid("1.2.840.113549.1.1.11"), Some(DigestAlg::Sha256));
        assert_eq!(digest_alg_from_oid("1.2.840.113549.2.5"), None); // MD5
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
    fn ber_total_len_short_long_and_indefinite_forms() {
        // Short form: SEQUENCE, length 3 -> total 5.
        assert_eq!(ber_total_len(&[0x30, 0x03, 1, 2, 3]), Some(5));
        // Long form: length encoded in 2 bytes (0x0102 = 258) -> total 4 + 258.
        assert_eq!(ber_total_len(&[0x30, 0x82, 0x01, 0x02]), Some(4 + 258));
        assert_eq!(ber_total_len(&[0x30]), None);
        // Indefinite form (BER): SEQUENCE containing one definite child, then
        // EOC (00 00). Trailing zero padding beyond the EOC is not counted.
        let ber = [0x30, 0x80, 0x04, 0x02, 0xAA, 0xBB, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(ber_total_len(&ber), Some(8));
        // Nested indefinite inside indefinite.
        let nested = [0x30, 0x80, 0x30, 0x80, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(ber_total_len(&nested), Some(8));
        // Unterminated indefinite -> None, not a panic or a bogus length.
        assert_eq!(ber_total_len(&[0x30, 0x80, 0x04, 0x01, 0xAA]), None);
    }

    #[test]
    fn der_octet_string_decodes_short_and_long_form() {
        assert_eq!(der_octet_string(&[0x04, 0x02, 0xAA, 0xBB]), Some(vec![0xAA, 0xBB]));
        let mut long = vec![0x04, 0x81, 0x03];
        long.extend_from_slice(&[1, 2, 3]);
        assert_eq!(der_octet_string(&long), Some(vec![1, 2, 3]));
        assert_eq!(der_octet_string(&[0x30, 0x01, 0x00]), None); // not an OCTET STRING
        assert_eq!(der_octet_string(&[0x04, 0x05, 1, 2]), None); // truncated
    }

    #[test]
    fn strip_date_prefix_drops_d_colon() {
        assert_eq!(strip_date_prefix("D:20240101000000Z"), "20240101000000Z");
        assert_eq!(strip_date_prefix("no prefix"), "no prefix");
    }
}

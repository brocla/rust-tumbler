//! Password-protected PDFs (issues #12, #57).
//!
//! At open time the encrypted file bytes are decrypted to a plaintext PDF and
//! *that* becomes `DocEntry.buffer`, so every buffer-model feature (metadata,
//! page ops, compression, forms, text layer) works on a password-protected
//! document exactly as on an ordinary one. The password stays on the
//! `DocEntry` (in memory only) and Save / Save As re-encrypt the buffer with
//! it (AES-256) before writing, so the file on disk keeps its password.
//!
//! `remove_password` is the one explicit way to drop the protection: it
//! clears the stored password so the next Save writes a plaintext file.
//! `set_password` (issue #58) is its mirror: it stores a password so the next
//! Save writes an AES-256-encrypted file — on an already-protected document it
//! simply replaces the stored password (the buffer is plaintext either way).

use crate::error::AppError;
use crate::state::{lock_mutex, AppState};
use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{EncryptionState, EncryptionVersion, Permissions};
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::{Emitter, State};

/// Decrypts an encrypted PDF into plaintext bytes, also returning the
/// document's permission bits so a later save can re-encrypt with them.
/// `password` is whatever string opened the document — the empty string for a
/// file that is owner-password-protected but has no user password.
pub fn decrypt_to_plaintext(
    bytes: &[u8],
    password: &str,
) -> Result<(Vec<u8>, Permissions), AppError> {
    // Decrypt-during-parse (LoadOptions::with_password), not load_mem +
    // decrypt(): without the password the parser can't read the encrypted
    // object streams, so a post-hoc decrypt() would see almost no objects.
    // The loader authenticates, decrypts every object, and drops /Encrypt
    // from the trailer, leaving `encryption_state` describing the original
    // scheme (which carries the permission bits).
    let mut doc = lopdf::Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions::with_password(password),
    )
    .map_err(|e| AppError::lopdf("Failed to decrypt PDF", e))?;
    let permissions = doc
        .encryption_state
        .as_ref()
        .map(|s| s.permissions())
        .unwrap_or_else(Permissions::all);
    // `save_to` rewrites every object under one fresh cross-reference table;
    // the incremental-update trailer keys would dangle (see forms.rs).
    doc.trailer.remove(b"Prev");
    doc.trailer.remove(b"XRefStm");
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize decrypted PDF", e))?;
    Ok((out, permissions))
}

/// Encrypts plaintext PDF bytes with AES-256 (the strongest standard scheme;
/// the original file's algorithm is not preserved), using `password` as both
/// the user and owner password and carrying over the original permission
/// bits. Save calls this so a password-protected document stays protected on
/// disk.
pub fn encrypt_with_password(
    bytes: &[u8],
    password: &str,
    permissions: Permissions,
) -> Result<Vec<u8>, AppError> {
    let mut doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| AppError::lopdf("Failed to parse PDF for encryption", e))?;

    let mut key = [0u8; 32];
    getrandom::fill(&mut key)
        .map_err(|e| AppError::Other(format!("Failed to generate an encryption key: {e}")))?;

    let crypt_filters: BTreeMap<Vec<u8>, Arc<dyn CryptFilter>> = BTreeMap::from([(
        b"StdCF".to_vec(),
        Arc::new(Aes256CryptFilter) as Arc<dyn CryptFilter>,
    )]);
    let version = EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters,
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: password,
        user_password: password,
        permissions,
    };
    let enc_state = EncryptionState::try_from(version)
        .map_err(|e| AppError::lopdf("Failed to build encryption state", e))?;
    doc.encrypt(&enc_state)
        .map_err(|e| AppError::lopdf("Failed to encrypt PDF", e))?;

    doc.trailer.remove(b"Prev");
    doc.trailer.remove(b"XRefStm");
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| AppError::io("Failed to serialize encrypted PDF", e))?;
    Ok(out)
}

/// Drops the document's password protection: the next Save / Save As writes a
/// plaintext PDF that opens without a password. The buffer (already plaintext
/// since open) is untouched; only the stored password is cleared, and the
/// document goes dirty because what Save would write has changed.
#[tauri::command]
pub fn remove_password(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    remove_password_impl(&state, &doc_id).map_err(String::from)?;
    let _ = app.emit(
        "document-dirty-changed",
        crate::commands::save::dirty_changed_payload(&state, doc_id, true),
    );
    Ok(())
}

pub(crate) fn remove_password_impl(state: &AppState, doc_id: &str) -> Result<(), AppError> {
    let entry_arc = state.get_document(doc_id)?;
    let mut entry = lock_mutex(&entry_arc)?;
    if !entry.encrypted {
        return Err(AppError::Other(
            "This document is not password-protected.".to_string(),
        ));
    }
    entry.encrypted = false;
    entry.password = None;
    entry.permissions = None;
    entry.dirty = true;
    Ok(())
}

/// Protects the document with `password` (AES-256, same string for the user
/// and owner password — issue #58): the next Save / Save As writes an
/// encrypted file that requires it to open. On an already-protected document
/// this just replaces the stored password. The buffer stays plaintext — like
/// `remove_password`, only what Save will write changes, so the document goes
/// dirty and nothing touches disk until the user saves.
#[tauri::command]
pub fn set_password(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    doc_id: String,
    password: String,
) -> Result<(), String> {
    set_password_impl(&state, &doc_id, &password).map_err(String::from)?;
    let _ = app.emit(
        "document-dirty-changed",
        crate::commands::save::dirty_changed_payload(&state, doc_id, true),
    );
    Ok(())
}

pub(crate) fn set_password_impl(
    state: &AppState,
    doc_id: &str,
    password: &str,
) -> Result<(), AppError> {
    // An empty user password means "opens without a prompt" — that's what
    // remove_password is for, and storing it would silently protect nothing.
    if password.is_empty() {
        return Err(AppError::Other("The password cannot be empty.".to_string()));
    }
    let entry_arc = state.get_document(doc_id)?;
    let mut entry = lock_mutex(&entry_arc)?;
    // A password change keeps the file's original permission bits; a newly
    // protected document allows everything (owner == user makes the bits
    // advisory anyway).
    if entry.permissions.is_none() {
        entry.permissions = Some(Permissions::all());
    }
    entry.encrypted = true;
    entry.password = Some(password.to_string());
    entry.dirty = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DocEntry;

    fn tmp_path(name: &str) -> String {
        std::env::temp_dir().join(name).to_string_lossy().into_owned()
    }

    /// AES-256 round trip of the exact pipeline Save uses: decrypt the
    /// encrypted fixture to plaintext, re-encrypt it, and confirm the result
    /// opens (lopdf and pdfium) only with the same password.
    #[test]
    fn decrypt_then_encrypt_round_trip_aes256() {
        let _guard = crate::test_pdfium_guard();
        let encrypted = std::fs::read(crate::encrypted_fixture_path()).expect("read fixture");

        let (plaintext, permissions) =
            decrypt_to_plaintext(&encrypted, crate::ENCRYPTED_FIXTURE_PASSWORD).expect("decrypt");
        let doc = lopdf::Document::load_mem(&plaintext).expect("parse plaintext");
        assert!(!doc.is_encrypted(), "plaintext must carry no /Encrypt");

        let re_encrypted =
            encrypt_with_password(&plaintext, crate::ENCRYPTED_FIXTURE_PASSWORD, permissions)
                .expect("encrypt");
        let doc = lopdf::Document::load_mem(&re_encrypted).expect("parse re-encrypted");
        assert!(doc.is_encrypted(), "re-encrypted bytes must carry /Encrypt");

        // pdfium (a fully independent implementation) must agree: rejected
        // without the password, opens with it.
        let pdfium = crate::test_pdfium();
        assert!(pdfium
            .load_pdf_from_byte_vec(re_encrypted.clone(), None)
            .is_err());
        let doc = pdfium
            .load_pdf_from_byte_vec(re_encrypted, Some(crate::ENCRYPTED_FIXTURE_PASSWORD))
            .expect("open re-encrypted with password");
        assert_eq!(doc.pages().len(), 1);
    }

    /// RC4 (V2/128-bit) files must decrypt too — build one with lopdf itself
    /// from the plain fixture, then run it through `DocEntry::load` to prove
    /// the whole open path yields a plaintext, editable buffer.
    #[test]
    fn rc4_encrypted_file_opens_with_plaintext_buffer() {
        let _guard = crate::test_pdfium_guard();
        let mut doc =
            lopdf::Document::load(crate::fixture_path()).expect("load plain fixture");
        // RC4 key derivation hashes the file ID; the plain fixture has none.
        let id = lopdf::Object::String(vec![0x42; 16], lopdf::StringFormat::Hexadecimal);
        doc.trailer.set("ID", lopdf::Object::Array(vec![id.clone(), id]));
        let version = EncryptionVersion::V2 {
            document: &doc,
            owner_password: "rc4pw",
            user_password: "rc4pw",
            key_length: 128,
            permissions: Permissions::all(),
        };
        let enc_state = EncryptionState::try_from(version).expect("rc4 state");
        doc.encrypt(&enc_state).expect("rc4 encrypt");
        let path = tmp_path("tumbler_rc4_fixture.pdf");
        doc.save(&path).expect("save rc4 file");

        let pdfium = crate::test_pdfium();
        let entry = DocEntry::load(pdfium, &path, Some("rc4pw")).expect("open rc4 file");
        assert!(entry.encrypted);
        assert_eq!(entry.password.as_deref(), Some("rc4pw"));
        let parsed = lopdf::Document::load_mem(&entry.buffer).expect("parse buffer");
        assert!(!parsed.is_encrypted(), "buffer must be plaintext");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn remove_password_clears_protection_and_marks_dirty() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = crate::encrypted_fixture_path().to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &path, Some(crate::ENCRYPTED_FIXTURE_PASSWORD))
            .expect("open encrypted");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        remove_password_impl(&state, "doc1").expect("remove password");

        let entry_arc = state.get_document("doc1").expect("get");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert!(!entry.encrypted);
        assert!(entry.password.is_none());
        assert!(entry.dirty, "save output changed, doc must be dirty");
    }

    #[test]
    fn set_password_protects_document_and_marks_dirty() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = crate::fixture_path().to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &path, None).expect("open");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        set_password_impl(&state, "doc1", "new-secret").expect("set password");

        let entry_arc = state.get_document("doc1").expect("get");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert!(entry.encrypted);
        assert_eq!(entry.password.as_deref(), Some("new-secret"));
        assert_eq!(entry.permissions, Some(Permissions::all()));
        assert!(entry.dirty, "save output changed, doc must be dirty");
        // The buffer stays plaintext — encryption happens only at Save.
        let parsed = lopdf::Document::load_mem(&entry.buffer).expect("parse buffer");
        assert!(!parsed.is_encrypted());
    }

    /// Calling set_password on an already-protected document is a password
    /// change: it replaces the stored password and keeps the file's original
    /// permission bits.
    #[test]
    fn set_password_on_encrypted_document_changes_password() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = crate::encrypted_fixture_path().to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &path, Some(crate::ENCRYPTED_FIXTURE_PASSWORD))
            .expect("open encrypted");
        let original_permissions = entry.permissions;
        state.insert_document("doc1".to_string(), entry).expect("insert");

        set_password_impl(&state, "doc1", "different-pw").expect("change password");

        let entry_arc = state.get_document("doc1").expect("get");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert!(entry.encrypted);
        assert_eq!(entry.password.as_deref(), Some("different-pw"));
        assert_eq!(entry.permissions, original_permissions);
        assert!(entry.dirty);
    }

    #[test]
    fn set_password_rejects_empty_password() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = crate::fixture_path().to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &path, None).expect("open");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let err = set_password_impl(&state, "doc1", "").expect_err("must reject");
        assert!(err.to_string().contains("cannot be empty"));

        let entry_arc = state.get_document("doc1").expect("get");
        let entry = lock_mutex(&entry_arc).expect("lock");
        assert!(!entry.encrypted, "a rejected call must not change the entry");
        assert!(!entry.dirty);
    }

    /// End-to-end shape of issue #58: plain file → set_password → Save As
    /// writes a file pdfium rejects without the password and opens with it.
    #[test]
    fn set_password_then_save_writes_encrypted_file() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = crate::fixture_path().to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &path, None).expect("open");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        set_password_impl(&state, "doc1", "issue58-pw").expect("set password");
        let dest = tmp_path("tumbler_set_password_save.pdf");
        crate::commands::save::save_document_as_impl(&state, "doc1", &dest)
            .expect("save as");

        assert!(
            pdfium.load_pdf_from_file(&dest, None).is_err(),
            "saved file must require the password"
        );
        let doc = pdfium
            .load_pdf_from_file(&dest, Some("issue58-pw"))
            .expect("open with the new password");
        assert_eq!(doc.pages().len(), 1);

        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn remove_password_on_unencrypted_document_is_error() {
        let _guard = crate::test_pdfium_guard();
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium, None);
        let path = crate::fixture_path().to_string_lossy().into_owned();
        let entry = DocEntry::load(pdfium, &path, None).expect("open");
        state.insert_document("doc1".to_string(), entry).expect("insert");

        let err = remove_password_impl(&state, "doc1").expect_err("must reject");
        assert!(err.to_string().contains("not password-protected"));
    }
}

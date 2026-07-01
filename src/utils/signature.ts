// Shared types + helpers for digital-signature verification display (issue #17).
// Mirrors the Rust `SignatureInfo` / `SignatureStatus` (serde camelCase).

export type SignatureStatus = "unsigned" | "verified" | "modifiedAfter" | "invalid";

export interface SignatureEntry {
  signerName: string;
  reason: string;
  location: string;
  signingTime: string;
  integrityOk: boolean;
  modifiedAfter: boolean;
}

export interface SignatureInfo {
  count: number;
  signatures: SignatureEntry[];
  status: SignatureStatus;
}

/** True once a document carries at least one signature (any status but unsigned). */
export function isSigned(status: SignatureStatus | undefined): boolean {
  return status !== undefined && status !== "unsigned";
}

export interface SignatureBadge {
  text: string;
  kind: "ok" | "warn";
}

/**
 * Status-bar badge for the active tab. Honest wording: "Verified" means the
 * signature is cryptographically intact, not that the signer is trusted.
 * Returns null when there's nothing to show (unsigned / unknown).
 */
export function signatureBadge(status: SignatureStatus | undefined): SignatureBadge | null {
  switch (status) {
    case "verified":
      return { text: "Verified Signed Document", kind: "ok" };
    case "modifiedAfter":
      return { text: "Signed — modified after signing", kind: "warn" };
    case "invalid":
      return { text: "Signed — signature could not be verified", kind: "warn" };
    default:
      return null;
  }
}

/** Overridable warning shown before an edit that would invalidate a signature. */
export const SIGNATURE_EDIT_WARNING =
  "This document is signed. Saving these changes will modify the file and " +
  "invalidate the digital signature — it can't be re-verified afterward.";

/**
 * Warning shown before saving a searchable copy of a signed document.
 * The source is untouched, but the copy will contain invalid signature data
 * because the added text layer changes the document content.
 */
export const SIGNATURE_SEARCHABLE_COPY_WARNING =
  "This document is signed. The searchable copy will include the signature " +
  "data, but because a text layer will be added, the signature will not " +
  "verify in the copy — it cannot be treated as a signed document.";

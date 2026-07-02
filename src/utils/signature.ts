// Shared types + helpers for digital-signature verification display (issue #17).
// Mirrors the Rust `SignatureInfo` / `SignatureStatus` (serde camelCase).

export type SignatureStatus =
  | "unsigned"
  | "verified"
  | "modifiedAfter"
  | "invalid"
  | "unknown";

export type Integrity = "ok" | "failed" | "unknown";

export interface SignatureEntry {
  signerName: string;
  reason: string;
  location: string;
  signingTime: string;
  integrity: Integrity;
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
  kind: "ok" | "warn" | "info";
}

/**
 * Status-bar badge for the active tab. Honest wording: "Verified" means the
 * signature is cryptographically intact, not that the signer is trusted;
 * "not verified here" means we detected a signature but can't check it in this
 * build (e.g. an Adobe BER-encoded CMS) — NOT that it's invalid. Returns null
 * when there's nothing to show (unsigned).
 */
export function signatureBadge(status: SignatureStatus | undefined): SignatureBadge | null {
  switch (status) {
    case "verified":
      return { text: "Verified Signed Document", kind: "ok" };
    case "modifiedAfter":
      return { text: "Signed — modified after signing", kind: "warn" };
    case "invalid":
      return { text: "Signed — signature is invalid", kind: "warn" };
    case "unknown":
      return { text: "Signed — not verified here", kind: "info" };
    default:
      return null;
  }
}

/** Overridable warning shown before an edit that would invalidate a signature. */
export const SIGNATURE_EDIT_WARNING =
  "This document is signed. Saving these changes will modify the file and " +
  "invalidate the digital signature — it can't be re-verified afterward.";

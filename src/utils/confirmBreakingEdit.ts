import { confirm } from "@tauri-apps/plugin-dialog";

/**
 * Ask the user to confirm an edit that will break some property of the file
 * (declared ISO conformance, a digital signature, etc.). The warning is always
 * overridable — this returns the user's choice, it does not block the edit.
 *
 * `reason` should be a complete sentence describing what will break, e.g.
 * "Optimizing it removes metadata and re-encodes images, which will break the
 * declared PDF/A-2b conformance." A standard "Continue anyway?" prompt and OK/
 * Cancel labels are appended here so callers stay consistent.
 *
 * Returns `true` if the user chose to proceed, `false` otherwise.
 *
 * Shared by the compression guard (issue #16) and intended for the
 * signature-invalidation guard (issue #17).
 */
export async function confirmBreakingEdit(reason: string): Promise<boolean> {
  return confirm(`${reason}\n\nContinue anyway?`, {
    title: "Continue?",
    kind: "warning",
    okLabel: "Continue",
    cancelLabel: "Cancel",
  });
}

import { invoke } from "@tauri-apps/api/core";
import { save, message } from "@tauri-apps/plugin-dialog";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";
import { evictDoc } from "./renderCache";

/**
 * Save / discard for a staged redacted copy (issue #1). Shared by the Redact
 * panel and the preview banner. Unlike utils/saveDocument.ts these never touch
 * the document buffer or retarget the tab — the redacted bytes are a separate
 * staged artifact, written only under a new name.
 */

/** Cache key namespace for preview renders of the staged redacted copy. */
export function redactPreviewCacheId(docId: string): string {
  return `${docId}::redacted`;
}

export function suggestRedactedName(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  const base = dot > 0 ? fileName.slice(0, dot) : fileName;
  return `${base}-redacted.pdf`;
}

/**
 * Prompts for a destination and writes the staged redacted copy there.
 * The backend refuses unverified output and the original file's path.
 * On success the preview ends and pending regions are cleared (the backend
 * cleared its staging). Returns true when the copy was saved.
 */
export async function saveRedactedCopyAs(tab: TabState): Promise<boolean> {
  const destPath = await save({
    filters: [{ name: "PDF", extensions: ["pdf"] }],
    defaultPath: suggestRedactedName(tab.fileName),
  });
  if (!destPath) return false;

  try {
    const savedPath = await invoke<string>("save_redacted_copy", {
      docId: tab.docId,
      destPath,
    });
    const store = usePdfStore.getState();
    store.updateTab(tab.id, { redactPreview: null });
    store.clearRedactRegions(tab.docId);
    evictDoc(redactPreviewCacheId(tab.docId));
    store.showNotice(`Redacted copy saved to ${savedPath}`);
    return true;
  } catch (err) {
    await message(String(err), { title: "Save Redacted Copy Failed", kind: "error" });
    return false;
  }
}

/** Drops the staged redacted copy and exits the preview. */
export async function discardRedaction(tab: TabState): Promise<void> {
  try {
    await invoke("discard_redaction", { docId: tab.docId });
  } catch {
    // Best-effort: staging may already be gone (e.g. a buffer edit cleared it).
  }
  const store = usePdfStore.getState();
  store.updateTab(tab.id, { redactPreview: null });
  evictDoc(redactPreviewCacheId(tab.docId));
}

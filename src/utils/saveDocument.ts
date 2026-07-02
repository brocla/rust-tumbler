import { invoke } from "@tauri-apps/api/core";
import { save, message } from "@tauri-apps/plugin-dialog";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

/**
 * Save / Save As for the non-destructive editing model (issue #31). These are
 * the only frontend paths that commit a document's in-memory buffer to disk.
 * Both return true when the document was actually saved — the close guards
 * use that to decide whether closing may proceed. The store's isDirty flag is
 * cleared via the backend's "document-dirty-changed" event, not here.
 */

/** Overwrites the tab's file with the in-memory buffer. */
export async function saveTab(tab: TabState): Promise<boolean> {
  try {
    await invoke("save_document", { docId: tab.docId });
    return true;
  } catch (err) {
    await message(String(err), { title: "Save Failed", kind: "error" });
    return false;
  }
}

/**
 * Prompts for a destination and writes the buffer there; the tab is retargeted
 * to the new path. Returns false if the user cancelled the dialog.
 */
export async function saveTabAs(tab: TabState): Promise<boolean> {
  const destPath = await save({
    filters: [{ name: "PDF", extensions: ["pdf"] }],
    defaultPath: tab.filePath,
  });
  if (!destPath) return false;

  try {
    const canonical = await invoke<string>("save_document_as", {
      docId: tab.docId,
      destPath,
    });
    usePdfStore.getState().updateTab(tab.id, {
      filePath: canonical,
      fileName: canonical.split(/[\\/]/).pop() ?? tab.fileName,
    });
    return true;
  } catch (err) {
    await message(String(err), { title: "Save As Failed", kind: "error" });
    return false;
  }
}

/**
 * The shared close-guard flow: prompts Save / Don't Save / Cancel for a dirty
 * tab and returns whether closing may proceed. Used by the tab × button and
 * the window-close handler.
 */
export async function confirmCloseDirtyTab(tab: TabState): Promise<boolean> {
  const choice = await usePdfStore.getState().askUnsaved(tab.fileName);
  if (choice === "cancel") return false;
  if (choice === "save") return saveTab(tab);
  return true; // discard
}

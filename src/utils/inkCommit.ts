import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";

/**
 * Flattens the open Ink Signature group into the document buffer, if there is
 * one (issue #120).
 *
 * Lives outside React because the group has to be able to close from places
 * that are not the panel — a Save, or any other command that rewrites the
 * buffer, has to commit pending ink *first* or it would write a file without
 * the signature the user just drew.
 *
 * `inkTake` clears the group as it hands it over, so two close triggers firing
 * together (say, Done and a page change) cannot commit the same strokes twice.
 * An empty group returns null and costs nothing.
 */
export async function commitOpenInk(): Promise<void> {
  const group = usePdfStore.getState().inkTake();
  if (!group) return;
  // The document may have closed under the group — a tab close, or a reopen
  // that minted a fresh doc_id. There is nothing to flatten the ink into, and
  // asking the backend would only raise "Document not found".
  const open = usePdfStore.getState().tabs.some((t) => t.docId === group.docId);
  if (!open) return;
  try {
    await invoke("apply_ink", {
      docId: group.docId,
      page: group.page,
      strokes: group.strokes,
    });
  } catch (err) {
    // The group was cleared on the way out, so a failed commit would otherwise
    // destroy the signature silently — put it back and let the caller report.
    usePdfStore.setState({ ink: { ...group, redo: [] } });
    throw err;
  }
}

/**
 * True when `docId` has ink drawn but not yet flattened into the buffer.
 *
 * Pending ink is unsaved work that the backend does not know about yet — the
 * buffer is untouched until the group closes, so `isDirty` is still false.
 * Anything asking "are there unsaved changes?" has to consider this too, or
 * Save stays greyed out over a drawn signature and closing the tab discards it
 * without asking.
 */
export function hasPendingInk(docId: string): boolean {
  const ink = usePdfStore.getState().ink;
  return !!ink && ink.docId === docId && ink.strokes.length > 0;
}

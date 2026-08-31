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
  await invoke("apply_ink", {
    docId: group.docId,
    page: group.page,
    strokes: group.strokes,
  });
}

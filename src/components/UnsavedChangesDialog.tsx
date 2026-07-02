import { usePdfStore } from "../store/usePdfStore";

/**
 * The three-way Save / Don't Save / Cancel prompt shown when a dirty document
 * is about to be discarded (tab close or window close). In-app because native
 * Tauri dialogs support at most two buttons. Driven by the store's
 * unsavedPrompt slice; the close guards await the promise it resolves.
 */
export function UnsavedChangesDialog() {
  const prompt = usePdfStore((s) => s.unsavedPrompt);
  const resolveUnsaved = usePdfStore((s) => s.resolveUnsaved);

  if (!prompt) return null;

  return (
    <div className="print-progress-overlay">
      <div className="print-progress-dialog unsaved-dialog">
        <p>Save changes to "{prompt.fileName}"?</p>
        <div className="unsaved-dialog-buttons">
          <button autoFocus onClick={() => resolveUnsaved("save")}>
            Save
          </button>
          <button onClick={() => resolveUnsaved("discard")}>Don't Save</button>
          <button onClick={() => resolveUnsaved("cancel")}>Cancel</button>
        </div>
      </div>
    </div>
  );
}

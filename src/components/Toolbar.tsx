import { BookOpen, ChevronLeft, ChevronRight, Eraser, FileSearch, Lock, LockOpen, Moon, MoveHorizontal, MoveVertical, Printer, Save, SaveAll, ScanSearch, ScrollText, Sun, ZoomIn, ZoomOut } from "lucide-react";
import { useEffect, useState } from "react";
import { save, message, ask } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";
import type { DisplayMode } from "../store/usePdfStore";
import { ZOOM_PRESETS } from "../utils/zoomConstants";
import { evictPages } from "../utils/renderCache";
import { confirmBreakingEdit } from "../utils/confirmBreakingEdit";
import { saveTab, saveTabAs } from "../utils/saveDocument";
import { isSigned, SIGNATURE_EDIT_WARNING } from "../utils/signature";
import type { SignatureInfo } from "../utils/signature";
import { SetPasswordDialog } from "./SetPasswordDialog";

interface TextExportResult {
  pages: number;
  ocrPages: number;
  cancelled: boolean;
}

interface AddTextLayerResult {
  pagesWritten: number;
  pagesSkippedUnsupportedGeometry: number;
  cancelled: boolean;
}

interface ToolbarProps {
  onOpenFile: () => void;
  onPrint: () => void;
}

const DISPLAY_MODE_ORDER: DisplayMode[] = ["normal", "invert", "sepia"];

const DISPLAY_MODE_INFO: Record<DisplayMode, { label: string; icon: typeof Sun }> = {
  normal: { label: "Normal", icon: Sun },
  invert: { label: "Inverted", icon: Moon },
  sepia: { label: "Sepia", icon: BookOpen },
};

export function Toolbar({ onOpenFile, onPrint }: ToolbarProps) {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const updateTab = usePdfStore((s) => s.updateTab);
  const setOcrProgress = usePdfStore((s) => s.setOcrProgress);
  const bumpFormEpoch = usePdfStore((s) => s.bumpFormEpoch);

  // A password-protected PDF is fully editable (its buffer is decrypted at
  // open, and Save re-encrypts with the same password — issue #57). The flag
  // only drives the "Remove password" button.
  const encrypted = !!activeTab?.encrypted;

  // Whether the active document has any AcroForm fields — gates the Clear-form
  // button. Re-checked when the active document changes.
  const [hasForm, setHasForm] = useState(false);
  const activeDocId = activeTab?.docId;
  useEffect(() => {
    if (!activeDocId) {
      setHasForm(false);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const v = await invoke<boolean>("document_has_form", { docId: activeDocId });
        if (!cancelled) setHasForm(!!v);
      } catch {
        if (!cancelled) setHasForm(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeDocId]);

  const handleClearForm = async () => {
    if (!activeTab) return;
    try {
      await invoke("clear_form", { docId: activeTab.docId });
      bumpFormEpoch(activeTab.docId);
      // Repaint the pages so pdfium's render drops appearances shown on the
      // canvas (comb values, drawn signatures) rather than via HTML overlays.
      evictPages(activeTab.docId);
      updateTab(activeTab.id, { contentEpoch: activeTab.contentEpoch + 1 });
    } catch (err) {
      await message(`Failed to clear form: ${err}`, {
        title: "Clear form",
        kind: "error",
      });
    }
  };

  const handlePrevPage = () => {
    if (!activeTab || activeTab.currentPage <= 1) return;
    updateTab(activeTab.id, { currentPage: activeTab.currentPage - 1 });
  };

  const handleNextPage = () => {
    if (!activeTab || activeTab.currentPage >= activeTab.pageCount) return;
    updateTab(activeTab.id, { currentPage: activeTab.currentPage + 1 });
  };

  const handlePageInput = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== "Enter" || !activeTab) return;
    const val = parseInt((e.target as HTMLInputElement).value, 10);
    if (val >= 1 && val <= activeTab.pageCount) {
      updateTab(activeTab.id, { currentPage: val });
    }
  };

  const handleZoomIn = () => {
    if (!activeTab) return;
    const next = ZOOM_PRESETS.find((z) => z > activeTab.zoom);
    if (next) updateTab(activeTab.id, { zoom: next, zoomMode: "numeric" });
  };

  const handleZoomOut = () => {
    if (!activeTab) return;
    const prev = [...ZOOM_PRESETS].reverse().find((z) => z < activeTab.zoom);
    if (prev) updateTab(activeTab.id, { zoom: prev, zoomMode: "numeric" });
  };

  const handleZoomSelect = (e: React.ChangeEvent<HTMLSelectElement>) => {
    if (!activeTab) return;
    const val = parseInt(e.target.value, 10);
    if (!isNaN(val)) {
      updateTab(activeTab.id, { zoom: val, zoomMode: "numeric" });
    }
  };

  const handleFitWidth = () => {
    if (!activeTab) return;
    updateTab(activeTab.id, { zoomMode: activeTab.zoomMode === "fit-width" ? "numeric" : "fit-width" });
  };

  const handleFitPage = () => {
    if (!activeTab) return;
    updateTab(activeTab.id, { zoomMode: activeTab.zoomMode === "fit-page" ? "numeric" : "fit-page" });
  };

  // Wheel-zoom moves in fixed increments and isn't snapped to ZOOM_PRESETS,
  // so the controlled <select> below needs a matching <option> for whatever
  // arbitrary value activeTab.zoom currently holds — otherwise its displayed
  // value goes blank/stale.
  const zoomOptions =
    activeTab && !ZOOM_PRESETS.includes(activeTab.zoom)
      ? [...ZOOM_PRESETS, activeTab.zoom].sort((a, b) => a - b)
      : ZOOM_PRESETS;

  const handleCycleDisplayMode = () => {
    if (!activeTab) return;
    const idx = DISPLAY_MODE_ORDER.indexOf(activeTab.displayMode);
    const next = DISPLAY_MODE_ORDER[(idx + 1) % DISPLAY_MODE_ORDER.length];
    updateTab(activeTab.id, { displayMode: next });
  };

  const handleExportText = async () => {
    if (!activeTab) return;
    const dir = activeTab.filePath.replace(/[\\/][^\\/]*$/, "");
    const baseName = activeTab.fileName.replace(/\.pdf$/i, "");
    const destPath = await save({
      filters: [{ name: "Text", extensions: ["txt"] }],
      defaultPath: `${dir}/${baseName}.txt`,
    });
    if (!destPath) return;

    // Offer OCR only when there are pages with no text layer (likely scans).
    let useOcr = false;
    try {
      const missing = await invoke<number>("count_pages_without_text", {
        docId: activeTab.docId,
      });
      if (missing > 0) {
        useOcr = await ask(
          `${missing} page${missing === 1 ? " has" : "s have"} no text layer ` +
            `and may be scanned images.\n\nRun OCR on ${
              missing === 1 ? "it" : "them"
            } so the exported text includes their content? ` +
            `This takes roughly 1–3 seconds per page.`,
          { title: "Export Text", kind: "info" },
        );
      }
    } catch (err) {
      await message(String(err), { title: "Export Failed", kind: "error" });
      return;
    }

    // Show the progress overlay only when OCR (the slow path) will run.
    if (useOcr) {
      setOcrProgress({ page: 0, total: activeTab.pageCount });
    }
    try {
      const result = await invoke<TextExportResult>("export_text", {
        docId: activeTab.docId,
        destPath,
        useOcr,
      });
      if (result.cancelled) {
        await message("Export cancelled.", { title: "Export Text", kind: "info" });
      } else {
        const ocrNote =
          result.ocrPages > 0 ? ` (${result.ocrPages} via OCR)` : "";
        await message(`Exported ${result.pages} pages${ocrNote}.`, {
          title: "Export Complete",
          kind: "info",
        });
      }
    } catch (err) {
      await message(String(err), { title: "Export Failed", kind: "error" });
    } finally {
      setOcrProgress(null);
    }
  };

  // Document-level "Make Searchable": OCR every page that has no text layer so
  // search, selection/copy, and a later text export all work on scanned pages.
  //
  // When every page already reports text there's normally nothing to do — but
  // "has text" is not "has *useful* text". Scanners routinely emit an invisible
  // OCR layer that is wrong, misplaced, or both (issue #97), and refusing to
  // run would leave the user stuck with no way forward. So offer a forced run
  // instead of declining: the user can see the text is junk, and Tumbler can't
  // tell without guessing. Forcing is safe — OCR is session-only and nothing
  // reaches disk unless they separately Add Text Layer and Save.
  const handleMakeSearchable = async () => {
    if (!activeTab) return;

    let missing = 0;
    try {
      missing = await invoke<number>("count_pages_without_text", {
        docId: activeTab.docId,
      });
    } catch (err) {
      await message(String(err), { title: "Make Searchable", kind: "error" });
      return;
    }

    let force = false;
    if (missing === 0) {
      force = await ask(
        "Every page already has a text layer, so there's nothing to OCR.\n\n" +
          "If that text is wrong — scanned files often carry a bad OCR layer " +
          "you can't select — you can re-OCR every page anyway and use the " +
          "result instead. Nothing is written to disk.",
        {
          title: "Make Searchable",
          kind: "info",
          okLabel: "Re-OCR anyway",
          cancelLabel: "Cancel",
        },
      );
      if (!force) return;
    }

    setOcrProgress({ page: 0, total: activeTab.pageCount });
    try {
      const result = await invoke<{ pagesOcred: number; cancelled: boolean }>(
        "ocr_document",
        { docId: activeTab.docId, force },
      );
      // Refresh the text overlay so the newly-recognized pages are selectable.
      updateTab(activeTab.id, { ocrEpoch: activeTab.ocrEpoch + 1 });
      if (result.cancelled) {
        await message(
          `Cancelled after making ${result.pagesOcred} page${
            result.pagesOcred === 1 ? "" : "s"
          } searchable.`,
          { title: "Make Searchable", kind: "info" },
        );
      } else if (result.pagesOcred === 0) {
        // Only reachable on a forced run: OCR read the pages and found no
        // words. Say so plainly rather than claim success.
        await message(
          "Re-OCR finished, but no text was recognized on any page. The " +
            "document's existing text layer is unchanged.",
          { title: "Make Searchable", kind: "info" },
        );
      } else {
        const pages = `${result.pagesOcred} page${result.pagesOcred === 1 ? "" : "s"}`;
        await message(
          force
            ? `Re-OCR'd ${pages}. Search, selection, and copy now use the ` +
                "recognized text instead of the layer already in the file."
            : `Made ${pages} searchable. You can now search, select, and copy their text.`,
          { title: "Make Searchable", kind: "info" },
        );
      }
    } catch (err) {
      await message(String(err), { title: "Make Searchable", kind: "error" });
    } finally {
      setOcrProgress(null);
    }
  };

  // "Remove password": drop the document's password protection so the next
  // Save writes an ordinary, unprotected PDF (issue #57). In-memory like every
  // other edit — closing without saving leaves the file untouched — so no
  // confirmation dialog; the button tooltip states the consequence.
  const handleRemovePassword = async () => {
    if (!activeTab) return;

    // Saving after this rewrites the bytes, so an embedded digital signature
    // won't verify anymore. Warn first (overridable; nothing is saved yet).
    if (isSigned(activeTab.signatureStatus)) {
      const proceed = await confirmBreakingEdit(SIGNATURE_EDIT_WARNING);
      if (!proceed) return;
    }

    try {
      await invoke("remove_password", { docId: activeTab.docId });
      updateTab(activeTab.id, { encrypted: false });
      await message(
        "Password protection removed. Save or Save As will write an " +
          "unprotected PDF that opens without a password.",
        { title: "Remove Password", kind: "info" },
      );
    } catch (err) {
      await message(String(err), { title: "Remove Password", kind: "error" });
    }
  };

  // "Set password" / "Change password" (issue #58): store a password on the
  // document so the next Save writes an AES-256-encrypted file. Like
  // remove-password this is an in-memory change — no confirmation dialog, the
  // tooltip and the post-action notice state the consequence.
  const [setPasswordOpen, setSetPasswordOpen] = useState(false);

  const handleSetPasswordClick = async () => {
    if (!activeTab) return;

    // Saving after this rewrites the bytes, so an embedded digital signature
    // won't verify anymore. Warn first (overridable; nothing is saved yet).
    if (isSigned(activeTab.signatureStatus)) {
      const proceed = await confirmBreakingEdit(SIGNATURE_EDIT_WARNING);
      if (!proceed) return;
    }
    setSetPasswordOpen(true);
  };

  const handleSetPasswordSubmit = async (password: string) => {
    if (!activeTab) return;
    setSetPasswordOpen(false);
    const changing = encrypted;
    try {
      await invoke("set_password", { docId: activeTab.docId, password });
      updateTab(activeTab.id, { encrypted: true });
      await message(
        changing
          ? "Password changed. Save or Save As will write the file " +
              "encrypted with the new password."
          : "Password set. Save or Save As will write an encrypted PDF " +
              "that requires this password to open.",
        { title: changing ? "Change Password" : "Set Password", kind: "info" },
      );
    } catch (err) {
      await message(String(err), {
        title: changing ? "Change Password" : "Set Password",
        kind: "error",
      });
    }
  };

  // "Add Text Layer": embed an invisible OCR text layer into the document's
  // scanned pages, searchable in any reader. Like every edit (issue #31) it
  // lands in the in-memory buffer — the user commits it with Save / Save As.
  const handleAddTextLayer = async () => {
    if (!activeTab) return;

    // The embedded text layer means the signature won't verify once saved.
    // Warn first; the warning is overridable and nothing is saved yet.
    if (isSigned(activeTab.signatureStatus)) {
      const proceed = await confirmBreakingEdit(SIGNATURE_EDIT_WARNING);
      if (!proceed) return;
    }

    setOcrProgress({ page: 0, total: activeTab.pageCount });
    try {
      const result = await invoke<AddTextLayerResult>("add_text_layer", {
        docId: activeTab.docId,
      });
      // The buffer changed (native text now exists), so refresh the text
      // overlay for selection/search.
      updateTab(activeTab.id, { ocrEpoch: activeTab.ocrEpoch + 1 });
      if (result.pagesWritten > 0) {
        // The edit diverged the buffer from the signed bytes — re-verify so
        // the badge honestly shows "modified" while unsaved, matching what
        // the page-edit path does. Best-effort, like every other refresh.
        try {
          const sig = await invoke<SignatureInfo>("get_signature_info", {
            docId: activeTab.docId,
          });
          updateTab(activeTab.id, { signatureStatus: sig.status });
        } catch {
          /* best-effort */
        }
      }
      const plural = (n: number) => (n === 1 ? "" : "s");
      const written = result.pagesWritten;
      const skipped = result.pagesSkippedUnsupportedGeometry;
      // A "rotated or offset" clause describing the skipped pages, or "" when
      // there were none. These pages were OCR'd but their geometry isn't yet
      // supported, so they got no searchable layer — say so rather than hide it.
      const skippedNote =
        skipped > 0
          ? `${skipped} rotated or offset page${plural(skipped)} couldn't be made searchable`
          : "";

      let text: string;
      if (result.cancelled) {
        text = "Cancelled — no text layer was added.";
      } else if (written > 0 && skipped > 0) {
        text = `Added a text layer to ${written} page${plural(written)}; ${skippedNote}. Use Save or Save As to keep it.`;
      } else if (written > 0) {
        text = `Added a text layer to ${written} page${plural(written)}. Use Save or Save As to keep it.`;
      } else if (skipped > 0) {
        text = `No text layer added — ${skippedNote} (unsupported page geometry).`;
      } else {
        text = "Every page already has a text layer — nothing to add.";
      }
      await message(text, { title: "Add Text Layer", kind: "info" });
    } catch (err) {
      await message(String(err), { title: "Add Text Layer", kind: "error" });
    } finally {
      setOcrProgress(null);
    }
  };

  return (
    <div className="toolbar">
      <div className="toolbar-group">
        <button
          className="toolbar-button toolbar-button-text"
          onClick={onOpenFile}
          title="Open PDF (Ctrl+O)"
        >
          <strong>Open PDF</strong>
        </button>
      </div>

      {activeTab && (
        <>
          <div className="toolbar-group">
            <button
              className="toolbar-button"
              onClick={() => void saveTab(activeTab)}
              disabled={!activeTab.isDirty}
              title="Save (Ctrl+S)"
            >
              <Save size={18} />
            </button>
            <button
              className="toolbar-button"
              onClick={() => void saveTabAs(activeTab)}
              title="Save As... (Ctrl+Shift+S)"
            >
              <SaveAll size={18} />
            </button>
            <button
              className="toolbar-button"
              onClick={handleSetPasswordClick}
              title={
                encrypted
                  ? "Change the password (the next Save uses the new password)"
                  : "Set a password (the next Save writes an encrypted file)"
              }
            >
              <Lock size={18} />
            </button>
            {encrypted && (
              <button
                className="toolbar-button"
                onClick={handleRemovePassword}
                title="Remove password protection (the next Save writes an unprotected file)"
              >
                <LockOpen size={18} />
              </button>
            )}
            {hasForm && (
              <button
                className="toolbar-button"
                onClick={handleClearForm}
                title="Clear form fields"
              >
                <Eraser size={18} />
              </button>
            )}
          </div>

          <div className="toolbar-spacer" />
          <div className="toolbar-group">
            <button
              className="toolbar-button"
              onClick={handlePrevPage}
              disabled={activeTab.currentPage <= 1}
              title="Previous page"
            >
              <ChevronLeft size={18} />
            </button>
            <input
              className="page-input"
              type="text"
              defaultValue={activeTab.currentPage}
              key={`${activeTab.id}-${activeTab.currentPage}`}
              onKeyDown={handlePageInput}
              title="Go to page"
            />
            <span className="page-label">/ {activeTab.pageCount}</span>
            <button
              className="toolbar-button"
              onClick={handleNextPage}
              disabled={activeTab.currentPage >= activeTab.pageCount}
              title="Next page"
            >
              <ChevronRight size={18} />
            </button>
          </div>

          <div className="toolbar-separator" />
          <div className="toolbar-group">
            <button
              className="toolbar-button"
              onClick={handleZoomOut}
              disabled={activeTab.zoom <= ZOOM_PRESETS[0]}
              title="Zoom out"
            >
              <ZoomOut size={18} />
            </button>
            <select
              className="zoom-select"
              value={activeTab.zoom}
              onChange={handleZoomSelect}
            >
              {zoomOptions.map((z) => (
                <option key={z} value={z}>
                  {z}%
                </option>
              ))}
            </select>
            <button
              className="toolbar-button"
              onClick={handleZoomIn}
              disabled={activeTab.zoom >= ZOOM_PRESETS[ZOOM_PRESETS.length - 1]}
              title="Zoom in"
            >
              <ZoomIn size={18} />
            </button>
            <button
              className={`toolbar-button${activeTab.zoomMode === "fit-width" ? " active" : ""}`}
              onClick={handleFitWidth}
              title="Fit to width"
            >
              <MoveHorizontal size={18} />
            </button>
            <button
              className={`toolbar-button${activeTab.zoomMode === "fit-page" ? " active" : ""}`}
              onClick={handleFitPage}
              title="Fit to height"
            >
              <MoveVertical size={18} />
            </button>
          </div>

          <div className="toolbar-separator" />
          {(() => {
            const { label, icon: DisplayModeIcon } = DISPLAY_MODE_INFO[activeTab.displayMode];
            return (
              <button
                className="toolbar-button"
                onClick={handleCycleDisplayMode}
                title={`Display mode: ${label} (click to cycle)`}
              >
                <DisplayModeIcon size={18} />
              </button>
            );
          })()}

          <div className="toolbar-separator" />
          <button
            className="toolbar-button"
            onClick={handleMakeSearchable}
            title="OCR - Make Text Searchable"
          >
            <ScanSearch size={18} />
          </button>
          <button
            className="toolbar-button"
            onClick={handleAddTextLayer}
            title="Add Text Layer (make searchable in any reader)"
          >
            <FileSearch size={18} />
          </button>
          <button
            className="toolbar-button"
            onClick={handleExportText}
            title="Export Text..."
          >
            <ScrollText size={18} />
          </button>
          <button
            className="toolbar-button"
            onClick={onPrint}
            title="Print (Ctrl+P)"
          >
            <Printer size={18} />
          </button>

          {setPasswordOpen && (
            <SetPasswordDialog
              fileName={activeTab.fileName}
              changing={encrypted}
              onSubmit={handleSetPasswordSubmit}
              onCancel={() => setSetPasswordOpen(false)}
            />
          )}
        </>
      )}
    </div>
  );
}

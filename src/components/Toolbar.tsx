import { BookOpen, ChevronLeft, ChevronRight, Moon, Printer, ScanSearch, ScrollText, Sun, ZoomIn, ZoomOut } from "lucide-react";
import { save, message, ask } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";
import type { DisplayMode } from "../store/usePdfStore";
import { ZOOM_PRESETS } from "../utils/zoomConstants";

interface TextExportResult {
  pages: number;
  ocrPages: number;
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
    if (missing === 0) {
      await message("Every page already has a text layer — nothing to OCR.", {
        title: "Make Searchable",
        kind: "info",
      });
      return;
    }

    setOcrProgress({ page: 0, total: activeTab.pageCount });
    try {
      const result = await invoke<{ pagesOcred: number; cancelled: boolean }>(
        "ocr_document",
        { docId: activeTab.docId },
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
      } else {
        await message(
          `Made ${result.pagesOcred} page${
            result.pagesOcred === 1 ? "" : "s"
          } searchable. You can now search, select, and copy their text.`,
          { title: "Make Searchable", kind: "info" },
        );
      }
    } catch (err) {
      await message(String(err), { title: "Make Searchable", kind: "error" });
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
        </>
      )}
    </div>
  );
}

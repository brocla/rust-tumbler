import { BookOpen, ChevronLeft, ChevronRight, Moon, Printer, ScrollText, Sun, ZoomIn, ZoomOut } from "lucide-react";
import { save, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";
import type { DisplayMode } from "../store/usePdfStore";
import { ZOOM_PRESETS } from "../utils/zoomConstants";

interface TextExportResult {
  pages: number;
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
    try {
      const result = await invoke<TextExportResult>("export_text", {
        docId: activeTab.docId,
        destPath,
      });
      await message(`Exported ${result.pages} pages.`, {
        title: "Export Complete",
        kind: "info",
      });
    } catch (err) {
      await message(String(err), { title: "Export Failed", kind: "error" });
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

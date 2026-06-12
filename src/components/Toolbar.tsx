import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { FileText, ChevronLeft, ChevronRight, ZoomIn, ZoomOut } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import type { PageDimension } from "../store/usePdfStore";
import { ZOOM_PRESETS } from "../utils/zoomConstants";

interface DocInfo {
  docId: string;
  pageCount: number;
  pageDimensions: PageDimension[];
}

export function Toolbar() {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const addTab = usePdfStore((s) => s.addTab);
  const updateTab = usePdfStore((s) => s.updateTab);

  const handleOpen = async () => {
    const selected = await open({
      multiple: false,
      filters: [{ name: "PDF Documents", extensions: ["pdf"] }],
    });
    if (!selected) return;

    const path = typeof selected === "string" ? selected : selected;

    try {
      const info = await invoke<DocInfo>("open_document", { path });
      const tabId = crypto.randomUUID();
      addTab({
        id: tabId,
        docId: info.docId,
        fileName: path.split(/[\\/]/).pop() ?? "Untitled",
        pageCount: info.pageCount,
        pageDimensions: info.pageDimensions,
        currentPage: 1,
        scrollTop: 0,
        zoom: 100,
        zoomMode: "numeric",
        displayMode: "normal",
        searchQuery: "",
        searchResults: [],
        searchResultIndex: -1,
        metadataDirty: false,
        loading: false,
      });
    } catch (err) {
      console.error("Failed to open document:", err);
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

  return (
    <div className="toolbar">
      <div className="toolbar-group">
        <button
          className="toolbar-button"
          onClick={handleOpen}
          title="Open PDF (Ctrl+O)"
        >
          <FileText size={18} />
        </button>
      </div>

      {activeTab && (
        <>
          <div className="toolbar-separator" />
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
              {ZOOM_PRESETS.map((z) => (
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
        </>
      )}
    </div>
  );
}

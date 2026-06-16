import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { message } from "@tauri-apps/plugin-dialog";
import { ImageOff, RotateCw, RotateCcw, Trash2, Scissors, FileInput } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";

const THUMBNAIL_SCALE = 0.18;

interface PageInfo {
  pageCount: number;
  pageDimensions: { width: number; height: number }[];
}

interface SplitForm {
  firstPage: string;
  lastPage: string;
}

export function PagesPanel() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));

  const [editMode, setEditMode] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState(false);
  const [splitForm, setSplitForm] = useState<SplitForm | null>(null);
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);

  // Clear selection when page count changes (after an operation)
  const prevDocIdRef = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (activeTab?.docId !== prevDocIdRef.current) {
      setSelected(new Set());
      setEditMode(false);
      setSplitForm(null);
      prevDocIdRef.current = activeTab?.docId;
    }
  }, [activeTab?.docId]);

  useEffect(() => {
    setSelected(new Set());
    setSplitForm(null);
  }, [activeTab?.pagesVersion]);

  if (!activeTab) return null;

  const { docId, pageDimensions, pagesVersion } = activeTab;
  const pageCount = activeTab.pageCount;

  const toggleSelect = (page: number) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(page)) next.delete(page);
      else next.add(page);
      return next;
    });
  };

  const selectAll = () => {
    setSelected(new Set(Array.from({ length: pageCount }, (_, i) => i + 1)));
  };

  const clearSelection = () => setSelected(new Set());

  async function runOp(op: () => Promise<PageInfo | void>) {
    setBusy(true);
    try {
      await op();
    } catch (err) {
      await message(String(err), { title: "Page Operation Failed", kind: "error" });
    } finally {
      setBusy(false);
    }
  }

  const handleDelete = () =>
    runOp(async () => {
      if (selected.size === 0) return;
      await invoke<PageInfo>("delete_pages", {
        docId,
        pageNumbers: Array.from(selected),
      });
    });

  const handleRotateCw = () =>
    runOp(async () => {
      if (selected.size === 0) return;
      await invoke<PageInfo>("rotate_pages", {
        docId,
        pageNumbers: Array.from(selected),
        clockwiseTurns: 1,
      });
    });

  const handleRotateCcw = () =>
    runOp(async () => {
      if (selected.size === 0) return;
      await invoke<PageInfo>("rotate_pages", {
        docId,
        pageNumbers: Array.from(selected),
        clockwiseTurns: 3,
      });
    });

  const handleMerge = () =>
    runOp(async () => {
      const sourcePath = await open({
        multiple: false,
        filters: [{ name: "PDF Documents", extensions: ["pdf"] }],
      });
      if (!sourcePath) return;
      await invoke<PageInfo>("merge_document", {
        docId,
        sourcePath,
        insertAfterPage: pageCount,
      });
    });

  const handleSplit = async () => {
    if (!splitForm) {
      setSplitForm({ firstPage: "1", lastPage: String(pageCount) });
      return;
    }
    const first = parseInt(splitForm.firstPage, 10);
    const last = parseInt(splitForm.lastPage, 10);
    if (isNaN(first) || isNaN(last) || first < 1 || last < first || last > pageCount) {
      await message(`Invalid page range: ${first}–${last} (document has ${pageCount} pages)`, {
        title: "Invalid Range",
        kind: "error",
      });
      return;
    }
    const destPath = await save({
      filters: [{ name: "PDF Documents", extensions: ["pdf"] }],
    });
    if (!destPath) return;
    setBusy(true);
    try {
      await invoke("split_document", {
        docId,
        firstPage: first,
        lastPage: last,
        destPath,
      });
      setSplitForm(null);
    } catch (err) {
      await message(String(err), { title: "Split Failed", kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  // ── Drag-and-drop reorder ─────────────────────────────────────────────────

  const handleDragStart = (index: number) => setDragIndex(index);
  const handleDragOver = (e: React.DragEvent, index: number) => {
    e.preventDefault();
    setDropIndex(index);
  };
  const handleDragEnd = () => {
    if (dragIndex !== null && dropIndex !== null && dragIndex !== dropIndex) {
      const order = Array.from({ length: pageCount }, (_, i) => i + 1);
      const [moved] = order.splice(dragIndex, 1);
      order.splice(dropIndex, 0, moved);
      runOp(() =>
        invoke<PageInfo>("reorder_pages", { docId, newOrder: order }),
      );
    }
    setDragIndex(null);
    setDropIndex(null);
  };

  return (
    <div className="pages-panel">
      {/* Header */}
      <div className="pages-panel-header">
        <span className="pages-panel-title">
          {pageCount} page{pageCount !== 1 ? "s" : ""}
        </span>
        <button
          className={`pages-edit-toggle ${editMode ? "active" : ""}`}
          onClick={() => {
            setEditMode((m) => !m);
            setSelected(new Set());
            setSplitForm(null);
          }}
        >
          {editMode ? "Done" : "Edit"}
        </button>
      </div>

      {/* Action bar (edit mode only) */}
      {editMode && (
        <div className="pages-action-bar">
          <button
            className="pages-action-button"
            onClick={selected.size === pageCount ? clearSelection : selectAll}
          >
            {selected.size === pageCount ? "Deselect All" : "Select All"}
          </button>
          <span className="pages-action-sep" />
          <button
            className="pages-action-button pages-action-danger"
            title="Delete selected pages"
            disabled={selected.size === 0 || busy}
            onClick={handleDelete}
          >
            <Trash2 size={14} />
          </button>
          <button
            className="pages-action-button"
            title="Rotate selected pages clockwise"
            disabled={selected.size === 0 || busy}
            onClick={handleRotateCw}
          >
            <RotateCw size={14} />
          </button>
          <button
            className="pages-action-button"
            title="Rotate selected pages counter-clockwise"
            disabled={selected.size === 0 || busy}
            onClick={handleRotateCcw}
          >
            <RotateCcw size={14} />
          </button>
          <button
            className="pages-action-button"
            title="Merge another PDF into this document"
            disabled={busy}
            onClick={handleMerge}
          >
            <FileInput size={14} />
          </button>
          <button
            className={`pages-action-button ${splitForm ? "active" : ""}`}
            title="Split pages to a new PDF"
            disabled={busy}
            onClick={handleSplit}
          >
            <Scissors size={14} />
          </button>
        </div>
      )}

      {/* Split form */}
      {editMode && splitForm && (
        <div className="pages-split-form">
          <label className="pages-split-label">Pages</label>
          <input
            className="pages-split-input"
            type="number"
            min={1}
            max={pageCount}
            value={splitForm.firstPage}
            onChange={(e) => setSplitForm({ ...splitForm, firstPage: e.target.value })}
            aria-label="First page"
          />
          <span className="pages-split-to">–</span>
          <input
            className="pages-split-input"
            type="number"
            min={1}
            max={pageCount}
            value={splitForm.lastPage}
            onChange={(e) => setSplitForm({ ...splitForm, lastPage: e.target.value })}
            aria-label="Last page"
          />
          <button
            className="pages-split-save"
            onClick={handleSplit}
            disabled={busy}
          >
            Save…
          </button>
          <button
            className="pages-action-button"
            onClick={() => setSplitForm(null)}
          >
            ✕
          </button>
        </div>
      )}

      {/* Thumbnail grid */}
      <div className="pages-grid">
        {pageDimensions.map((dim, i) => {
          const pageNum = i + 1;
          const isDragging = dragIndex === i;
          const isDropTarget = dropIndex === i && dragIndex !== i;
          return (
            <PageThumb
              key={`${docId}-v${pagesVersion}-${pageNum}`}
              docId={docId}
              pageNumber={pageNum}
              pageWidth={dim.width}
              pageHeight={dim.height}
              isActive={activeTab.currentPage === pageNum}
              editMode={editMode}
              selected={selected.has(pageNum)}
              isDragging={isDragging}
              isDropTarget={isDropTarget}
              onSelect={() => toggleSelect(pageNum)}
              onClick={() => {
                if (!editMode) {
                  usePdfStore.getState().updateTab(activeTab.id, { currentPage: pageNum });
                }
              }}
              onDragStart={() => handleDragStart(i)}
              onDragOver={(e) => handleDragOver(e, i)}
              onDragEnd={handleDragEnd}
            />
          );
        })}
      </div>
    </div>
  );
}

// ── PageThumb ─────────────────────────────────────────────────────────────────

interface PageThumbProps {
  docId: string;
  pageNumber: number;
  pageWidth: number;
  pageHeight: number;
  isActive: boolean;
  editMode: boolean;
  selected: boolean;
  isDragging: boolean;
  isDropTarget: boolean;
  onSelect: () => void;
  onClick: () => void;
  onDragStart: () => void;
  onDragOver: (e: React.DragEvent) => void;
  onDragEnd: () => void;
}

function PageThumb({
  docId,
  pageNumber,
  pageWidth,
  pageHeight,
  isActive,
  editMode,
  selected,
  isDragging,
  isDropTarget,
  onSelect,
  onClick,
  onDragStart,
  onDragOver,
  onDragEnd,
}: PageThumbProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [rendered, setRendered] = useState(false);
  const [failed, setFailed] = useState(false);

  const cssWidth = Math.round(pageWidth * THUMBNAIL_SCALE);
  const cssHeight = Math.round(pageHeight * THUMBNAIL_SCALE);

  const renderThumb = useCallback(async () => {
    const canvas = canvasRef.current;
    if (!canvas || rendered || failed) return;

    const dpr = window.devicePixelRatio || 1;
    const renderWidth = Math.round(cssWidth * dpr);

    try {
      const buffer = await invoke<ArrayBuffer>("render_page", {
        docId,
        page: pageNumber,
        width: renderWidth,
      });

      const rgba = new Uint8ClampedArray(buffer);
      const actualHeight = rgba.byteLength / (4 * renderWidth);
      const imageData = new ImageData(rgba, renderWidth, actualHeight);

      canvas.width = renderWidth;
      canvas.height = actualHeight;
      canvas.style.width = `${cssWidth}px`;
      canvas.style.height = `${cssHeight}px`;

      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.putImageData(imageData, 0, 0);
        setRendered(true);
      }
    } catch {
      setFailed(true);
    }
  }, [docId, pageNumber, cssWidth, cssHeight, rendered, failed]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new IntersectionObserver(
      ([entry]) => { if (entry.isIntersecting) renderThumb(); },
      { threshold: 0.1 },
    );
    observer.observe(container);
    return () => observer.disconnect();
  }, [renderThumb]);

  const handleClick = () => {
    if (editMode) onSelect();
    else onClick();
  };

  return (
    <div
      ref={containerRef}
      className={[
        "pages-thumb",
        isActive && !editMode ? "active" : "",
        selected ? "selected" : "",
        isDragging ? "dragging" : "",
        isDropTarget ? "drop-target" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      draggable={editMode}
      onClick={handleClick}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
    >
      {editMode && (
        <input
          type="checkbox"
          className="pages-thumb-checkbox"
          checked={selected}
          onChange={onSelect}
          onClick={(e) => e.stopPropagation()}
        />
      )}
      <canvas ref={canvasRef} style={{ width: cssWidth, height: cssHeight }} />
      {failed && (
        <div
          className="thumbnail-error"
          style={{ width: cssWidth, height: cssHeight }}
        >
          <ImageOff size={16} />
        </div>
      )}
      <span className="thumbnail-label">{pageNumber}</span>
    </div>
  );
}

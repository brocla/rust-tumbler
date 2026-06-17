import { useEffect, useLayoutEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { message } from "@tauri-apps/plugin-dialog";
import { ImageOff, RotateCw, RotateCcw, Trash2, Scissors, FileInput, GripVertical } from "lucide-react";
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

  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState(false);
  const [splitForm, setSplitForm] = useState<SplitForm | null>(null);
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const [flipIndex, setFlipIndex] = useState<number | null>(null);
  const [flipStyle, setFlipStyle] = useState<React.CSSProperties | undefined>(undefined);
  const gridRef = useRef<HTMLDivElement>(null);
  const draggedHeightRef = useRef<number>(0);
  const pendingFlipRef = useRef<{ fromY: number; fromIdx: number; toIdx: number } | null>(null);
  const lastOpRef = useRef<"rotate" | "other">("other");

  // Scroll to the last-known sidebar page when this panel mounts
  useEffect(() => {
    const page = usePdfStore.getState().getActiveTab()?.sidebarScrollPage ?? 1;
    if (page <= 1) return;
    requestAnimationFrame(() => {
      gridRef.current
        ?.querySelector<HTMLElement>(`[data-page="${page}"]`)
        ?.scrollIntoView({ block: "start" });
    });
  }, []); // mount only

  // Track topmost visible thumbnail and persist to store
  useEffect(() => {
    const grid = gridRef.current;
    if (!grid || !activeTab) return;
    const visible = new Set<number>();

    const obs = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const p = Number(entry.target.getAttribute("data-page"));
          if (entry.isIntersecting) visible.add(p);
          else visible.delete(p);
        }
        if (visible.size > 0) {
          const min = [...visible].reduce((a, b) => (b < a ? b : a));
          usePdfStore.getState().updateTab(activeTab.id, { sidebarScrollPage: min });
        }
      },
      { root: grid, threshold: 0.1 },
    );

    grid.querySelectorAll<HTMLElement>("[data-page]").forEach((el) => obs.observe(el));
    return () => obs.disconnect();
  }, [activeTab?.id, activeTab?.pageCount]);

  const prevDocIdRef = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (activeTab?.docId !== prevDocIdRef.current) {
      setSelected(new Set());
      setSplitForm(null);
      prevDocIdRef.current = activeTab?.docId;
    }
  }, [activeTab?.docId]);

  // useLayoutEffect: fires after new DOM is committed but before the browser
  // paints. We clear drag state here (no flash) and kick off the FLIP animation
  // so the moved thumbnail appears to slide from its old position to its new one.
  useLayoutEffect(() => {
    const flip = pendingFlipRef.current;
    if (flip) {
      pendingFlipRef.current = null;
      const el = gridRef.current
        ?.querySelectorAll<HTMLElement>("[data-page]")
        [flip.toIdx];
      if (el) {
        // At this point dragIndex/dropIndex are still set, so the element at
        // flip.toIdx has a displacement transform. Back it out to get the natural
        // position, which is where the gap actually is.
        const appliedShift = flip.fromIdx < flip.toIdx ? -dragShift : dragShift;
        const naturalTop = el.getBoundingClientRect().top - appliedShift;
        const delta = flip.fromY - naturalTop;
        if (Math.abs(delta) > 2) {
          setFlipIndex(flip.toIdx);
          setFlipStyle({ transform: `translateY(${delta}px)`, transition: "none" });
          requestAnimationFrame(() => {
            setFlipStyle({ transition: "transform 250ms cubic-bezier(0.22,1,0.36,1)" });
            setTimeout(() => {
              setFlipIndex(null);
              setFlipStyle(undefined);
            }, 270);
          });
        }
      }
    }
    const keepSelection = lastOpRef.current === "rotate";
    lastOpRef.current = "other";
    if (!keepSelection) setSelected(new Set());
    setSplitForm(null);
    setDragIndex(null);
    setDropIndex(null);
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

  const selectAll = () =>
    setSelected(new Set(Array.from({ length: pageCount }, (_, i) => i + 1)));

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
      await invoke<PageInfo>("delete_pages", {
        docId,
        pageNumbers: Array.from(selected),
      });
    });

  const handleRotateCw = () => {
    lastOpRef.current = "rotate";
    runOp(async () => {
      await invoke<PageInfo>("rotate_pages", {
        docId,
        pageNumbers: Array.from(selected),
        clockwiseTurns: 1,
      });
    });
  };

  const handleRotateCcw = () => {
    lastOpRef.current = "rotate";
    runOp(async () => {
      await invoke<PageInfo>("rotate_pages", {
        docId,
        pageNumbers: Array.from(selected),
        clockwiseTurns: 3,
      });
    });
  };

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
      await invoke("split_document", { docId, firstPage: first, lastPage: last, destPath });
      setSplitForm(null);
    } catch (err) {
      await message(String(err), { title: "Split Failed", kind: "error" });
    } finally {
      setBusy(false);
    }
  };

  // ── Drag-and-drop reorder ─────────────────────────────────────────────────

  const handleDragStart = (index: number, e: React.DragEvent) => {
    setDragIndex(index);
    setFlipIndex(null);
    setFlipStyle(undefined);
    draggedHeightRef.current = (e.currentTarget as HTMLElement).getBoundingClientRect().height;
  };
  const handleDragOver = (e: React.DragEvent, index: number) => {
    e.preventDefault();
    setDropIndex(index);
  };
  const handleDragEnd = () => {
    if (dragIndex !== null && dropIndex !== null && dragIndex !== dropIndex) {
      // Record the dragged item's current screen position for the FLIP animation.
      const draggingEl = gridRef.current
        ?.querySelectorAll<HTMLElement>("[data-page]")
        [dragIndex];
      if (draggingEl) {
        pendingFlipRef.current = {
          fromY: draggingEl.getBoundingClientRect().top,
          fromIdx: dragIndex,
          toIdx: dropIndex,
        };
      }
      const order = Array.from({ length: pageCount }, (_, i) => i + 1);
      const [moved] = order.splice(dragIndex, 1);
      order.splice(dropIndex, 0, moved);
      // Keep dragIndex/dropIndex set — the gap stays visible while Rust runs.
      // useLayoutEffect clears them once pagesVersion updates.
      runOp(() => invoke<PageInfo>("reorder_pages", { docId, newOrder: order }));
    } else {
      setDragIndex(null);
      setDropIndex(null);
    }
  };

  const noneSelected = selected.size === 0;

  // Compute translateY for each item: FLIP animation takes priority over drag shifts.
  const dragShift = draggedHeightRef.current + 8; // 8 = .pages-grid gap

  function getThumbStyle(i: number): React.CSSProperties | undefined {
    if (flipIndex !== null && i === flipIndex) return flipStyle;
    if (dragIndex === null || dropIndex === null || dragIndex === dropIndex) return undefined;
    if (dragIndex < dropIndex && i > dragIndex && i <= dropIndex)
      return { transform: `translateY(-${dragShift}px)` };
    if (dragIndex > dropIndex && i >= dropIndex && i < dragIndex)
      return { transform: `translateY(${dragShift}px)` };
    return undefined;
  }

  return (
    <div className="pages-panel">
      {/* Header */}
      <div className="pages-panel-header">
        <span className="pages-panel-title">
          {pageCount} page{pageCount !== 1 ? "s" : ""}
        </span>
      </div>

      {/* Action bar */}
      <div className="pages-action-bar">
        <button
          className="pages-action-button"
          onClick={selected.size === pageCount ? clearSelection : selectAll}
        >
          {selected.size === pageCount ? "Deselect All" : "Select All"}
        </button>
        <button
          className="pages-action-button pages-action-danger"
          title="Delete selected pages"
          disabled={noneSelected || busy}
          onClick={handleDelete}
        >
          <Trash2 size={18} />
        </button>
        <button
          className="pages-action-button"
          title="Rotate selected pages clockwise"
          disabled={noneSelected || busy}
          onClick={handleRotateCw}
        >
          <RotateCw size={18} />
        </button>
        <button
          className="pages-action-button"
          title="Rotate selected pages counter-clockwise"
          disabled={noneSelected || busy}
          onClick={handleRotateCcw}
        >
          <RotateCcw size={18} />
        </button>
        <button
          className="pages-action-button"
          title="Merge another PDF into this document"
          disabled={busy}
          onClick={handleMerge}
        >
          <FileInput size={18} />
        </button>
        <button
          className={`pages-action-button ${splitForm ? "active" : ""}`}
          title="Split pages to a new PDF"
          disabled={busy}
          onClick={handleSplit}
        >
          <Scissors size={18} />
        </button>
      </div>

      {/* Split form */}
      {splitForm && (
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
          <button className="pages-split-save" onClick={handleSplit} disabled={busy}>
            Save…
          </button>
          <button className="pages-action-button" onClick={() => setSplitForm(null)}>
            ✕
          </button>
        </div>
      )}

      {/* Thumbnail grid */}
      <div ref={gridRef} className="pages-grid">
        {pageDimensions.map((dim, i) => {
          const pageNum = i + 1;
          return (
            <PageThumb
              key={`${docId}-v${pagesVersion}-${pageNum}`}
              docId={docId}
              pageNumber={pageNum}
              pageWidth={dim.width}
              pageHeight={dim.height}
              isActive={activeTab.currentPage === pageNum}
              selected={selected.has(pageNum)}
              isDragging={dragIndex === i}
              isDropTarget={dropIndex === i && dragIndex !== i}
              thumbStyle={getThumbStyle(i)}
              onSelect={() => toggleSelect(pageNum)}
              onClick={() =>
                usePdfStore.getState().updateTab(activeTab.id, { currentPage: pageNum })
              }
              onDragStart={(e) => handleDragStart(i, e)}
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
  selected: boolean;
  isDragging: boolean;
  isDropTarget: boolean;
  thumbStyle?: React.CSSProperties;
  onSelect: () => void;
  onClick: () => void;
  onDragStart: (e: React.DragEvent) => void;
  onDragOver: (e: React.DragEvent) => void;
  onDragEnd: () => void;
}

function PageThumb({
  docId,
  pageNumber,
  pageWidth,
  pageHeight,
  isActive,
  selected,
  isDragging,
  isDropTarget,
  thumbStyle,
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

  return (
    <div
      ref={containerRef}
      className={[
        "pages-thumb",
        isActive ? "active" : "",
        selected ? "selected" : "",
        isDragging ? "dragging" : "",
        isDropTarget ? "drop-target" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      style={thumbStyle}
      data-page={pageNumber}
      draggable
      onClick={onClick}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
    >
      <input
        type="checkbox"
        className="pages-thumb-checkbox"
        checked={selected}
        onChange={onSelect}
        onClick={(e) => e.stopPropagation()}
      />
      <div className="pages-thumb-canvas-area">
        <div className="pages-thumb-grip" title="Drag to reorder">
          <GripVertical size={18} />
        </div>
        <canvas ref={canvasRef} style={{ width: cssWidth, height: cssHeight }} />
        {failed && (
          <div className="thumbnail-error" style={{ width: cssWidth, height: cssHeight }}>
            <ImageOff size={16} />
          </div>
        )}
      </div>
      <span className="thumbnail-label">{pageNumber}</span>
    </div>
  );
}

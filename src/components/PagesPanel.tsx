import { useEffect, useLayoutEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { message } from "@tauri-apps/plugin-dialog";
import { ImageOff, RotateCw, RotateCcw, Trash2, Scissors, FileInput, GripVertical } from "lucide-react";
import { usePdfStore, suppressedReloadDocs } from "../store/usePdfStore";
import { permuteDoc, getThumb, putThumb, evictDoc } from "../utils/renderCache";

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
  const [clearingGap, setClearingGap] = useState(false);
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const [flipIndex, setFlipIndex] = useState<number | null>(null);
  const [flipStyle, setFlipStyle] = useState<React.CSSProperties | undefined>(undefined);
  // Bumped (in the same render that clears the drag transforms) to force every
  // thumbnail to repaint from the relabeled cache after an in-place reorder, so
  // the thumbnails never flash back through their original order. Distinct from
  // the store's pagesVersion, which is only bumped by destructive ops (delete,
  // rotate, merge) that must re-render from a freshly evicted cache.
  const [reorderEpoch, setReorderEpoch] = useState(0);
  const gridRef = useRef<HTMLDivElement>(null);
  const draggedHeightRef = useRef<number>(0);
  const dragOffsetRef = useRef({ x: 0, y: 0 });
  const lastOpRef = useRef<"rotate" | "other">("other");
  const pageCountRef = useRef(0);
  const docIdRef = useRef("");
  const isDraggingRef = useRef(false);

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

  useEffect(() => {
    if (clearingGap) setClearingGap(false);
  }, [clearingGap]);

  // Clear drag/animation state once the backend reload lands (pagesVersion bump).
  useLayoutEffect(() => {
    setFlipIndex(null);
    setFlipStyle(undefined);
    const keepSelection = lastOpRef.current === "rotate";
    lastOpRef.current = "other";
    if (!keepSelection) setSelected(new Set());
    setSplitForm(null);
    if (!isDraggingRef.current) {
      // setClearingGap batches with setDragIndex/setDropIndex into one render.
      // The .clearing CSS class on the grid sets transition:none on all thumbs
      // in that same render, so the browser has no transition to fire when the
      // gap transforms are removed. clearingGap resets after the next paint.
      setClearingGap(true);
      setDragIndex(null);
      setDropIndex(null);
    }
  }, [activeTab?.pagesVersion]);

  if (!activeTab) return null;

  const { docId, pageDimensions, pagesVersion } = activeTab;
  const pageCount = activeTab.pageCount;
  pageCountRef.current = pageCount;
  docIdRef.current = docId;

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
    isDraggingRef.current = true;
    setDragIndex(index);
    setFlipIndex(null);
    setFlipStyle(undefined);
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    draggedHeightRef.current = rect.height;
    dragOffsetRef.current = { x: e.clientX - rect.left, y: e.clientY - rect.top };
  };
  const handleDragOver = (e: React.DragEvent, index: number) => {
    e.preventDefault();
    setDropIndex(index);
  };
  const handleDragEnd = (e: React.DragEvent) => {
    if (dragIndex !== null && dropIndex !== null && dragIndex !== dropIndex) {
      const items = gridRef.current?.querySelectorAll<HTMLElement>("[data-page]");
      const draggingEl = items?.[dragIndex];
      const dropEl = items?.[dropIndex];

      // startX/startY: offset to apply so the element appears exactly where the
      // drag ghost was when the user released (ghost top-left = cursor - dragOffset).
      // endX/endY: where the element should end up — natural column x (0), gap y.
      let startX = 0, startY = 0, endY = 0;
      if (draggingEl && dropEl) {
        const rect = draggingEl.getBoundingClientRect();
        startX = e.clientX - dragOffsetRef.current.x - rect.left;
        startY = e.clientY - dragOffsetRef.current.y - rect.top;
        const appliedShift = dragIndex < dropIndex ? -dragShift : dragShift;
        endY = dropEl.getBoundingClientRect().top - appliedShift - rect.top;
      }

      const savedDragIndex = dragIndex;
      const savedDropIndex = dropIndex;

      // Lock out other operations for the duration of the animation + backend call.
      // Batched with Phase 1 state updates — single render, no extra paint.
      setBusy(true);

      // Phase 1 – teleport the element to where the ghost was released.
      // transition:none prevents the 150ms CSS rule from animating this snap.
      setFlipIndex(savedDragIndex);
      setFlipStyle({ opacity: 1, transform: `translate(${startX}px, ${startY}px)`, transition: "none" });

      requestAnimationFrame(() => {
        // Phase 2 – animate from ghost-release position to the gap.
        // endX is 0: the column is single-file, so x always returns to centre.
        setFlipStyle({
          opacity: 1,
          transform: `translate(0px, ${endY}px)`,
          transition: "transform 250ms cubic-bezier(0.22,1,0.36,1)",
        });

        setTimeout(() => {
          // Phase 3 – commit. The dragged thumb is frozen at its drop position
          // and the neighbours are shifted via transforms, so the new order is
          // already on screen using the *old* bitmaps. Now make that order real
          // without a destructive reload:
          //   1. Relabel both render caches so slot N maps to the bitmap that
          //      page N should now show (a reorder changes no pixels, only
          //      labels).
          //   2. Permute the store's page dimensions to match.
          //   3. Bump reorderEpoch and clear every drag transform in the SAME
          //      render. The thumbnails repaint from the relabeled cache (in a
          //      pre-paint layout effect) exactly as the transforms come off —
          //      so natural slot positions already show the correct content and
          //      nothing snaps back to the original order.
          //   4. Tell the reload listener to skip the heavyweight refresh when
          //      reorder_pages' event lands.
          // Refs hold current docId/pageCount in case the closure is stale.
          const docId = docIdRef.current;
          const order = Array.from({ length: pageCountRef.current }, (_, i) => i + 1);
          const [moved] = order.splice(savedDragIndex, 1);
          order.splice(savedDropIndex, 0, moved);

          permuteDoc(docId, order);
          suppressedReloadDocs.add(docId);

          const store = usePdfStore.getState();
          const tab = store.tabs.find((t) => t.docId === docId);
          if (tab) {
            const dims = tab.pageDimensions.slice();
            const [movedDim] = dims.splice(savedDragIndex, 1);
            dims.splice(savedDropIndex, 0, movedDim);
            store.updateTab(tab.id, { pageDimensions: dims, contentEpoch: tab.contentEpoch + 1 });
          }

          setReorderEpoch((e) => e + 1);
          setClearingGap(true);
          setFlipIndex(null);
          setFlipStyle(undefined);
          setDragIndex(null);
          setDropIndex(null);
          isDraggingRef.current = false;

          void (async () => {
            try {
              await invoke<PageInfo>("reorder_pages", { docId, newOrder: order });
            } catch (err) {
              // The file was not changed — roll back the optimistic reorder by
              // forcing a real reload from the backend's (unchanged) document.
              suppressedReloadDocs.delete(docId);
              evictDoc(docId);
              const s = usePdfStore.getState();
              const t = s.tabs.find((x) => x.docId === docId);
              if (t) {
                s.updateTab(t.id, {
                  pagesVersion: t.pagesVersion + 1,
                  contentEpoch: t.contentEpoch + 1,
                });
              }
              await message(String(err), { title: "Reorder Failed", kind: "error" });
            } finally {
              setBusy(false);
            }
          })();
        }, 260);
      });
    } else {
      isDraggingRef.current = false;
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
      <div ref={gridRef} className={`pages-grid${clearingGap ? " clearing" : ""}`}>
        {pageDimensions.map((dim, i) => {
          const pageNum = i + 1;
          return (
            <PageThumb
              key={`${docId}-${pageNum}`}
              renderKey={`${docId}-v${pagesVersion}-r${reorderEpoch}-${pageNum}`}
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
  renderKey: string;
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
  onDragEnd: (e: React.DragEvent) => void;
}

function PageThumb({
  renderKey,
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
  const visibleRef = useRef(false);
  const [failed, setFailed] = useState(false);

  const dpr = window.devicePixelRatio || 1;
  const cssWidth = Math.round(pageWidth * THUMBNAIL_SCALE);
  const cssHeight = Math.round(pageHeight * THUMBNAIL_SCALE);
  const renderWidth = Math.round(cssWidth * dpr);

  const draw = useCallback(
    (bitmap: ImageBitmap) => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      canvas.style.width = `${cssWidth}px`;
      canvas.style.height = `${cssHeight}px`;
      canvas.getContext("2d")?.drawImage(bitmap, 0, 0);
    },
    [cssWidth, cssHeight],
  );

  // Cache-first render. A cache hit draws synchronously (used by the reorder
  // repaint below so it lands in the same paint as the transform clear); a miss
  // fetches from Rust and keeps the old bitmap visible until the new one lands.
  const render = useCallback(async () => {
    const canvas = canvasRef.current;
    if (!canvas || !visibleRef.current) return;

    const cached = getThumb(docId, pageNumber, dpr);
    if (cached) {
      draw(cached);
      setFailed(false);
      return;
    }

    try {
      const buffer = await invoke<ArrayBuffer>("render_page", {
        docId,
        page: pageNumber,
        width: renderWidth,
      });
      const rgba = new Uint8ClampedArray(buffer);
      const actualHeight = rgba.byteLength / (4 * renderWidth);
      const imageData = new ImageData(rgba, renderWidth, actualHeight);
      const bitmap = await createImageBitmap(imageData);
      putThumb(docId, pageNumber, dpr, bitmap);
      draw(bitmap);
      setFailed(false);
    } catch {
      setFailed(true);
    }
  }, [docId, pageNumber, dpr, renderWidth, draw]);

  // Repaint when content changes: renderKey folds in pagesVersion (destructive
  // reload — cache was evicted, so this re-fetches) and reorderEpoch (in-place
  // reorder — cache was relabeled, so the cache hit redraws synchronously here,
  // pre-paint, with no flash). useLayoutEffect keeps the cached redraw in the
  // same frame the drag transforms are removed.
  useLayoutEffect(() => {
    render();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [renderKey]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          visibleRef.current = true;
          render();
        }
      },
      { threshold: 0.1 },
    );
    observer.observe(container);
    return () => observer.disconnect();
  }, [render]);

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

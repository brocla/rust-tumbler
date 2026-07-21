import { useEffect, useRef } from "react";
import { usePdfStore } from "../store/usePdfStore";
import type { TypewriterAnnot } from "../store/usePdfStore";
import { commitTypewriter, fontFamilyCss, newAnnot, rgbToHex } from "../utils/typewriter";

// Stable empty fallback: a zustand selector must not fabricate a new [] each
// render (a fresh reference re-renders forever).
const NO_ANNOTS: TypewriterAnnot[] = [];

interface TypewriterLayerProps {
  docId: string;
  pageNumber: number;
  zoom: number;
}

/**
 * Editable overlay for typewriter notes (issue #99). Renders each note on this
 * page as positioned text; the selected note becomes a textarea that can be
 * typed into, dragged (move handle), and resized (corner handle). Placement,
 * re-editing (double-click), and commit-on-click-away mirror standard text-box
 * behavior. This overlay is authoritative for what the user sees — the page
 * render leaves annotations off — while `commitTypewriter` writes the notes
 * into the buffer as FreeText annotations (a dirty buffer edit; Save commits).
 *
 * Modeled on RedactLayer: absolutely positioned, scaled by zoom/100, page
 * points with a top-left origin. The container only captures pointer events
 * while the tool is armed (to place a note); otherwise clicks pass through to
 * the text layer, and only the note boxes themselves stay interactive.
 */
export function TypewriterLayer({ docId, pageNumber, zoom }: TypewriterLayerProps) {
  const annots = usePdfStore(
    (s) => s.tabs.find((t) => t.docId === docId)?.typewriterAnnots ?? NO_ANNOTS,
  );
  const armed = usePdfStore((s) => s.typewriterMode);
  const activeId = usePdfStore((s) => s.activeTypewriterId);
  const style = usePdfStore((s) => s.typewriterStyle);
  const addTypewriterAnnot = usePdfStore((s) => s.addTypewriterAnnot);
  const updateTypewriterAnnot = usePdfStore((s) => s.updateTypewriterAnnot);
  const removeTypewriterAnnot = usePdfStore((s) => s.removeTypewriterAnnot);
  const setActiveTypewriter = usePdfStore((s) => s.setActiveTypewriter);

  const layerRef = useRef<HTMLDivElement>(null);
  const scale = zoom / 100;
  const pageAnnots = annots.filter((a) => a.page === pageNumber);
  const activeOnThisPage = pageAnnots.some((a) => a.id === activeId);

  const toPoints = (e: React.MouseEvent) => {
    const rect = layerRef.current?.getBoundingClientRect();
    if (!rect) return null;
    return { x: (e.clientX - rect.left) / scale, y: (e.clientY - rect.top) / scale };
  };

  // Commit the active note and deselect it. An empty note (placed but never
  // typed) is dropped rather than persisted.
  const deactivate = () => {
    const id = usePdfStore.getState().activeTypewriterId;
    if (!id) return;
    const tab = usePdfStore.getState().tabs.find((t) => t.docId === docId);
    const annot = tab?.typewriterAnnots?.find((a) => a.id === id);
    if (annot && annot.text.trim() === "") removeTypewriterAnnot(docId, id);
    setActiveTypewriter(null);
    void commitTypewriter(docId);
  };

  // While a note on this page is active, a click anywhere outside a note box
  // commits and deselects it (the standard "click away to finish" gesture),
  // covering clicks on the page, the panel, or another tab.
  useEffect(() => {
    if (!activeOnThisPage) return;
    const onMouseDown = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      // Keep the note active while interacting with it or with the panel's
      // style controls (which edit the selected note); commit on any other
      // click.
      if (target.closest(".typewriter-note") || target.closest(".typewriter-panel")) return;
      deactivate();
    };
    window.addEventListener("mousedown", onMouseDown);
    return () => window.removeEventListener("mousedown", onMouseDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeOnThisPage, docId]);

  // Place a new note on an empty-space click while armed. When a note is active,
  // the window handler above deactivates it first (this click just dismisses).
  const handleLayerMouseDown = (e: React.MouseEvent) => {
    if (!armed || e.button !== 0) return;
    if (e.target !== layerRef.current) return; // clicked a note — let it handle
    if (usePdfStore.getState().activeTypewriterId) return; // dismiss, don't place
    const p = toPoints(e);
    if (!p) return;
    e.preventDefault();
    const annot = newAnnot(pageNumber, p.x, p.y, style);
    addTypewriterAnnot(docId, annot);
    setActiveTypewriter(annot.id);
  };

  const beginMove = (e: React.MouseEvent, annot: TypewriterAnnot) => {
    e.preventDefault();
    e.stopPropagation();
    const start = { x: e.clientX, y: e.clientY, ox: annot.x, oy: annot.y };
    const onMove = (ev: MouseEvent) => {
      updateTypewriterAnnot(docId, annot.id, {
        x: Math.max(0, start.ox + (ev.clientX - start.x) / scale),
        y: Math.max(0, start.oy + (ev.clientY - start.y) / scale),
      });
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      void commitTypewriter(docId);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const beginResize = (e: React.MouseEvent, annot: TypewriterAnnot) => {
    e.preventDefault();
    e.stopPropagation();
    const start = { x: e.clientX, y: e.clientY, ow: annot.width, oh: annot.height };
    const onMove = (ev: MouseEvent) => {
      updateTypewriterAnnot(docId, annot.id, {
        width: Math.max(24, start.ow + (ev.clientX - start.x) / scale),
        height: Math.max(16, start.oh + (ev.clientY - start.y) / scale),
      });
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      void commitTypewriter(docId);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const deleteAnnot = (annot: TypewriterAnnot) => {
    removeTypewriterAnnot(docId, annot.id);
    if (activeId === annot.id) setActiveTypewriter(null);
    void commitTypewriter(docId);
  };

  // Nothing to draw and not armed: render nothing so clicks pass through.
  if (pageAnnots.length === 0 && !armed) return null;

  return (
    <div
      ref={layerRef}
      className={`typewriter-layer${armed ? " armed" : ""}`}
      data-testid={`typewriter-layer-${pageNumber}`}
      style={{ pointerEvents: armed || activeOnThisPage ? "auto" : "none" }}
      onMouseDown={handleLayerMouseDown}
    >
      {pageAnnots.map((annot) => {
        const active = annot.id === activeId;
        const box: React.CSSProperties = {
          position: "absolute",
          left: annot.x * scale,
          top: annot.y * scale,
          width: annot.width * scale,
          height: annot.height * scale,
          fontFamily: fontFamilyCss(annot.fontFamily),
          fontSize: annot.fontSize * scale,
          fontWeight: annot.bold ? "bold" : "normal",
          fontStyle: annot.italic ? "italic" : "normal",
          color: rgbToHex(annot.color),
          lineHeight: 1.2,
        };
        return (
          <div
            key={annot.id}
            className={`typewriter-note${active ? " active" : ""}`}
            data-testid={`typewriter-note-${annot.id}`}
            style={{ ...box, pointerEvents: "auto" }}
            onDoubleClick={() => setActiveTypewriter(annot.id)}
          >
            {active ? (
              <>
                <div className="typewriter-toolbar">
                  <span
                    className="typewriter-move"
                    title="Move"
                    onMouseDown={(e) => beginMove(e, annot)}
                  >
                    ✥
                  </span>
                  <button
                    className="typewriter-delete"
                    title="Delete note"
                    onClick={() => deleteAnnot(annot)}
                  >
                    ✕
                  </button>
                </div>
                <textarea
                  className="typewriter-input"
                  autoFocus
                  value={annot.text}
                  onChange={(e) =>
                    updateTypewriterAnnot(docId, annot.id, { text: e.target.value })
                  }
                  onKeyDown={(e) => {
                    if (e.key === "Escape") {
                      e.preventDefault();
                      deactivate();
                    }
                  }}
                  style={{
                    fontFamily: box.fontFamily,
                    fontSize: box.fontSize,
                    fontWeight: box.fontWeight,
                    fontStyle: box.fontStyle,
                    color: box.color,
                    lineHeight: box.lineHeight,
                  }}
                />
                <span
                  className="typewriter-resize"
                  title="Resize"
                  onMouseDown={(e) => beginResize(e, annot)}
                />
              </>
            ) : (
              <div className="typewriter-text">{annot.text}</div>
            )}
          </div>
        );
      })}
    </div>
  );
}

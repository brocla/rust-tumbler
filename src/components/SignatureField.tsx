import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { RotateCcw } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { evictPageCache } from "../utils/renderCache";

type Point = [number, number];
type Stroke = Point[];

interface SignatureFieldProps {
  docId: string;
  pageNumber: number;
  fieldId: string;
  rect: { x: number; y: number; width: number; height: number };
  zoom: number;
}

const clamp01 = (v: number) => (v < 0 ? 0 : v > 1 ? 1 : v);

/**
 * A drawn-signature draw target overlaid on a `/Sig` widget. Input is unified
 * across mouse, pen, touch, and trackpad via the Pointer Events API. Strokes
 * are kept as field-local normalized points and, on blur, sent to the backend
 * which writes them as a vector `/AP` appearance; pdfium then renders the
 * signature (so the canvas is a transparent "ghost" at rest and only opaque
 * while being drawn). No Clear button: Undo (Ctrl+Z) / Redo (Ctrl+Y) per
 * stroke, Esc to start over, and a hover-revealed reset when there's ink.
 */
export function SignatureField({ docId, pageNumber, fieldId, rect, zoom }: SignatureFieldProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const strokesRef = useRef<Stroke[]>([]);
  const redoRef = useRef<Stroke[]>([]);
  const drawingRef = useRef<Point[] | null>(null);
  const dirtyRef = useRef(false);
  const [hasInk, setHasInk] = useState(false);
  const [active, setActive] = useState(false);
  const [hovering, setHovering] = useState(false);
  const updateTab = usePdfStore((s) => s.updateTab);
  // A form-wide Clear/Reset bumps formEpoch and clears the backend appearance;
  // drop our in-memory strokes to match (otherwise re-focusing would redraw the
  // old signature).
  const formEpoch = usePdfStore(
    (s) => s.tabs.find((t) => t.docId === docId)?.formEpoch ?? 0,
  );

  const scale = zoom / 100;
  const cssW = rect.width * scale;
  const cssH = rect.height * scale;
  const dpr = window.devicePixelRatio || 1;

  // Draw the strokes onto the canvas. Only while active — at rest the canvas is
  // cleared/transparent so pdfium's committed appearance shows through.
  const redraw = () => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);
    if (!active) return;
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = "#000";
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    const all = drawingRef.current
      ? [...strokesRef.current, drawingRef.current]
      : strokesRef.current;
    for (const st of all) {
      if (st.length === 0) continue;
      ctx.beginPath();
      st.forEach(([nx, ny], i) => {
        const px = nx * cssW;
        const py = ny * cssH;
        if (i === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      });
      if (st.length === 1) ctx.lineTo(st[0][0] * cssW, st[0][1] * cssH); // a dot
      ctx.stroke();
    }
  };

  useEffect(redraw, [active, cssW, cssH, dpr]);

  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => {
    strokesRef.current = [];
    redoRef.current = [];
    drawingRef.current = null;
    dirtyRef.current = false;
    setHasInk(false);
    redraw();
  }, [formEpoch]);

  const norm = (e: React.PointerEvent): Point => {
    const r = canvasRef.current!.getBoundingClientRect();
    return [clamp01((e.clientX - r.left) / r.width), clamp01((e.clientY - r.top) / r.height)];
  };

  const onPointerDown = (e: React.PointerEvent) => {
    e.currentTarget.setPointerCapture?.(e.pointerId);
    setActive(true);
    drawingRef.current = [norm(e)];
    redoRef.current = [];
    redraw();
  };
  const onPointerMove = (e: React.PointerEvent) => {
    if (!drawingRef.current) return;
    drawingRef.current.push(norm(e));
    redraw();
  };
  const endStroke = () => {
    const st = drawingRef.current;
    drawingRef.current = null;
    if (st && st.length > 0) {
      strokesRef.current = [...strokesRef.current, st];
      dirtyRef.current = true;
      setHasInk(true);
      redraw();
    }
  };

  const undo = () => {
    const s = strokesRef.current;
    if (s.length === 0) return;
    redoRef.current = [...redoRef.current, s[s.length - 1]];
    strokesRef.current = s.slice(0, -1);
    dirtyRef.current = true;
    setHasInk(strokesRef.current.length > 0);
    redraw();
  };
  const redo = () => {
    const r = redoRef.current;
    if (r.length === 0) return;
    strokesRef.current = [...strokesRef.current, r[r.length - 1]];
    redoRef.current = r.slice(0, -1);
    dirtyRef.current = true;
    setHasInk(true);
    redraw();
  };
  const startOver = () => {
    if (strokesRef.current.length === 0 && !drawingRef.current) return;
    strokesRef.current = [];
    redoRef.current = [];
    drawingRef.current = null;
    dirtyRef.current = true;
    setHasInk(false);
    redraw();
  };

  // The hover-revealed reset is clicked from the resting (committed) state, so
  // it must persist the clear right away — Esc during a draw can wait for blur
  // because the active white canvas already hides the old ink.
  const startOverAndCommit = async () => {
    startOver();
    await commit();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.key.toLowerCase() === "z") {
      e.preventDefault();
      undo();
    } else if (ctrl && e.key.toLowerCase() === "y") {
      e.preventDefault();
      redo();
    } else if (e.key === "Escape") {
      e.preventDefault();
      startOver();
    }
  };

  // On blur, persist the strokes as the field's appearance and repaint the page
  // so pdfium renders them (comb-style: evict the cached bitmap + bump epoch).
  const commit = async () => {
    setActive(false);
    if (!dirtyRef.current) return;
    dirtyRef.current = false;
    try {
      await invoke("set_signature_strokes", {
        docId,
        fieldId,
        strokes: strokesRef.current,
      });
      evictPageCache(docId, pageNumber);
      const tab = usePdfStore.getState().tabs.find((t) => t.docId === docId);
      if (tab) updateTab(tab.id, { contentEpoch: tab.contentEpoch + 1 });
    } catch (err) {
      console.error(`Failed to save signature ${fieldId}:`, err);
    }
  };

  return (
    <div
      className="signature-field"
      style={{
        position: "absolute",
        left: rect.x * scale,
        top: rect.y * scale,
        width: cssW,
        height: cssH,
      }}
      onMouseEnter={() => setHovering(true)}
      onMouseLeave={() => setHovering(false)}
    >
      <canvas
        ref={canvasRef}
        className={`signature-canvas${active ? " active" : ""}`}
        width={Math.round(cssW * dpr)}
        height={Math.round(cssH * dpr)}
        style={{ width: cssW, height: cssH }}
        tabIndex={0}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endStroke}
        onPointerCancel={endStroke}
        onFocus={() => setActive(true)}
        onBlur={commit}
        onKeyDown={onKeyDown}
      />
      {(hovering || active) && hasInk && (
        <button
          type="button"
          className="signature-reset"
          title="Start over (or press Esc)"
          // Keep the canvas focused so blur doesn't fire mid-reset.
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => void startOverAndCommit()}
        >
          <RotateCcw size={14} />
        </button>
      )}
      {!hasInk && !active && <span className="signature-hint">Sign here</span>}
    </div>
  );
}

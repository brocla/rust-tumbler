import { useRef, useState } from "react";
import { usePdfStore } from "../store/usePdfStore";
import { INK_COLOR, INK_WIDTH_PT } from "../utils/ink";
import type { InkPoint, InkStroke } from "../utils/ink";

interface InkLayerProps {
  docId: string;
  pageNumber: number;
  zoom: number;
}

/**
 * Drawing surface for the Ink Signature tool (issue #120).
 *
 * Only ever shows the group *not yet committed*: once a group closes, its ink
 * is flattened into the page content stream and the page render draws it, so
 * this overlay has nothing left to show. That is the whole reason the tool
 * needs no re-hydration on open, unlike the typewriter's annotations.
 *
 * Points are stored in PDF points with a top-left origin — the space search and
 * redaction rects already use, and the space `apply_ink` expects — so what is
 * committed is exactly what was drawn, at whatever zoom it was drawn at. The
 * on-screen width is scaled so the pen keeps its weight as you zoom.
 */
export function InkLayer({ docId, pageNumber, zoom }: InkLayerProps) {
  const ink = usePdfStore((s) => s.ink);
  const inkBegin = usePdfStore((s) => s.inkBegin);
  const inkAddStroke = usePdfStore((s) => s.inkAddStroke);

  const layerRef = useRef<HTMLDivElement>(null);
  // The stroke under the pointer right now. Local state, not the store: it
  // changes on every pointermove, and only lands in the store on pointerup.
  const [drawing, setDrawing] = useState<InkStroke | null>(null);

  const scale = zoom / 100;
  const committed = ink && ink.docId === docId && ink.page === pageNumber ? ink.strokes : [];

  const toPoints = (e: React.PointerEvent): InkPoint | null => {
    const rect = layerRef.current?.getBoundingClientRect();
    if (!rect) return null;
    return [(e.clientX - rect.left) / scale, (e.clientY - rect.top) / scale];
  };

  const handlePointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    const p = toPoints(e);
    if (!p) return;
    e.preventDefault();
    // Opening the group here means the first pen-down on a page claims it; a
    // group already open for another page was closed by the page change.
    if (!ink || ink.docId !== docId || ink.page !== pageNumber) {
      inkBegin(docId, pageNumber);
    }
    e.currentTarget.setPointerCapture?.(e.pointerId);
    setDrawing([p]);
  };

  const handlePointerMove = (e: React.PointerEvent) => {
    if (!drawing) return;
    const p = toPoints(e);
    if (!p) return;
    setDrawing([...drawing, p]);
  };

  const endStroke = () => {
    if (!drawing) return;
    inkAddStroke(drawing);
    setDrawing(null);
  };

  const strokes = drawing ? [...committed, drawing] : committed;

  return (
    <div
      ref={layerRef}
      className="ink-layer"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={endStroke}
      onPointerCancel={endStroke}
    >
      <svg width="100%" height="100%">
        {strokes.map((stroke, i) => (
          <polyline
            key={i}
            points={stroke.map(([x, y]) => `${x * scale},${y * scale}`).join(" ")}
            fill="none"
            stroke={INK_COLOR}
            strokeWidth={INK_WIDTH_PT * scale}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        ))}
      </svg>
    </div>
  );
}

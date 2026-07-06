import { useRef, useState } from "react";
import { usePdfStore } from "../store/usePdfStore";
import type { RedactRegion } from "../store/usePdfStore";
import { dragToRegion } from "../utils/redaction";

// Stable fallback: a zustand selector must not fabricate a new [] per render
// (a fresh reference re-renders forever).
const NO_REGIONS: RedactRegion[] = [];

interface RedactLayerProps {
  docId: string;
  pageNumber: number;
  zoom: number;
}

/**
 * Overlay for pending redaction regions (issue #1): draws each region on this
 * page as a black box (click to remove), and — while the panel's "Draw region"
 * mode is armed — captures a marquee drag to add an area region. Cloned from
 * the HighlightLayer/TextLayer overlay pattern: absolutely positioned, scaled
 * by zoom/100.
 *
 * These boxes are the pre-Apply markers, not the redaction itself; the burn
 * happens in the backend flatten and is shown by the post-Apply preview.
 */
export function RedactLayer({ docId, pageNumber, zoom }: RedactLayerProps) {
  const regions = usePdfStore(
    (s) => s.tabs.find((t) => t.docId === docId)?.redactRegions ?? NO_REGIONS,
  );
  const drawMode = usePdfStore((s) => s.redactDrawMode);
  const removeRedactRegion = usePdfStore((s) => s.removeRedactRegion);
  const addRedactRegions = usePdfStore((s) => s.addRedactRegions);

  const layerRef = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<{ x1: number; y1: number; x2: number; y2: number } | null>(
    null,
  );

  const scale = zoom / 100;

  // Pointer position → page points (top-left origin).
  const toPoints = (e: React.MouseEvent) => {
    const rect = layerRef.current?.getBoundingClientRect();
    if (!rect) return null;
    return { x: (e.clientX - rect.left) / scale, y: (e.clientY - rect.top) / scale };
  };

  const handleMouseDown = (e: React.MouseEvent) => {
    if (!drawMode || e.button !== 0) return;
    const p = toPoints(e);
    if (!p) return;
    e.preventDefault();
    setDrag({ x1: p.x, y1: p.y, x2: p.x, y2: p.y });
  };

  const handleMouseMove = (e: React.MouseEvent) => {
    if (!drag) return;
    const p = toPoints(e);
    if (!p) return;
    setDrag({ ...drag, x2: p.x, y2: p.y });
  };

  const handleMouseUp = () => {
    if (!drag) return;
    const region = dragToRegion(pageNumber, drag.x1, drag.y1, drag.x2, drag.y2);
    if (region) addRedactRegions(docId, [region]);
    setDrag(null);
  };

  const pending = drag ? dragToRegion(pageNumber, drag.x1, drag.y1, drag.x2, drag.y2) : null;

  if (!drawMode && regions.every((r) => r.page !== pageNumber)) return null;

  return (
    <div
      ref={layerRef}
      className={`redact-layer${drawMode ? " drawing" : ""}`}
      data-testid={`redact-layer-${pageNumber}`}
      onMouseDown={handleMouseDown}
      onMouseMove={handleMouseMove}
      onMouseUp={handleMouseUp}
      onMouseLeave={handleMouseUp}
    >
      {regions.map((region, index) =>
        region.page === pageNumber ? (
          <div
            key={index}
            className="redact-region"
            title="Marked for redaction — click to remove"
            onClick={() => removeRedactRegion(docId, index)}
            style={{
              position: "absolute",
              left: region.rect.x * scale,
              top: region.rect.y * scale,
              width: region.rect.width * scale,
              height: region.rect.height * scale,
            }}
          />
        ) : null,
      )}
      {pending && (
        <div
          className="redact-region pending"
          style={{
            position: "absolute",
            left: pending.rect.x * scale,
            top: pending.rect.y * scale,
            width: pending.rect.width * scale,
            height: pending.rect.height * scale,
          }}
        />
      )}
    </div>
  );
}

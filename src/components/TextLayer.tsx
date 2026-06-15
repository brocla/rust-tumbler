import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface TextItem {
  text: string;
  x: number;
  y: number;
  width: number;
  height: number;
  fontSize: number;
}

interface TextLayerProps {
  docId: string;
  pageNumber: number;
  zoom: number;
}

let measureCtx: CanvasRenderingContext2D | null = null;

// The text layer renders with a generic "serif" font, which has different
// glyph metrics than the PDF's embedded font. Without correction, the
// selection highlight (sized to the rendered glyphs) drifts from the
// PDF-rendered text on the canvas underneath. We measure the natural
// width and scale each span horizontally so it spans exactly item.width,
// matching the canvas rendering (same approach pdf.js uses).
function measureTextWidth(text: string, fontSizePx: number): number {
  if (!measureCtx) {
    measureCtx = document.createElement("canvas").getContext("2d");
  }
  if (!measureCtx) return 0;
  measureCtx.font = `${fontSizePx}px serif`;
  return measureCtx.measureText(text).width;
}

export function TextLayer({
  docId,
  pageNumber,
  zoom,
}: TextLayerProps) {
  const [textItems, setTextItems] = useState<TextItem[]>([]);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const items = await invoke<TextItem[]>("extract_page_text", {
          docId,
          page: pageNumber,
        });
        if (!cancelled) setTextItems(items);
      } catch (err) {
        console.error(`Failed to extract text for page ${pageNumber}:`, err);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [docId, pageNumber]);

  const scale = zoom / 100;

  // Group items into lines so the global copy handler can insert "\n"
  // between lines (absolutely positioned spans don't trigger the browser's
  // normal block-level line-break serialization on copy). Items on the same
  // visual line can still have different y/height — e.g. a list number set
  // in a larger font than its item text — so group by vertical overlap with
  // the previous item rather than a fixed y tolerance.
  const spans = useMemo(() => {
    let lineIndex = 0;
    let prevTop: number | null = null;
    let prevBottom: number | null = null;
    return textItems.map((item) => {
      const itemBottom = item.y + item.height;
      if (prevTop !== null && prevBottom !== null) {
        const overlaps = item.y < prevBottom && itemBottom > prevTop;
        if (!overlaps) lineIndex++;
      }
      prevTop = item.y;
      prevBottom = itemBottom;

      const fontSizePx = item.fontSize * scale;
      const targetWidth = item.width * scale;
      const measuredWidth = measureTextWidth(item.text, fontSizePx);
      const scaleX = measuredWidth > 0 ? targetWidth / measuredWidth : 1;
      return { item, fontSizePx, targetWidth, scaleX, lineIndex };
    });
  }, [textItems, scale]);

  return (
    <div className="text-layer">
      {spans.map(({ item, fontSizePx, targetWidth, scaleX, lineIndex }, i) => (
        <span
          key={i}
          data-line={`${pageNumber}-${lineIndex}`}
          data-x={item.x}
          data-right={item.x + item.width}
          data-font-size={item.fontSize}
          style={{
            position: "absolute",
            left: item.x * scale,
            top: item.y * scale,
            width: targetWidth,
            height: item.height * scale,
            fontSize: fontSizePx,
            lineHeight: `${item.height * scale}px`,
            fontFamily: "serif",
            color: "transparent",
            whiteSpace: "pre",
            userSelect: "text",
            WebkitUserSelect: "text",
            transform: `scaleX(${scaleX})`,
            transformOrigin: "0 0",
          }}
        >
          {item.text}
        </span>
      ))}
    </div>
  );
}

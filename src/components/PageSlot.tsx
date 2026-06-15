import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCached, putCached } from "../utils/renderCache";
import { TextLayer } from "./TextLayer";
import { HighlightLayer } from "./HighlightLayer";

interface HighlightRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface PageSlotProps {
  docId: string;
  pageNumber: number;
  pageWidth: number;
  pageHeight: number;
  zoom: number;
  isInRenderWindow: boolean;
  displayMode: "normal" | "invert" | "sepia";
  highlightRects: HighlightRect[];
  activeHighlightIndex: number;
}

const DISPLAY_FILTERS = {
  normal: "none",
  invert: "invert(1) hue-rotate(180deg)",
  sepia: "sepia(0.6) brightness(0.9)",
};

export function PageSlot({
  docId,
  pageNumber,
  pageWidth,
  pageHeight,
  zoom,
  isInRenderWindow,
  displayMode,
  highlightRects,
  activeHighlightIndex,
}: PageSlotProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [rendered, setRendered] = useState(false);
  const [failed, setFailed] = useState(false);
  const renderIdRef = useRef(0);

  const dpr = window.devicePixelRatio || 1;
  const scale = (zoom / 100) * dpr;
  const cssWidth = (pageWidth * zoom) / 100;
  const cssHeight = (pageHeight * zoom) / 100;
  const pixelWidth = Math.round(pageWidth * scale);

  useEffect(() => {
    if (!isInRenderWindow) {
      setRendered(false);
      return;
    }

    const canvas = canvasRef.current;
    if (!canvas) return;

    const renderId = ++renderIdRef.current;
    setFailed(false);

    const cached = getCached(docId, pageNumber, zoom, dpr);
    if (cached) {
      canvas.width = cached.width;
      canvas.height = cached.height;
      canvas.style.width = `${cssWidth}px`;
      canvas.style.height = `${cssHeight}px`;
      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.drawImage(cached, 0, 0);
        setRendered(true);
      }
      return;
    }

    let cancelled = false;

    (async () => {
      try {
        const buffer = await invoke<ArrayBuffer>("render_page", {
          docId,
          page: pageNumber,
          width: pixelWidth,
        });

        if (cancelled || renderId !== renderIdRef.current) return;

        const rgba = new Uint8ClampedArray(buffer);
        // pdfium may return slightly different height than requested
        const actualHeight = rgba.byteLength / (4 * pixelWidth);
        const imageData = new ImageData(rgba, pixelWidth, actualHeight);
        const bitmap = await createImageBitmap(imageData);

        if (cancelled || renderId !== renderIdRef.current) {
          bitmap.close();
          return;
        }

        putCached(docId, pageNumber, zoom, dpr, bitmap);

        canvas.width = pixelWidth;
        canvas.height = actualHeight;
        canvas.style.width = `${cssWidth}px`;
        canvas.style.height = `${cssHeight}px`;
        const ctx = canvas.getContext("2d");
        if (ctx) {
          ctx.drawImage(bitmap, 0, 0);
          setRendered(true);
        }
      } catch (err) {
        console.error(`Failed to render page ${pageNumber}:`, err);
        if (!cancelled && renderId === renderIdRef.current) setFailed(true);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [docId, pageNumber, zoom, dpr, isInRenderWindow, pixelWidth, cssWidth, cssHeight]);

  const filter = DISPLAY_FILTERS[displayMode];

  if (!isInRenderWindow) {
    return (
      <div
        className="page-slot placeholder"
        style={{ width: cssWidth, height: cssHeight }}
      />
    );
  }

  return (
    <div
      className="page-slot"
      style={{ width: cssWidth, height: cssHeight }}
    >
      <canvas
        ref={canvasRef}
        style={{
          filter: filter === "none" ? undefined : filter,
          opacity: rendered ? 1 : 0,
        }}
      />
      {rendered && (
        <>
          <TextLayer
            docId={docId}
            pageNumber={pageNumber}
            zoom={zoom}
          />
          <HighlightLayer
            rects={highlightRects}
            activeIndex={activeHighlightIndex}
            zoom={zoom}
          />
        </>
      )}
      {!rendered && !failed && <div className="page-loading">Loading...</div>}
      {failed && <div className="page-error">Failed to load page {pageNumber}</div>}
    </div>
  );
}

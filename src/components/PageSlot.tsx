import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCached, putCached } from "../utils/renderCache";
import { redactPreviewCacheId } from "../utils/redactSave";
import { TextLayer } from "./TextLayer";
import { FormLayer } from "./FormLayer";
import { HighlightLayer } from "./HighlightLayer";
import { RedactLayer } from "./RedactLayer";

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
  // Bumped on an in-place reorder. The render cache has been relabeled to match,
  // so re-running the render effect repaints this slot from cache (no blank).
  contentEpoch: number;
  displayMode: "normal" | "invert" | "sepia";
  highlightRects: HighlightRect[];
  activeHighlightIndex: number;
  // True while the tab previews a staged redacted copy (issue #1): the page
  // is rendered from the staged bytes via render_redacted_page (cached under
  // a separate key), and the interactive overlays are hidden — the preview is
  // the flattened raster itself.
  redactedPreview?: boolean;
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
  contentEpoch,
  displayMode,
  highlightRects,
  activeHighlightIndex,
  redactedPreview = false,
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

    // Preview renders come from the staged redacted bytes and are cached under
    // their own key so they never mix with the document's own renders.
    const cacheDocId = redactedPreview ? redactPreviewCacheId(docId) : docId;
    const renderCommand = redactedPreview ? "render_redacted_page" : "render_page";

    const cached = getCached(cacheDocId, pageNumber, zoom, dpr);
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
        const buffer = await invoke<ArrayBuffer>(renderCommand, {
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

        putCached(cacheDocId, pageNumber, zoom, dpr, bitmap);

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
  }, [docId, pageNumber, zoom, dpr, isInRenderWindow, contentEpoch, pixelWidth, cssWidth, cssHeight, redactedPreview]);

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
      {rendered && !redactedPreview && (
        <>
          <TextLayer
            docId={docId}
            pageNumber={pageNumber}
            zoom={zoom}
          />
          <FormLayer
            docId={docId}
            pageNumber={pageNumber}
            zoom={zoom}
          />
          <HighlightLayer
            rects={highlightRects}
            activeIndex={activeHighlightIndex}
            zoom={zoom}
          />
          <RedactLayer
            docId={docId}
            pageNumber={pageNumber}
            zoom={zoom}
          />
        </>
      )}
      {!rendered && !failed && <div className="page-loading">Loading...</div>}
      {failed && <div className="page-error">Failed to load page {pageNumber}</div>}
    </div>
  );
}

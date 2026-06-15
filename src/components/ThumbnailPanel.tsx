import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ImageOff } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";

const THUMBNAIL_SCALE = 0.18;

export function ThumbnailPanel() {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const updateTab = usePdfStore((s) => s.updateTab);

  if (!activeTab) return null;

  return (
    <div className="thumbnail-panel">
      {activeTab.pageDimensions.map((dim, i) => (
        <Thumbnail
          key={`${activeTab.docId}-${i + 1}`}
          docId={activeTab.docId}
          pageNumber={i + 1}
          pageWidth={dim.width}
          pageHeight={dim.height}
          isActive={activeTab.currentPage === i + 1}
          onClick={() => updateTab(activeTab.id, { currentPage: i + 1 })}
        />
      ))}
    </div>
  );
}

interface ThumbnailProps {
  docId: string;
  pageNumber: number;
  pageWidth: number;
  pageHeight: number;
  isActive: boolean;
  onClick: () => void;
}

function Thumbnail({
  docId,
  pageNumber,
  pageWidth,
  pageHeight,
  isActive,
  onClick,
}: ThumbnailProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [rendered, setRendered] = useState(false);
  const [failed, setFailed] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  const cssWidth = Math.round(pageWidth * THUMBNAIL_SCALE);
  const cssHeight = Math.round(pageHeight * THUMBNAIL_SCALE);

  const renderThumb = useCallback(async () => {
    const canvas = canvasRef.current;
    if (!canvas || rendered || failed) return;

    const dpr = window.devicePixelRatio || 1;
    const renderWidth = Math.round(cssWidth * dpr);
    const renderHeight = Math.round(cssHeight * dpr);

    try {
      const buffer = await invoke<ArrayBuffer>("render_page", {
        docId,
        page: pageNumber,
        width: renderWidth,
        height: renderHeight,
      });

      const rgba = new Uint8ClampedArray(buffer);
      // pdfium may return a bitmap with height slightly different from requested
      // (set_target_width is exact, set_maximum_height is a cap)
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
    } catch (err) {
      console.error(`Failed to render thumbnail page ${pageNumber}:`, err);
      setFailed(true);
    }
  }, [docId, pageNumber, cssWidth, cssHeight, rendered, failed]);

  // Lazy render when visible
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          renderThumb();
        }
      },
      { threshold: 0.1 },
    );

    observer.observe(container);
    return () => observer.disconnect();
  }, [renderThumb]);

  return (
    <div
      ref={containerRef}
      className={`thumbnail ${isActive ? "active" : ""}`}
      onClick={onClick}
    >
      <canvas ref={canvasRef} style={{ width: cssWidth, height: cssHeight }} />
      {failed && (
        <div
          className="thumbnail-error"
          style={{ width: cssWidth, height: cssHeight }}
          title={`Failed to load page ${pageNumber}`}
        >
          <ImageOff size={16} />
        </div>
      )}
      <span className="thumbnail-label">{pageNumber}</span>
    </div>
  );
}

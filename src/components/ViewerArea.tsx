import { useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { FileText } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { bgraToRgba } from "../utils/bgraConvert";

export function ViewerArea() {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const canvasRef = useRef<HTMLCanvasElement>(null);

  const renderPage = useCallback(
    async (docId: string, page: number, pageWidth: number, pageHeight: number) => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const dpr = window.devicePixelRatio || 1;
      const zoom = activeTab?.zoom ?? 100;
      const scale = (zoom / 100) * dpr;

      const renderWidth = Math.round(pageWidth * scale);
      const renderHeight = Math.round(pageHeight * scale);

      const buffer = await invoke<ArrayBuffer>("render_page", {
        docId,
        page,
        width: renderWidth,
        height: renderHeight,
      });

      const rgba = bgraToRgba(buffer);
      const imageData = new ImageData(rgba, renderWidth, renderHeight);

      canvas.width = renderWidth;
      canvas.height = renderHeight;
      canvas.style.width = `${renderWidth / dpr}px`;
      canvas.style.height = `${renderHeight / dpr}px`;

      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.putImageData(imageData, 0, 0);
      }
    },
    [activeTab?.zoom],
  );

  useEffect(() => {
    if (!activeTab) return;
    const { docId, currentPage, pageDimensions } = activeTab;
    if (!docId || pageDimensions.length === 0) return;

    const dim = pageDimensions[currentPage - 1];
    if (!dim) return;

    renderPage(docId, currentPage, dim.width, dim.height);
  }, [activeTab?.docId, activeTab?.currentPage, activeTab?.zoom, renderPage]);

  if (!activeTab) {
    return (
      <div className="empty-state">
        <FileText size={64} className="empty-state-icon" />
        <div className="empty-state-text">No document open</div>
        <div className="empty-state-hint">
          Press Ctrl+O or click the open button to load a PDF
        </div>
      </div>
    );
  }

  return (
    <div className="viewer-scroll-container">
      <div className="page-wrapper">
        <canvas ref={canvasRef} />
      </div>
      <div className="viewer-status">
        Page {activeTab.currentPage} of {activeTab.pageCount} &mdash;{" "}
        {activeTab.fileName}
      </div>
    </div>
  );
}

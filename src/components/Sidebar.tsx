import { useRef, useCallback, useEffect } from "react";
import { usePdfStore } from "../store/usePdfStore";
import { SearchPanel } from "./SearchPanel";
import { ThumbnailPanel } from "./ThumbnailPanel";
import { MetadataPanel } from "./MetadataPanel";
import { PagesPanel } from "./PagesPanel";
import { OptimizePanel } from "./OptimizePanel";

const MIN_WIDTH = 150;
const MAX_WIDTH = 500;

// Sidebar tools that read/write the document via lopdf and so can't operate on
// a still-encrypted (view-only) buffer. (issue #12)
const ENCRYPTED_DISABLED_TOOLS = new Set(["metadata", "pages", "optimize"]);

export function Sidebar() {
  const activeTool = usePdfStore((s) => s.activeSidebarTool);
  const encrypted = usePdfStore((s) => {
    const t = s.tabs.find((tab) => tab.id === s.activeTabId);
    return !!t?.encrypted;
  });
  const sidebarWidth = usePdfStore((s) => s.sidebarWidth);
  const setSidebarWidth = usePdfStore((s) => s.setSidebarWidth);
  const dragRef = useRef<{ startX: number; startWidth: number } | null>(null);

  const handleMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      dragRef.current = { startX: e.clientX, startWidth: sidebarWidth };
    },
    [sidebarWidth],
  );

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!dragRef.current) return;
      const delta = e.clientX - dragRef.current.startX;
      const newWidth = Math.max(
        MIN_WIDTH,
        Math.min(MAX_WIDTH, dragRef.current.startWidth + delta),
      );
      setSidebarWidth(newWidth);
    };

    const handleMouseUp = () => {
      if (dragRef.current) {
        dragRef.current = null;
        // Persist to localStorage
        localStorage.setItem(
          "tumbler-sidebar-width",
          String(usePdfStore.getState().sidebarWidth),
        );
      }
    };

    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };
  }, [setSidebarWidth]);

  // Restore width from localStorage on mount
  useEffect(() => {
    const saved = localStorage.getItem("tumbler-sidebar-width");
    if (saved) {
      const width = parseInt(saved, 10);
      if (width >= MIN_WIDTH && width <= MAX_WIDTH) {
        setSidebarWidth(width);
      }
    }
  }, [setSidebarWidth]);

  if (!activeTool) return null;

  // The tool can stay selected across a tab switch (it's global, not per-tab),
  // so an encrypted tab may inherit an edit tool from a previous document.
  // Show a note instead of a panel that would read the encrypted buffer.
  const toolDisabled = encrypted && ENCRYPTED_DISABLED_TOOLS.has(activeTool);

  return (
    <div className="sidebar" style={{ width: sidebarWidth }}>
      <div className="sidebar-content">
        {toolDisabled ? (
          <p className="sidebar-encrypted-note">
            This tool isn't available for password-protected PDFs. The document
            opened in view-only mode.
          </p>
        ) : (
          <>
            {activeTool === "thumbnails" && <ThumbnailPanel />}
            {activeTool === "search" && <SearchPanel />}
            {activeTool === "metadata" && <MetadataPanel />}
            {activeTool === "pages" && <PagesPanel />}
            {activeTool === "optimize" && <OptimizePanel />}
          </>
        )}
      </div>
      <div className="sidebar-resize-handle" onMouseDown={handleMouseDown} />
    </div>
  );
}

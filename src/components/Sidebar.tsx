import { useRef, useCallback, useEffect } from "react";
import { usePdfStore } from "../store/usePdfStore";
import { SearchPanel } from "./SearchPanel";
import { ThumbnailPanel } from "./ThumbnailPanel";
import { MetadataPanel } from "./MetadataPanel";
import { PagesPanel } from "./PagesPanel";
import { OptimizePanel } from "./OptimizePanel";
import { RedactPanel } from "./RedactPanel";
import { TypewriterPanel } from "./TypewriterPanel";

const MIN_WIDTH = 150;
const MAX_WIDTH = 500;

export function Sidebar() {
  const activeTool = usePdfStore((s) => s.activeSidebarTool);
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

  return (
    <div className="sidebar" style={{ width: sidebarWidth }}>
      <div className="sidebar-content">
        {activeTool === "thumbnails" && <ThumbnailPanel />}
        {activeTool === "search" && <SearchPanel />}
        {activeTool === "metadata" && <MetadataPanel />}
        {activeTool === "pages" && <PagesPanel />}
        {activeTool === "optimize" && <OptimizePanel />}
        {activeTool === "redact" && <RedactPanel />}
        {activeTool === "typewriter" && <TypewriterPanel />}
      </div>
      <div className="sidebar-resize-handle" onMouseDown={handleMouseDown} />
    </div>
  );
}

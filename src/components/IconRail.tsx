import { Layers, Search, Info, PocketKnife, Minimize2 } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";

export function IconRail() {
  const activeTool = usePdfStore((s) => s.activeSidebarTool);
  const setSidebarTool = usePdfStore((s) => s.setSidebarTool);
  const hasActiveTab = usePdfStore(
    (s) => s.activeTabId !== null && s.tabs.some((t) => t.id === s.activeTabId),
  );

  if (!hasActiveTab) return <div className="icon-rail" />;

  return (
    <div className="icon-rail">
      <button
        className={`rail-button ${activeTool === "thumbnails" ? "active" : ""}`}
        onClick={() => setSidebarTool("thumbnails")}
        title="Thumbnails"
      >
        <Layers size={20} />
      </button>
      <button
        className={`rail-button ${activeTool === "search" ? "active" : ""}`}
        onClick={() => setSidebarTool("search")}
        title="Search"
      >
        <Search size={20} />
      </button>
      <button
        className={`rail-button ${activeTool === "metadata" ? "active" : ""}`}
        onClick={() => setSidebarTool("metadata")}
        title="Metadata"
      >
        <Info size={20} />
      </button>
      <button
        className={`rail-button ${activeTool === "pages" ? "active" : ""}`}
        onClick={() => setSidebarTool("pages")}
        title="Page Operations"
      >
        <PocketKnife size={20} />
      </button>
      <button
        className={`rail-button ${activeTool === "optimize" ? "active" : ""}`}
        onClick={() => setSidebarTool("optimize")}
        title="Compress"
      >
        <Minimize2 size={20} />
      </button>
    </div>
  );
}

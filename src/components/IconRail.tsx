import { Layers, Search, Info, PocketKnife, Minimize2, EyeOff, Type } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";

export function IconRail() {
  const activeTool = usePdfStore((s) => s.activeSidebarTool);
  const setSidebarTool = usePdfStore((s) => s.setSidebarTool);
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );

  if (!activeTab) return <div className="icon-rail" />;

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
        title="Web Optimization"
      >
        <Minimize2 size={20} />
      </button>
      <button
        className={`rail-button ${activeTool === "redact" ? "active" : ""}`}
        onClick={() => setSidebarTool("redact")}
        title="Redact"
      >
        {/* Mirrored: the slash reads better rising left-to-right. */}
        <EyeOff size={20} style={{ transform: "scaleX(-1)" }} />
      </button>
      <button
        className={`rail-button ${activeTool === "typewriter" ? "active" : ""}`}
        onClick={() => setSidebarTool("typewriter")}
        title="Typewriter"
      >
        <Type size={20} />
      </button>
    </div>
  );
}

import { Layers, Search, Info, PocketKnife, Minimize2 } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";

// Tooltip on rail tools disabled for a password-protected (view-only) PDF
// (issue #12). Metadata, page operations, and compression all read or write
// the document via lopdf, which can't touch the still-encrypted buffer.
const ENCRYPTED_DISABLED_TITLE = (tool: string) =>
  `${tool} isn't available for password-protected PDFs (view-only)`;

export function IconRail() {
  const activeTool = usePdfStore((s) => s.activeSidebarTool);
  const setSidebarTool = usePdfStore((s) => s.setSidebarTool);
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );

  if (!activeTab) return <div className="icon-rail" />;

  const encrypted = !!activeTab.encrypted;

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
        disabled={encrypted}
        title={encrypted ? ENCRYPTED_DISABLED_TITLE("Metadata") : "Metadata"}
      >
        <Info size={20} />
      </button>
      <button
        className={`rail-button ${activeTool === "pages" ? "active" : ""}`}
        onClick={() => setSidebarTool("pages")}
        disabled={encrypted}
        title={
          encrypted ? ENCRYPTED_DISABLED_TITLE("Page operations") : "Page Operations"
        }
      >
        <PocketKnife size={20} />
      </button>
      <button
        className={`rail-button ${activeTool === "optimize" ? "active" : ""}`}
        onClick={() => setSidebarTool("optimize")}
        disabled={encrypted}
        title={encrypted ? ENCRYPTED_DISABLED_TITLE("Compression") : "Compress"}
      >
        <Minimize2 size={20} />
      </button>
    </div>
  );
}

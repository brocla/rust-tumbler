import { useEffect, useRef } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { Toolbar } from "./components/Toolbar";
import { IconRail } from "./components/IconRail";
import { Sidebar } from "./components/Sidebar";
import { ViewerArea } from "./components/ViewerArea";
import { usePdfStore } from "./store/usePdfStore";
import type { PageDimension } from "./store/usePdfStore";

interface DocInfo {
  docId: string;
  pageCount: number;
  pageDimensions: PageDimension[];
}

function App() {
  const addTab = usePdfStore((s) => s.addTab);
  const openFileRef = useRef<() => Promise<void>>();

  // Shared file-open logic used by both Ctrl+O and toolbar button
  openFileRef.current = async () => {
    const selected = await open({
      multiple: false,
      filters: [{ name: "PDF Documents", extensions: ["pdf"] }],
    });
    if (!selected) return;

    const path = typeof selected === "string" ? selected : selected;

    try {
      const info = await invoke<DocInfo>("open_document", { path });
      const tabId = crypto.randomUUID();
      addTab({
        id: tabId,
        docId: info.docId,
        fileName: path.split(/[\\/]/).pop() ?? "Untitled",
        pageCount: info.pageCount,
        pageDimensions: info.pageDimensions,
        currentPage: 1,
        scrollTop: 0,
        zoom: 100,
        zoomMode: "numeric",
        displayMode: "normal",
        searchQuery: "",
        searchResults: [],
        searchResultIndex: -1,
        metadataDirty: false,
        loading: false,
      });
    } catch (err) {
      console.error("Failed to open document:", err);
    }
  };

  // Ctrl+O shortcut
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "o") {
        e.preventDefault();
        openFileRef.current?.();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  // Ctrl+F — open search panel, focus and select input
  // Uses capture phase to intercept before WebView2's native find dialog
  useEffect(() => {
    const handleCtrlF = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "f") {
        e.preventDefault();
        e.stopImmediatePropagation();
        const store = usePdfStore.getState();
        if (store.activeSidebarTool !== "search") {
          store.setSidebarTool("search");
        }
        // Allow SearchPanel to mount, then focus + select its input
        setTimeout(() => {
          const input = document.querySelector<HTMLInputElement>(".search-input");
          if (input) {
            input.focus();
            input.select();
          }
        }, 50);
      }
    };
    window.addEventListener("keydown", handleCtrlF, { capture: true });
    return () => window.removeEventListener("keydown", handleCtrlF, { capture: true });
  }, []);

  return (
    <div className="app-shell">
      <Toolbar onOpenFile={() => openFileRef.current?.()} />
      <div className="app-body">
        <IconRail />
        <Sidebar />
        <div className="viewer-area">
          <ViewerArea />
        </div>
      </div>
    </div>
  );
}

export default App;

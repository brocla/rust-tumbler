import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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
  const printRef = useRef<() => Promise<void>>();
  const [printProgress, setPrintProgress] = useState<{ page: number; total: number } | null>(null);

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

  // Shared print logic
  printRef.current = async () => {
    const tab = usePdfStore.getState().getActiveTab();
    if (!tab) return;
    try {
      setPrintProgress({ page: 0, total: tab.pageCount });
      await invoke("print_document", { docId: tab.docId });
    } catch (err) {
      console.error("Print failed:", err);
    } finally {
      setPrintProgress(null);
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

  // Listen for print progress events from Rust
  useEffect(() => {
    const unlisten = listen<{ page: number; total: number }>("print-progress", (event) => {
      setPrintProgress(event.payload);
    });
    return () => { unlisten.then((f) => f()); };
  }, []);

  // Ctrl+P shortcut
  useEffect(() => {
    const handleCtrlP = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key === "p") {
        e.preventDefault();
        printRef.current?.();
      }
    };
    window.addEventListener("keydown", handleCtrlP);
    return () => window.removeEventListener("keydown", handleCtrlP);
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
      <Toolbar onOpenFile={() => openFileRef.current?.()} onPrint={() => printRef.current?.()} />
      <div className="app-body">
        <IconRail />
        <Sidebar />
        <div className="viewer-area">
          <ViewerArea />
        </div>
      </div>
      {printProgress && (
        <div className="print-progress-overlay">
          <div className="print-progress-dialog">
            <p>Printing page {printProgress.page} of {printProgress.total}...</p>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;

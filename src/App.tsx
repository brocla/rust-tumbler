import { useEffect, useRef, useState } from "react";
import { message, open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Toolbar } from "./components/Toolbar";
import { TabBar } from "./components/TabBar";
import { IconRail } from "./components/IconRail";
import { Sidebar } from "./components/Sidebar";
import { ViewerArea } from "./components/ViewerArea";
import { usePdfStore } from "./store/usePdfStore";
import type { PageDimension } from "./store/usePdfStore";
import { contrastTextColor } from "./utils/color";

interface DocInfo {
  docId: string;
  pageCount: number;
  pageDimensions: PageDimension[];
}

interface AccentColors {
  accent: string;
  accentDim: string;
}

function App() {
  const addTab = usePdfStore((s) => s.addTab);
  const openFileRef = useRef<() => Promise<void>>();
  const printRef = useRef<() => Promise<void>>();
  const [printProgress, setPrintProgress] = useState<{ page: number; total: number } | null>(null);

  // Open a PDF by path in a new tab. Used by the file picker, the startup
  // file-association path, and "open-file" events from a second instance.
  const openDocumentByPath = async (path: string) => {
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
      await message(String(err), { title: "Failed to Open PDF", kind: "error" });
    }
  };

  // Shared file-open logic used by both Ctrl+O and toolbar button
  openFileRef.current = async () => {
    const selected = await open({
      multiple: false,
      filters: [{ name: "PDF Documents", extensions: ["pdf"] }],
    });
    if (!selected) return;

    const path = typeof selected === "string" ? selected : selected;
    await openDocumentByPath(path);
  };

  // Shared print logic
  printRef.current = async () => {
    const tab = usePdfStore.getState().getActiveTab();
    if (!tab) return;
    try {
      setPrintProgress({ page: 0, total: tab.pageCount });
      await invoke("print_document", { docId: tab.docId });
    } catch (err) {
      await message(String(err), { title: "Print Failed", kind: "error" });
    } finally {
      setPrintProgress(null);
    }
  };

  // Apply the Windows accent color to the theme on startup.
  useEffect(() => {
    invoke<AccentColors>("get_accent_color")
      .then(({ accent, accentDim }) => {
        const root = document.documentElement.style;
        root.setProperty("--color-accent-dynamic", accent);
        root.setProperty("--color-accent-dim-dynamic", accentDim);
        root.setProperty("--color-on-accent-dynamic", contrastTextColor(accent));
      })
      .catch((err) => console.error("Failed to read accent color:", err));
  }, []);

  // On startup, open a PDF passed via file association (Explorer double-click
  // or "Open with"), and listen for the same from a second app instance.
  useEffect(() => {
    invoke<string | null>("take_startup_file").then((path) => {
      if (path) openDocumentByPath(path);
    });

    const unlisten = listen<string>("open-file", (event) => {
      openDocumentByPath(event.payload);
    });
    return () => { unlisten.then((f) => f()); };
  }, []);

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

  // Reconstruct copied text with line breaks and inter-item spacing.
  // Text-layer spans are absolutely positioned, so the browser's default
  // plain-text serialization concatenates lines without inserting "\n" or
  // preserving gaps between separate runs on the same line (e.g. a list
  // number and its item text). Each span carries data-line/data-x/
  // data-right/data-font-size (set by TextLayer); insert "\n" when the
  // line changes, and a tab between same-line items separated by a real
  // gap (e.g. a list number and its item text). The Rust extractor only
  // splits same-line runs at gaps >= 0.5 * fontSize, so any gap above this
  // threshold is a deliberate spatial gap, not adjacent run fragments —
  // use one threshold/character so gaps of varying width (e.g. "1." vs
  // "10.") still align to the same tab stop.
  useEffect(() => {
    const GAP_THRESHOLD = 0.2;

    const handleCopy = (e: ClipboardEvent) => {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) return;

      const fragment = selection.getRangeAt(0).cloneContents();
      if (!fragment.querySelector("[data-line]")) return;

      let result = "";
      let lastLine: string | null = null;
      let prevRight: number | null = null;

      const walk = (node: Node) => {
        if (node.nodeType === Node.TEXT_NODE) {
          result += node.textContent ?? "";
          return;
        }
        if (node.nodeType === Node.ELEMENT_NODE) {
          const el = node as Element;
          const line = el.getAttribute("data-line");
          if (line !== null) {
            const x = parseFloat(el.getAttribute("data-x") ?? "0");
            const right = parseFloat(el.getAttribute("data-right") ?? "0");
            const fontSize = parseFloat(el.getAttribute("data-font-size") ?? "0");

            if (lastLine !== null && line !== lastLine) {
              result += "\n";
            } else if (lastLine !== null && prevRight !== null && fontSize > 0) {
              const gap = x - prevRight;
              if (gap > fontSize * GAP_THRESHOLD) {
                result += "\t";
              }
            }

            lastLine = line;
            prevRight = right;
          }
          el.childNodes.forEach(walk);
        }
      };
      fragment.childNodes.forEach(walk);

      e.preventDefault();
      e.clipboardData?.setData("text/plain", result);
    };

    document.addEventListener("copy", handleCopy);
    return () => document.removeEventListener("copy", handleCopy);
  }, []);

  return (
    <div className="app-shell">
      <Toolbar onOpenFile={() => openFileRef.current?.()} onPrint={() => printRef.current?.()} />
      <TabBar onOpenFile={() => openFileRef.current?.()} />
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

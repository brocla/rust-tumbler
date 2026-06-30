import { useEffect, useRef, useState } from "react";
import { message, open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Toolbar } from "./components/Toolbar";
import { TabBar } from "./components/TabBar";
import { IconRail } from "./components/IconRail";
import { Sidebar } from "./components/Sidebar";
import { ViewerArea } from "./components/ViewerArea";
import { StatusBar } from "./components/StatusBar";
import { usePdfStore, suppressedReloadDocs } from "./store/usePdfStore";
import type { PageDimension, CompressProgress } from "./store/usePdfStore";
import type { SignatureInfo } from "./utils/signature";
import { contrastTextColor } from "./utils/color";
import { reconstructCopyText, type CopyToken } from "./utils/textSelection";
import { evictDoc, evictPages } from "./utils/renderCache";

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
  const updateTab = usePdfStore((s) => s.updateTab);
  const ocrProgress = usePdfStore((s) => s.ocrProgress);
  const setOcrProgress = usePdfStore((s) => s.setOcrProgress);
  const compressProgress = usePdfStore((s) => s.compressProgress);
  const setCompressProgress = usePdfStore((s) => s.setCompressProgress);
  const openFileRef = useRef<() => Promise<void>>();
  const printRef = useRef<() => Promise<void>>();
  const [printProgress, setPrintProgress] = useState<{ page: number; total: number } | null>(null);

  // Verify the document's digital signatures and store the status for the
  // bottom badge + edit guards. Best-effort: a failure just leaves the status
  // unset (no badge), never blocks anything.
  const refreshSignatureStatus = (docId: string, tabId: string) => {
    invoke<SignatureInfo>("get_signature_info", { docId })
      .then((sig) => updateTab(tabId, { signatureStatus: sig.status }))
      .catch(() => {});
  };

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
        filePath: path,
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
        pagesVersion: 0,
        contentEpoch: 0,
        sidebarScrollPage: 1,
        ocrEpoch: 0,
      });
      refreshSignatureStatus(info.docId, tabId);
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
      const result = await invoke<{ printed: boolean; pagesPrinted: number; cancelled: boolean }>(
        "print_document", { docId: tab.docId }
      );
      if (result.cancelled) {
        await message("Print job cancelled.", { title: "Cancelled", kind: "info" });
      }
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

  // Listen for document-wide OCR progress (Make Searchable / Export Text)
  useEffect(() => {
    const unlisten = listen<{ page: number; total: number }>("ocr-progress", (event) => {
      setOcrProgress(event.payload);
    });
    return () => { unlisten.then((f) => f()); };
  }, []);

  // Listen for compression progress (Compress panel "Run")
  useEffect(() => {
    const unlisten = listen<CompressProgress>("compress-progress", (event) => {
      setCompressProgress(event.payload);
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

  // Sync page count and dimensions after any page operation (delete, rotate, reorder, merge, split).
  // The backend emits this event for all tabs sharing the same file, so a second open tab also updates.
  useEffect(() => {
    interface PagesChangedPayload {
      docIds: string[];
      pageCount: number;
      pageDimensions: PageDimension[];
    }
    const unlisten = listen<PagesChangedPayload>("document-pages-changed", (event) => {
      const { docIds, pageCount, pageDimensions } = event.payload;
      const { tabs } = usePdfStore.getState();
      for (const tab of tabs) {
        if (!docIds.includes(tab.docId)) continue;
        // Was this reload already applied optimistically by an in-place reorder?
        const optimistic = suppressedReloadDocs.delete(tab.docId);
        // Reconcile to the backend's authoritative pages. The main canvas won't
        // flash: a contentEpoch bump re-renders the slots without remounting, so
        // the old (correct) bitmap stays on screen until the fresh one is drawn
        // — no blank, no "Loading…".
        //
        // For an optimistic reorder we evict only the page cache (the thumbnail
        // cache keeps its relabeled bitmaps so the *next* reorder can still
        // repaint synchronously) and we do NOT bump pagesVersion — bumping it is
        // what remounts every slot and blinks the whole document. A destructive
        // op (delete/rotate/merge) evicts everything and bumps pagesVersion so
        // the document fully re-renders.
        if (optimistic) {
          evictPages(tab.docId);
          updateTab(tab.id, { pageCount, pageDimensions, contentEpoch: tab.contentEpoch + 1 });
        } else {
          evictDoc(tab.docId);
          updateTab(tab.id, {
            pageCount,
            pageDimensions,
            pagesVersion: tab.pagesVersion + 1,
            contentEpoch: tab.contentEpoch + 1,
          });
        }
        // A page operation rewrote the file, so any signature is now invalid —
        // re-verify so the badge reflects reality.
        refreshSignatureStatus(tab.docId, tab.id);
      }
    });
    return () => { unlisten.then((f) => f()); };
  }, [updateTab]);

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
  // data-right/data-font-size (set by TextLayer); walk the selection into a
  // flat list of tokens and let reconstructCopyText (utils/textSelection)
  // decide where "\n" and "\t" belong.
  useEffect(() => {
    const handleCopy = (e: ClipboardEvent) => {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) return;

      const fragment = selection.getRangeAt(0).cloneContents();
      if (!fragment.querySelector("[data-line]")) return;

      const tokens: CopyToken[] = [];

      const walk = (node: Node) => {
        if (node.nodeType === Node.TEXT_NODE) {
          tokens.push({ text: node.textContent ?? "", line: null, x: 0, right: 0, fontSize: 0 });
          return;
        }
        if (node.nodeType === Node.ELEMENT_NODE) {
          const el = node as Element;
          const line = el.getAttribute("data-line");
          if (line !== null) {
            tokens.push({
              text: "",
              line,
              x: parseFloat(el.getAttribute("data-x") ?? "0"),
              right: parseFloat(el.getAttribute("data-right") ?? "0"),
              fontSize: parseFloat(el.getAttribute("data-font-size") ?? "0"),
            });
          }
          el.childNodes.forEach(walk);
        }
      };
      fragment.childNodes.forEach(walk);

      e.preventDefault();
      e.clipboardData?.setData("text/plain", reconstructCopyText(tokens));
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
      <StatusBar />
      {printProgress && (
        <div className="print-progress-overlay">
          <div className="print-progress-dialog">
            <p>Printing page {printProgress.page} of {printProgress.total}...</p>
            <button onClick={() => void invoke("cancel_print")}>Cancel</button>
          </div>
        </div>
      )}
      {ocrProgress && (
        <div className="print-progress-overlay">
          <div className="print-progress-dialog">
            <p>
              {ocrProgress.page === 0
                ? "Preparing OCR..."
                : `OCR page ${ocrProgress.page} of ${ocrProgress.total}...`}
            </p>
            <button onClick={() => void invoke("cancel_ocr")}>Cancel</button>
          </div>
        </div>
      )}
      {compressProgress && (
        <div className="print-progress-overlay">
          <div className="print-progress-dialog">
            <p>{describeCompress(compressProgress)}</p>
            <button onClick={() => void invoke("cancel_compress")}>Cancel</button>
          </div>
        </div>
      )}
    </div>
  );
}

const COMPRESS_STEP_LABELS: Record<string, string> = {
  recompress_streams: "Recompressing streams",
  prune_unused: "Pruning unused objects",
  delete_zero_length: "Deleting empty streams",
  strip_extras: "Stripping extras",
  recompress_images: "Downsampling images",
};

function describeCompress(p: CompressProgress): string {
  if (p.step === "recompress_images" && p.imageTotal > 0) {
    return `Compressing — image ${p.image} of ${p.imageTotal}...`;
  }
  const label = COMPRESS_STEP_LABELS[p.step] ?? "Compressing";
  return `${label} (step ${p.stepIndex} of ${p.stepCount})...`;
}

export default App;

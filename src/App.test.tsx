import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import App from "./App";
import { usePdfStore } from "./store/usePdfStore";
import type { TabState } from "./store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
  save: vi.fn(),
  message: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onCloseRequested: vi.fn(async () => () => {}),
  }),
}));

// The document-open flow lives in App itself; the child panels are irrelevant
// here and drag in their own Tauri calls, so stub them out.
vi.mock("./components/Toolbar", () => ({ Toolbar: () => null }));
vi.mock("./components/TabBar", () => ({ TabBar: () => null }));
vi.mock("./components/IconRail", () => ({ IconRail: () => null }));
vi.mock("./components/Sidebar", () => ({ Sidebar: () => null }));
vi.mock("./components/ViewerArea", () => ({ ViewerArea: () => null }));
vi.mock("./components/StatusBar", () => ({ StatusBar: () => null }));

const CANONICAL = "C:\\Docs\\report.pdf";

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "report.pdf",
    filePath: CANONICAL,
    pageCount: 3,
    pageDimensions: [{ width: 200, height: 200 }],
    currentPage: 1,
    scrollTop: 0,
    zoom: 100,
    zoomMode: "numeric",
    displayMode: "normal",
    searchQuery: "",
    searchResults: [],
    searchResultIndex: -1,
    metadataDirty: false,
    isDirty: false,
    loading: false,
    pagesVersion: 0,
    contentEpoch: 0,
    sidebarScrollPage: 1,
    ocrEpoch: 0,
    ...overrides,
  };
}

/** Renders App and returns a function that fires the "open-file" event. */
async function renderAppAndGetOpenFile(): Promise<(path: string) => Promise<void>> {
  let openFileHandler: ((event: { payload: string }) => void) | undefined;
  vi.mocked(listen).mockImplementation(async (event: string, handler: never) => {
    if (event === "open-file") openFileHandler = handler;
    return () => {};
  });

  await act(async () => {
    render(<App />);
    await new Promise((r) => setTimeout(r, 0));
  });

  return async (path: string) => {
    await act(async () => {
      openFileHandler!({ payload: path });
      await new Promise((r) => setTimeout(r, 0));
    });
  };
}

describe("App single-instance-per-file open guard", () => {
  beforeEach(() => {
    usePdfStore.setState({ tabs: [], activeTabId: null });
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
      switch (cmd) {
        case "canonicalize_path":
          // Simulate Windows normalization: any spelling of report.pdf
          // resolves to the one canonical form.
          return CANONICAL;
        case "open_document":
          return {
            docId: `doc-for-${(args as { path: string }).path}`,
            pageCount: 3,
            pageDimensions: [{ width: 200, height: 200 }],
          };
        case "take_startup_file":
          return null;
        case "get_accent_color":
          return { accent: "#0078d4", accentDim: "#004578" };
        default:
          throw new Error(`unmocked invoke: ${cmd}`);
      }
    });
  });

  it("opens a not-yet-open file in a new tab with the canonical path", async () => {
    const openFile = await renderAppAndGetOpenFile();

    await openFile("c:\\docs\\..\\docs\\REPORT.pdf");

    const { tabs, activeTabId } = usePdfStore.getState();
    expect(tabs).toHaveLength(1);
    expect(tabs[0].filePath).toBe(CANONICAL);
    expect(tabs[0].fileName).toBe("report.pdf");
    expect(activeTabId).toBe(tabs[0].id);
    expect(invoke).toHaveBeenCalledWith("open_document", { path: CANONICAL });
  });

  it("focuses the existing tab instead of opening a duplicate", async () => {
    const openFile = await renderAppAndGetOpenFile();
    usePdfStore.setState({
      tabs: [makeTab({ id: "tab-1" }), makeTab({ id: "tab-2", docId: "doc-2", filePath: "C:\\Docs\\other.pdf" })],
      activeTabId: "tab-2",
    });

    // A differently spelled path to the file already open in tab-1.
    await openFile("C:\\DOCS\\report.PDF");

    const { tabs, activeTabId } = usePdfStore.getState();
    expect(tabs).toHaveLength(2);
    expect(activeTabId).toBe("tab-1");
    expect(invoke).not.toHaveBeenCalledWith("open_document", expect.anything());
  });

  it("mirrors document-dirty-changed events into the tab's isDirty flag", async () => {
    let dirtyHandler:
      | ((event: { payload: { docId: string; dirty: boolean } }) => void)
      | undefined;
    vi.mocked(listen).mockImplementation(async (event: string, handler: never) => {
      if (event === "document-dirty-changed") dirtyHandler = handler;
      return () => {};
    });

    await act(async () => {
      render(<App />);
      await new Promise((r) => setTimeout(r, 0));
    });
    usePdfStore.setState({ tabs: [makeTab()], activeTabId: "tab-1" });

    await act(async () => {
      dirtyHandler!({ payload: { docId: "doc-1", dirty: true } });
    });
    expect(usePdfStore.getState().tabs[0].isDirty).toBe(true);

    await act(async () => {
      dirtyHandler!({ payload: { docId: "doc-1", dirty: false } });
    });
    expect(usePdfStore.getState().tabs[0].isDirty).toBe(false);
  });

  it("falls back to the raw path when canonicalization fails", async () => {
    const openFile = await renderAppAndGetOpenFile();
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "canonicalize_path") throw new Error("not found");
      if (cmd === "open_document") {
        return { docId: "doc-raw", pageCount: 1, pageDimensions: [{ width: 100, height: 100 }] };
      }
      return null;
    });

    await openFile("C:\\Docs\\new.pdf");

    const { tabs } = usePdfStore.getState();
    expect(tabs).toHaveLength(1);
    expect(tabs[0].filePath).toBe("C:\\Docs\\new.pdf");
    expect(invoke).toHaveBeenCalledWith("open_document", { path: "C:\\Docs\\new.pdf" });
  });
});

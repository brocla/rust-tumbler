import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { save, message, ask } from "@tauri-apps/plugin-dialog";
import { Toolbar } from "./Toolbar";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(),
  message: vi.fn(),
  ask: vi.fn(),
}));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "test.pdf",
    filePath: "C:\\Users\\test\\test.pdf",
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
    loading: false,
    pagesVersion: 0,
    contentEpoch: 0,
    sidebarScrollPage: 1,
    ...overrides,
  };
}

function renderToolbar() {
  usePdfStore.setState({
    tabs: [makeTab()],
    activeTabId: "tab-1",
    exportProgress: null,
  });
  return render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);
}

async function clickExport() {
  await act(async () => {
    fireEvent.click(screen.getByTitle("Export Text..."));
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe("Toolbar export text", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(ask).mockReset();
    vi.mocked(save).mockResolvedValue("C:\\out.txt");
    vi.mocked(message).mockResolvedValue(undefined as never);
  });

  it("exports without OCR and without prompting when every page has text", async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "count_pages_without_text") return Promise.resolve(0);
      if (cmd === "export_text")
        return Promise.resolve({ pages: 3, ocrPages: 0, cancelled: false });
      return Promise.resolve(undefined);
    });

    renderToolbar();
    await clickExport();

    expect(ask).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith("export_text", {
      docId: "doc-1",
      destPath: "C:\\out.txt",
      useOcr: false,
    });
    expect(message).toHaveBeenCalledWith(
      "Exported 3 pages.",
      expect.objectContaining({ title: "Export Complete" }),
    );
  });

  it("offers OCR when pages lack text and exports with OCR on accept", async () => {
    vi.mocked(ask).mockResolvedValue(true);
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "count_pages_without_text") return Promise.resolve(2);
      if (cmd === "export_text")
        return Promise.resolve({ pages: 3, ocrPages: 2, cancelled: false });
      return Promise.resolve(undefined);
    });

    renderToolbar();
    await clickExport();

    expect(ask).toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith("export_text", {
      docId: "doc-1",
      destPath: "C:\\out.txt",
      useOcr: true,
    });
    expect(message).toHaveBeenCalledWith(
      "Exported 3 pages (2 via OCR).",
      expect.objectContaining({ title: "Export Complete" }),
    );
  });

  it("exports without OCR when the user declines the OCR prompt", async () => {
    vi.mocked(ask).mockResolvedValue(false);
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "count_pages_without_text") return Promise.resolve(2);
      if (cmd === "export_text")
        return Promise.resolve({ pages: 3, ocrPages: 0, cancelled: false });
      return Promise.resolve(undefined);
    });

    renderToolbar();
    await clickExport();

    expect(ask).toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith("export_text", {
      docId: "doc-1",
      destPath: "C:\\out.txt",
      useOcr: false,
    });
  });
});

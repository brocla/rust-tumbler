import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { save, message, ask, confirm } from "@tauri-apps/plugin-dialog";
import { Toolbar } from "./Toolbar";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(),
  message: vi.fn(),
  ask: vi.fn(),
  confirm: vi.fn(),
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
    ocrEpoch: 0,
    ...overrides,
  };
}

function renderToolbar() {
  usePdfStore.setState({
    tabs: [makeTab()],
    activeTabId: "tab-1",
    ocrProgress: null,
  });
  return render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);
}

async function clickExport() {
  await act(async () => {
    fireEvent.click(screen.getByTitle("Export Text..."));
    await new Promise((r) => setTimeout(r, 0));
  });
}

async function clickMakeSearchable() {
  await act(async () => {
    fireEvent.click(screen.getByTitle("OCR - Make Text Searchable"));
    await new Promise((r) => setTimeout(r, 0));
  });
}

async function clickSaveSearchable() {
  await act(async () => {
    fireEvent.click(screen.getByTitle("Save Searchable Copy..."));
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

describe("Toolbar make searchable", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(message).mockResolvedValue(undefined as never);
  });

  it("OCRs the document and bumps ocrEpoch when pages lack text", async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "count_pages_without_text") return Promise.resolve(2);
      if (cmd === "ocr_document")
        return Promise.resolve({ pagesOcred: 2, cancelled: false });
      return Promise.resolve(undefined);
    });

    renderToolbar();
    await clickMakeSearchable();

    expect(invoke).toHaveBeenCalledWith("ocr_document", { docId: "doc-1" });
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Made 2 pages searchable"),
      expect.objectContaining({ title: "Make Searchable" }),
    );
    // Text overlay refresh signal bumped.
    expect(usePdfStore.getState().tabs[0].ocrEpoch).toBe(1);
  });

  it("does nothing but inform when every page already has text", async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "count_pages_without_text") return Promise.resolve(0);
      return Promise.resolve(undefined);
    });

    renderToolbar();
    await clickMakeSearchable();

    expect(invoke).not.toHaveBeenCalledWith("ocr_document", expect.anything());
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("already has a text layer"),
      expect.objectContaining({ title: "Make Searchable" }),
    );
    expect(usePdfStore.getState().tabs[0].ocrEpoch).toBe(0);
  });
});

describe("Toolbar save searchable copy", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(confirm).mockReset();
    vi.mocked(message).mockResolvedValue(undefined as never);
  });

  it("saves a copy to the chosen path and reports pages OCR'd", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out (searchable).pdf");
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 2,
      pagesSkippedUnsupportedGeometry: 0,
      cancelled: false,
    });

    renderToolbar();
    await clickSaveSearchable();

    expect(confirm).not.toHaveBeenCalled(); // unsigned document → no warning
    expect(invoke).toHaveBeenCalledWith("save_searchable_copy", {
      docId: "doc-1",
      destPath: "C:\\out (searchable).pdf",
    });
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Saved a searchable copy (2 pages OCR'd)."),
      expect.objectContaining({ title: "Save Searchable Copy" }),
    );
  });

  it("reports rotated/offset pages that were left un-searchable", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out (searchable).pdf");
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 2,
      pagesSkippedUnsupportedGeometry: 1,
      cancelled: false,
    });

    renderToolbar();
    await clickSaveSearchable();

    const [[text]] = vi.mocked(message).mock.calls;
    expect(text).toContain("2 pages OCR'd");
    expect(text).toContain("1 rotated or offset page couldn't be made searchable");
  });

  it("explains when every scanned page was skipped for geometry", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out (searchable).pdf");
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 0,
      pagesSkippedUnsupportedGeometry: 3,
      cancelled: false,
    });

    renderToolbar();
    await clickSaveSearchable();

    expect(message).toHaveBeenCalledWith(
      expect.stringContaining(
        "3 rotated or offset pages couldn't be made searchable",
      ),
      expect.objectContaining({ title: "Save Searchable Copy" }),
    );
  });

  it("does not invoke the backend when the user cancels the save dialog", async () => {
    vi.mocked(save).mockResolvedValue(null);

    renderToolbar();
    await clickSaveSearchable();

    expect(invoke).not.toHaveBeenCalled();
  });

  it("warns before saving a signed document and aborts if declined", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out (searchable).pdf");
    vi.mocked(confirm).mockResolvedValue(false);
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "verified" })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);

    await clickSaveSearchable();

    expect(confirm).toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalledWith(
      "save_searchable_copy",
      expect.anything(),
    );
  });

  it("reports when no OCR layer was needed", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out (searchable).pdf");
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 0,
      pagesSkippedUnsupportedGeometry: 0,
      cancelled: false,
    });

    renderToolbar();
    await clickSaveSearchable();

    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("no OCR text layer was added"),
      expect.objectContaining({ title: "Save Searchable Copy" }),
    );
  });
});

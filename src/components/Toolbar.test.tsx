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
    isDirty: false,
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

async function clickAddTextLayer() {
  await act(async () => {
    fireEvent.click(screen.getByTitle("Add Text Layer (make searchable in any reader)"));
    await new Promise((r) => setTimeout(r, 0));
  });
}

async function clickSaveLinearized() {
  await act(async () => {
    fireEvent.click(screen.getByTitle("Save Web-Optimized Copy..."));
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe("Toolbar save / save as (issue #31)", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(message).mockResolvedValue(undefined as never);
  });

  it("disables Save while the document is clean", () => {
    renderToolbar();
    expect(screen.getByTitle("Save (Ctrl+S)")).toBeDisabled();
  });

  it("enables Save when dirty and invokes save_document", async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);
    usePdfStore.setState({
      tabs: [makeTab({ isDirty: true })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);

    const saveButton = screen.getByTitle("Save (Ctrl+S)");
    expect(saveButton).toBeEnabled();
    await act(async () => {
      fireEvent.click(saveButton);
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(invoke).toHaveBeenCalledWith("save_document", { docId: "doc-1" });
  });

  it("Save As prompts for a destination, invokes save_document_as, and retargets the tab", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\renamed.pdf");
    vi.mocked(invoke).mockResolvedValue("C:\\Users\\test\\renamed.pdf");

    renderToolbar();
    await act(async () => {
      fireEvent.click(screen.getByTitle("Save As... (Ctrl+Shift+S)"));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({ defaultPath: "C:\\Users\\test\\test.pdf" }),
    );
    expect(invoke).toHaveBeenCalledWith("save_document_as", {
      docId: "doc-1",
      destPath: "C:\\Users\\test\\renamed.pdf",
    });
    const tab = usePdfStore.getState().tabs[0];
    expect(tab.filePath).toBe("C:\\Users\\test\\renamed.pdf");
    expect(tab.fileName).toBe("renamed.pdf");
  });

  it("Save As does nothing when the dialog is cancelled", async () => {
    vi.mocked(save).mockResolvedValue(null);

    renderToolbar();
    // The toolbar checks document_has_form on mount; ignore that and assert on
    // what the cancelled Save As does (or rather, doesn't do).
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    vi.mocked(invoke).mockClear();

    await act(async () => {
      fireEvent.click(screen.getByTitle("Save As... (Ctrl+Shift+S)"));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(invoke).not.toHaveBeenCalled();
  });

  it("reports a failed save and leaves the document dirty", async () => {
    vi.mocked(invoke).mockRejectedValue("disk full");
    usePdfStore.setState({
      tabs: [makeTab({ isDirty: true })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);

    await act(async () => {
      fireEvent.click(screen.getByTitle("Save (Ctrl+S)"));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(message).toHaveBeenCalledWith(
      "disk full",
      expect.objectContaining({ title: "Save Failed" }),
    );
    expect(usePdfStore.getState().tabs[0].isDirty).toBe(true);
  });
});

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

describe("Toolbar add text layer", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(confirm).mockReset();
    vi.mocked(message).mockResolvedValue(undefined as never);
  });

  it("adds the layer as a buffer edit — no save dialog — and points at Save", async () => {
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 2,
      pagesSkippedUnsupportedGeometry: 0,
      cancelled: false,
    });

    renderToolbar();
    await clickAddTextLayer();

    expect(confirm).not.toHaveBeenCalled(); // unsigned document → no warning
    expect(save).not.toHaveBeenCalled(); // deferred edit: no dialog
    expect(invoke).toHaveBeenCalledWith("add_text_layer", { docId: "doc-1" });
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Added a text layer to 2 pages. Use Save or Save As to keep it."),
      expect.objectContaining({ title: "Add Text Layer" }),
    );
    // The buffer changed, so the text overlay refreshes.
    expect(usePdfStore.getState().tabs[0].ocrEpoch).toBe(1);
    // ...and the signature badge is re-verified against the edited buffer.
    expect(invoke).toHaveBeenCalledWith("get_signature_info", { docId: "doc-1" });
  });

  it("reports rotated/offset pages that were left un-searchable", async () => {
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 2,
      pagesSkippedUnsupportedGeometry: 1,
      cancelled: false,
    });

    renderToolbar();
    await clickAddTextLayer();

    const [[text]] = vi.mocked(message).mock.calls;
    expect(text).toContain("Added a text layer to 2 pages");
    expect(text).toContain("1 rotated or offset page couldn't be made searchable");
  });

  it("explains when every scanned page was skipped for geometry", async () => {
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 0,
      pagesSkippedUnsupportedGeometry: 3,
      cancelled: false,
    });

    renderToolbar();
    await clickAddTextLayer();

    expect(message).toHaveBeenCalledWith(
      expect.stringContaining(
        "3 rotated or offset pages couldn't be made searchable",
      ),
      expect.objectContaining({ title: "Add Text Layer" }),
    );
  });

  it("warns before editing a signed document and aborts if declined", async () => {
    vi.mocked(confirm).mockResolvedValue(false);
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "verified" })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);

    await clickAddTextLayer();

    expect(confirm).toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalledWith(
      "add_text_layer",
      expect.anything(),
    );
  });

  it("reports when no page needed a layer", async () => {
    vi.mocked(invoke).mockResolvedValue({
      pagesWritten: 0,
      pagesSkippedUnsupportedGeometry: 0,
      cancelled: false,
    });

    renderToolbar();
    await clickAddTextLayer();

    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Every page already has a text layer"),
      expect.objectContaining({ title: "Add Text Layer" }),
    );
    // No edit happened, so there's nothing to re-verify.
    expect(invoke).not.toHaveBeenCalledWith("get_signature_info", expect.anything());
  });
});

describe("Toolbar Save Web-Optimized Copy (issue #3)", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(message).mockResolvedValue(undefined as never);
    usePdfStore.setState({ linearizeProgress: false });
  });

  it("prompts for a destination with a -web suggested name and exports", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\test-web.pdf");
    vi.mocked(invoke).mockResolvedValue({ originalSize: 1000, linearizedSize: 1100 });

    renderToolbar();
    await clickSaveLinearized();

    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({ defaultPath: "C:\\Users\\test/test-web.pdf" }),
    );
    expect(invoke).toHaveBeenCalledWith("export_linearized_copy", {
      docId: "doc-1",
      destPath: "C:\\Users\\test\\test-web.pdf",
    });
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Saved web-optimized copy (1.1 KB, was 1000 B)."),
      expect.objectContaining({ title: "Save Web-Optimized Copy" }),
    );
  });

  it("does nothing when the save dialog is cancelled", async () => {
    vi.mocked(save).mockResolvedValue(null);

    renderToolbar();
    await clickSaveLinearized();

    expect(invoke).not.toHaveBeenCalledWith(
      "export_linearized_copy",
      expect.anything(),
    );
  });

  it("notes the copy is unencrypted for a password-protected document", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\test-web.pdf");
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "export_linearized_copy")
        return Promise.resolve({ originalSize: 1000, linearizedSize: 1000 });
      return Promise.resolve(false);
    });
    usePdfStore.setState({
      tabs: [makeTab({ encrypted: true })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);

    await clickSaveLinearized();

    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("The copy is unencrypted"),
      expect.objectContaining({ title: "Save Web-Optimized Copy" }),
    );
  });

  it("reports a failed export", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\test-web.pdf");
    vi.mocked(invoke).mockRejectedValue("qpdf.dll failed to load");

    renderToolbar();
    await clickSaveLinearized();

    expect(message).toHaveBeenCalledWith(
      "qpdf.dll failed to load",
      expect.objectContaining({ title: "Save Web-Optimized Copy", kind: "error" }),
    );
  });

  it("sets linearizeProgress while the export is in flight and clears it after", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\test-web.pdf");
    let resolveInvoke!: (v: unknown) => void;
    vi.mocked(invoke).mockImplementation(
      () => new Promise((resolve) => (resolveInvoke = resolve)),
    );

    renderToolbar();
    await act(async () => {
      fireEvent.click(screen.getByTitle("Save Web-Optimized Copy..."));
      // Let the save() dialog promise and the invoke() call kick off.
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(usePdfStore.getState().linearizeProgress).toBe(true);

    await act(async () => {
      resolveInvoke({ originalSize: 500, linearizedSize: 500 });
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(usePdfStore.getState().linearizeProgress).toBe(false);
  });
});

describe("Toolbar for encrypted PDFs (issue #57 — fully editable)", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    // document_has_form is polled on mount; keep it quiet.
    vi.mocked(invoke).mockResolvedValue(false);
  });

  function renderEncrypted() {
    usePdfStore.setState({
      tabs: [makeTab({ encrypted: true, isDirty: true })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    return render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);
  }

  it("keeps Save, Save As, and Add Text Layer enabled on an encrypted document", () => {
    renderEncrypted();
    expect(screen.getByTitle("Save (Ctrl+S)")).toBeEnabled();
    expect(screen.getByTitle("Save As... (Ctrl+Shift+S)")).toBeEnabled();
    expect(
      screen.getByTitle("Add Text Layer (make searchable in any reader)"),
    ).toBeEnabled();
  });

  it("shows the Remove-password button only for an encrypted document", () => {
    const unencrypted = renderToolbar();
    expect(screen.queryByTitle(/Remove password protection/)).toBeNull();
    unencrypted.unmount();

    renderEncrypted();
    expect(screen.getByTitle(/Remove password protection/)).toBeEnabled();
  });

  it("Remove password invokes the command and clears the tab's encrypted flag", async () => {
    renderEncrypted();
    await act(async () => {
      fireEvent.click(screen.getByTitle(/Remove password protection/));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(invoke).toHaveBeenCalledWith("remove_password", { docId: "doc-1" });
    expect(usePdfStore.getState().tabs[0].encrypted).toBe(false);
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Password protection removed"),
      expect.objectContaining({ title: "Remove Password" }),
    );
  });

  it("surfaces a backend failure without clearing the encrypted flag", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "remove_password") throw "boom";
      return false;
    });
    renderEncrypted();
    await act(async () => {
      fireEvent.click(screen.getByTitle(/Remove password protection/));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(usePdfStore.getState().tabs[0].encrypted).toBe(true);
    expect(message).toHaveBeenCalledWith(
      "boom",
      expect.objectContaining({ title: "Remove Password", kind: "error" }),
    );
  });
});

describe("Toolbar set / change password (issue #58)", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(confirm).mockReset();
    vi.mocked(message).mockResolvedValue(undefined as never);
    // document_has_form is polled on mount; keep it quiet.
    vi.mocked(invoke).mockResolvedValue(false);
  });

  async function openDialogAndSubmit(password: string) {
    await act(async () => {
      fireEvent.click(screen.getByTitle(/Set a password|Change the password/));
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: password },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: password },
    });
    await act(async () => {
      fireEvent.click(screen.getByText(/Set Password|Change Password/));
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  it("labels the button Set on an unencrypted document and Change on an encrypted one", () => {
    const plain = renderToolbar();
    expect(screen.getByTitle(/Set a password/)).toBeEnabled();
    expect(screen.queryByTitle(/Change the password/)).toBeNull();
    plain.unmount();

    usePdfStore.setState({
      tabs: [makeTab({ encrypted: true })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);
    expect(screen.getByTitle(/Change the password/)).toBeEnabled();
    expect(screen.queryByTitle(/Set a password/)).toBeNull();
  });

  it("sets a password: invokes the command, flags the tab encrypted, and notifies", async () => {
    renderToolbar();
    await openDialogAndSubmit("secret-58");

    expect(invoke).toHaveBeenCalledWith("set_password", {
      docId: "doc-1",
      password: "secret-58",
    });
    expect(usePdfStore.getState().tabs[0].encrypted).toBe(true);
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Password set"),
      expect.objectContaining({ title: "Set Password" }),
    );
  });

  it("changes the password of an already-encrypted document", async () => {
    usePdfStore.setState({
      tabs: [makeTab({ encrypted: true })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);
    await openDialogAndSubmit("new-pw");

    expect(invoke).toHaveBeenCalledWith("set_password", {
      docId: "doc-1",
      password: "new-pw",
    });
    expect(usePdfStore.getState().tabs[0].encrypted).toBe(true);
    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("Password changed"),
      expect.objectContaining({ title: "Change Password" }),
    );
  });

  it("cancelling the dialog invokes nothing", async () => {
    renderToolbar();
    await act(async () => {
      fireEvent.click(screen.getByTitle(/Set a password/));
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.click(screen.getByText("Cancel"));

    expect(invoke).not.toHaveBeenCalledWith("set_password", expect.anything());
    expect(usePdfStore.getState().tabs[0].encrypted).toBeFalsy();
  });

  it("surfaces a backend failure without flagging the tab encrypted", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "set_password") throw "boom";
      return false;
    });
    renderToolbar();
    await openDialogAndSubmit("secret");

    expect(usePdfStore.getState().tabs[0].encrypted).toBeFalsy();
    expect(message).toHaveBeenCalledWith(
      "boom",
      expect.objectContaining({ title: "Set Password", kind: "error" }),
    );
  });

  it("warns before protecting a signed document and aborts if declined", async () => {
    vi.mocked(confirm).mockResolvedValue(false);
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "verified" })],
      activeTabId: "tab-1",
      ocrProgress: null,
    });
    render(<Toolbar onOpenFile={vi.fn()} onPrint={vi.fn()} />);

    await act(async () => {
      fireEvent.click(screen.getByTitle(/Set a password/));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(confirm).toHaveBeenCalled();
    // Declined: no dialog opened, nothing invoked.
    expect(screen.queryByLabelText("Password")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith("set_password", expect.anything());
  });
});

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { save, confirm, message } from "@tauri-apps/plugin-dialog";
import { OptimizePanel } from "./OptimizePanel";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(),
  message: vi.fn(),
  confirm: vi.fn(),
}));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "report.pdf",
    filePath: "C:\\Users\\test\\report.pdf",
    pageCount: 1,
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

const REPORT = {
  results: [
    { step: "recompress_streams", sizeBefore: 1000, sizeAfter: 800 },
    { step: "prune_unused", sizeBefore: 800, sizeAfter: 700 },
    { step: "delete_zero_length", sizeBefore: 700, sizeAfter: 700 },
    { step: "strip_extras", sizeBefore: 700, sizeAfter: 600 },
  ],
  skippedImages: [],
  imagesAtTarget: 0,
  cancelled: false,
};

// Modelled on the real flyer that motivated the inspector: two full-page CMYK
// scans sitting at exactly 72 DPI, saved at quality 94.
const FLYER_IMAGES = [
  {
    pages: [1],
    width: 1535,
    height: 2135,
    storedBytes: 1_640_024,
    filter: "DCTDecode",
    colorSpace: "CMYK (ICC)",
    dpi: 72,
    jpegQuality: 94,
  },
  {
    pages: [2],
    width: 1535,
    height: 2135,
    storedBytes: 1_270_905,
    filter: "DCTDecode",
    colorSpace: "CMYK (ICC)",
    dpi: 72,
    jpegQuality: 94,
  },
];

describe("OptimizePanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(confirm).mockReset();
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") return REPORT;
      if (cmd === "get_conformance") return { declared: [] };
      if (cmd === "save_document_as") return "C:\\out\\report-optimized.pdf";
      return undefined;
    });

    usePdfStore.setState({
      tabs: [makeTab()],
      activeTabId: "tab-1",
      activeSidebarTool: "optimize",
      sidebarWidth: 250,
    });
  });

  function imageCheckbox(): HTMLInputElement {
    return screen
      .getByText("Downsample images")
      .closest("label")!
      .querySelector("input")! as HTMLInputElement;
  }

  function dpiFieldset(): HTMLFieldSetElement {
    return screen.getByText(/Target DPI/).closest("fieldset") as HTMLFieldSetElement;
  }

  it("offers the image step unchecked by default with DPI/quality disabled", () => {
    render(<OptimizePanel />);
    const cb = imageCheckbox();
    expect(cb.disabled).toBe(false);
    expect(cb.checked).toBe(false);
    expect(dpiFieldset().disabled).toBe(true);
  });

  it("includes the image step and enables DPI/quality when checked", async () => {
    render(<OptimizePanel />);
    fireEvent.click(imageCheckbox());
    expect(imageCheckbox().checked).toBe(true);
    expect(dpiFieldset().disabled).toBe(false);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "run_optimization_steps");
    expect(call![1]).toMatchObject({
      steps: [
        "recompress_streams",
        "prune_unused",
        "delete_zero_length",
        "strip_extras",
        "recompress_images",
      ],
    });
  });

  it("renders a friendly skipped-images notice", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") {
        return { ...REPORT, skippedImages: [{ reason: "jpx", count: 2 }] };
      }
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText(/JPEG2000/)).toBeTruthy());
    expect(screen.getByText(/2 images/)).toBeTruthy();
  });

  // --- Image inspector ---------------------------------------------------

  it("does not inspect images until the image step is checked", async () => {
    render(<OptimizePanel />);
    await waitFor(() => expect(screen.getByText("Run")).toBeTruthy());
    expect(vi.mocked(invoke).mock.calls.some((c) => c[0] === "inspect_images")).toBe(false);
  });

  it("lists each image with its resolution and estimated quality", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "inspect_images") return FLYER_IMAGES;
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(imageCheckbox());
    });

    await waitFor(() => expect(screen.getByText(/2 images in this document/)).toBeTruthy());
    expect(screen.getAllByText(/1535×2135 · 72 DPI · ~q94/)).toHaveLength(2);
    expect(screen.getAllByText(/JPEG · CMYK \(ICC\)/)).toHaveLength(2);
    expect(screen.getByText("p1")).toBeTruthy();
    expect(screen.getByText("p2")).toBeTruthy();
  });

  // The whole point: say up front that a run will do nothing, rather than
  // letting the user discover it as an unexplained 0.00%.
  it("summarises what the current target DPI will and won't touch", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "inspect_images") return FLYER_IMAGES;
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(imageCheckbox());
    });

    await waitFor(() =>
      expect(screen.getByText(/At 150 DPI: 0 to downsample, 2 already small enough/)).toBeTruthy(),
    );

    // Dropping the target below the images' 72 DPI flips both into scope.
    fireEvent.change(screen.getByText(/Target DPI/).closest("div")!.querySelector("input")!, {
      target: { value: "50" },
    });
    expect(screen.getByText(/At 50 DPI: 2 to downsample, 0 already small enough/)).toBeTruthy();
  });

  it("reports an image that is never drawn as unmeasurable", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "inspect_images") {
        return [{ ...FLYER_IMAGES[0], pages: [], dpi: null, jpegQuality: null }];
      }
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(imageCheckbox());
    });

    await waitFor(() => expect(screen.getByText(/never drawn on a page/)).toBeTruthy());
    expect(screen.getByText(/1 unmeasurable/)).toBeTruthy();
    expect(screen.getByText("—")).toBeTruthy();
  });

  it("says so when the document has no images at all", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "inspect_images") return [];
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(imageCheckbox());
    });
    await waitFor(() => expect(screen.getByText(/No images in this document/)).toBeTruthy());
  });

  // Inspection is advisory — a backend failure must not block compressing.
  it("stays usable when inspection fails", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "inspect_images") throw new Error("parse failed");
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(imageCheckbox());
    });
    await waitFor(() => expect(screen.getByText(/No images in this document/)).toBeTruthy());
    expect((screen.getByText("Run") as HTMLButtonElement).disabled).toBe(false);
  });

  // Without this notice an image-only PDF whose images are already sensibly
  // sized reports 0.00% with nothing to explain it.
  it("explains images left alone for already being at the target DPI", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") return { ...REPORT, imagesAtTarget: 2 };
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText(/already at or below 150 DPI/)).toBeTruthy());
    expect(screen.getByText(/Lower the target DPI/)).toBeTruthy();
  });

  // The DPI slider stays live after a run, so the notice must quote the DPI the
  // report was produced at, not whatever the slider reads now.
  it("quotes the DPI the report was run at, not the current slider value", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") return { ...REPORT, imagesAtTarget: 1 };
      return undefined;
    });
    render(<OptimizePanel />);
    fireEvent.click(imageCheckbox());
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText(/already at or below 150 DPI/)).toBeTruthy());

    fireEvent.change(screen.getByText(/Target DPI/).closest("div")!.querySelector("input")!, {
      target: { value: "80" },
    });
    expect(screen.getByText(/already at or below 150 DPI/)).toBeTruthy();
  });

  it("shows no results when the run reports cancellation", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") return { ...REPORT, cancelled: true };
      return undefined;
    });
    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    expect(screen.queryByText(/Total:/)).toBeNull();
    expect(screen.queryByText("Save As…")).toBeNull();
  });

  it("runs the four checked steps in declared order and shows results", async () => {
    render(<OptimizePanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });

    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "run_optimization_steps");
    expect(call).toBeTruthy();
    expect(call![1]).toMatchObject({
      docId: "doc-1",
      steps: ["recompress_streams", "prune_unused", "delete_zero_length", "strip_extras"],
      targetDpi: 150,
      jpegQuality: 80,
    });

    // Results table + cumulative total (1000 -> 600 = 40%).
    await waitFor(() => expect(screen.getByText(/Total:/)).toBeTruthy());
    expect(screen.getByText(/40\.0%/)).toBeTruthy();
  });

  it("excludes an unchecked step from the run", async () => {
    render(<OptimizePanel />);

    // Uncheck "Prune unused objects".
    const pruneCheckbox = screen
      .getByText("Prune unused objects")
      .closest("label")!
      .querySelector("input")! as HTMLInputElement;
    fireEvent.click(pruneCheckbox);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });

    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "run_optimization_steps");
    expect(call![1]).toMatchObject({
      steps: ["recompress_streams", "delete_zero_length", "strip_extras"],
    });
  });

  it("saves via the ordinary Save As flow with a suggested name", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out\\report-optimized.pdf");
    render(<OptimizePanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText("Save As…")).toBeTruthy());

    await act(async () => {
      fireEvent.click(screen.getByText("Save As…"));
    });

    expect(vi.mocked(save).mock.calls[0][0]).toMatchObject({
      defaultPath: "report-compressed.pdf",
    });
    const saveCall = vi.mocked(invoke).mock.calls.find((c) => c[0] === "save_document_as");
    expect(saveCall![1]).toMatchObject({
      docId: "doc-1",
      destPath: "C:\\out\\report-optimized.pdf",
    });
  });

  it("hides Save As and shows a confirmation after a successful save", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out\\report-optimized.pdf");
    render(<OptimizePanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText("Save As…")).toBeTruthy());

    await act(async () => {
      fireEvent.click(screen.getByText("Save As…"));
    });

    await waitFor(() => expect(screen.getByText("✓ Saved")).toBeTruthy());
    expect(screen.queryByText("Save As…")).toBeNull();
  });

  it("keeps the report and Save As button when the save dialog is cancelled", async () => {
    vi.mocked(save).mockResolvedValue(null); // user dismisses the dialog
    render(<OptimizePanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText("Save As…")).toBeTruthy());

    await act(async () => {
      fireEvent.click(screen.getByText("Save As…"));
    });

    // No save happened; the result (already applied to the buffer) can still
    // be saved later, so the button stays.
    expect(vi.mocked(invoke).mock.calls.find((c) => c[0] === "save_document_as")).toBeUndefined();
    expect(screen.getByText("Save As…")).toBeTruthy();
  });

  it("warns before compressing a file that declares PDF/A and aborts if declined", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") return REPORT;
      if (cmd === "get_conformance") return { declared: ["PDF/A-2b"] };
      return undefined;
    });
    vi.mocked(confirm).mockResolvedValue(false); // user cancels

    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });

    expect(confirm).toHaveBeenCalledTimes(1);
    // The warning names the declared standard; honest wording (no "compliant").
    expect(String(vi.mocked(confirm).mock.calls[0][0])).toMatch(/PDF\/A-2b/);
    // Declined -> no compression run.
    expect(
      vi.mocked(invoke).mock.calls.find((c) => c[0] === "run_optimization_steps"),
    ).toBeUndefined();
  });

  it("proceeds with compression when the conformance warning is overridden", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") return REPORT;
      if (cmd === "get_conformance") return { declared: ["PDF/X-4"] };
      return undefined;
    });
    vi.mocked(confirm).mockResolvedValue(true); // user continues

    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });

    expect(confirm).toHaveBeenCalledTimes(1);
    expect(
      vi.mocked(invoke).mock.calls.find((c) => c[0] === "run_optimization_steps"),
    ).toBeTruthy();
  });

  it("warns before compressing a signed document and aborts if declined", async () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "verified" })],
      activeTabId: "tab-1",
    });
    vi.mocked(confirm).mockResolvedValue(false); // user cancels

    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });

    expect(confirm).toHaveBeenCalledTimes(1);
    expect(String(vi.mocked(confirm).mock.calls[0][0])).toMatch(/signed/i);
    expect(
      vi.mocked(invoke).mock.calls.find((c) => c[0] === "run_optimization_steps"),
    ).toBeUndefined();
  });

  it("does not warn when the file declares no PDF/A or PDF/X conformance", async () => {
    render(<OptimizePanel />); // default mock: declared: []
    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    expect(confirm).not.toHaveBeenCalled();
    expect(
      vi.mocked(invoke).mock.calls.find((c) => c[0] === "run_optimization_steps"),
    ).toBeTruthy();
  });

  it("clears a previous file's results when the active document changes", async () => {
    const { rerender } = render(<OptimizePanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText(/Total:/)).toBeTruthy());

    // Open a different document in the same (still-mounted) panel.
    act(() => {
      usePdfStore.setState({
        tabs: [makeTab({ id: "tab-2", docId: "doc-2", fileName: "other.pdf" })],
        activeTabId: "tab-2",
      });
    });
    rerender(<OptimizePanel />);

    expect(screen.queryByText(/Total:/)).toBeNull();
    expect(screen.queryByText("Save As…")).toBeNull();
  });

  it("explains web-optimization as compress-then-linearize", () => {
    render(<OptimizePanel />);
    expect(screen.getByText(/Web-optimization prepares a PDF/)).toBeTruthy();
    expect(screen.getByText(/Linearizing must be the last step/)).toBeTruthy();
  });
});

describe("OptimizePanel Save Linearized Copy (issue #3)", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(message).mockReset();
    vi.mocked(message).mockResolvedValue(undefined as never);
    usePdfStore.setState({
      tabs: [makeTab()],
      activeTabId: "tab-1",
      activeSidebarTool: "optimize",
      sidebarWidth: 250,
      linearizeProgress: false,
    });
  });

  function clickSaveLinearized() {
    return act(async () => {
      fireEvent.click(screen.getByText("Save Linearized Copy…"));
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  it("is available independent of running Compress first", () => {
    render(<OptimizePanel />);
    expect(screen.getByText("Save Linearized Copy…")).toBeEnabled();
  });

  it("prompts for a destination with a -linearized suggested name and exports", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\report-linearized.pdf");
    vi.mocked(invoke).mockResolvedValue(undefined);

    render(<OptimizePanel />);
    await clickSaveLinearized();

    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({ defaultPath: "C:\\Users\\test/report-linearized.pdf" }),
    );
    expect(invoke).toHaveBeenCalledWith("export_linearized_copy", {
      docId: "doc-1",
      destPath: "C:\\Users\\test\\report-linearized.pdf",
    });
    expect(message).toHaveBeenCalledWith(
      "Saved linearized copy.",
      expect.objectContaining({ title: "Save Linearized Copy" }),
    );
  });

  it("does nothing when the save dialog is cancelled", async () => {
    vi.mocked(save).mockResolvedValue(null);

    render(<OptimizePanel />);
    await clickSaveLinearized();

    expect(invoke).not.toHaveBeenCalledWith("export_linearized_copy", expect.anything());
  });

  it("notes the copy is unencrypted for a password-protected document", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\report-linearized.pdf");
    vi.mocked(invoke).mockResolvedValue(undefined);
    usePdfStore.setState({
      tabs: [makeTab({ encrypted: true })],
      activeTabId: "tab-1",
      activeSidebarTool: "optimize",
      sidebarWidth: 250,
    });

    render(<OptimizePanel />);
    await clickSaveLinearized();

    expect(message).toHaveBeenCalledWith(
      expect.stringContaining("The copy is unencrypted"),
      expect.objectContaining({ title: "Save Linearized Copy" }),
    );
  });

  it("reports a failed export", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\report-linearized.pdf");
    vi.mocked(invoke).mockRejectedValue("qpdf.dll failed to load");

    render(<OptimizePanel />);
    await clickSaveLinearized();

    expect(message).toHaveBeenCalledWith(
      "qpdf.dll failed to load",
      expect.objectContaining({ title: "Save Linearized Copy", kind: "error" }),
    );
  });

  it("disables the button and shows a busy label while the export is in flight", async () => {
    vi.mocked(save).mockResolvedValue("C:\\Users\\test\\report-linearized.pdf");
    let resolveInvoke!: (v: unknown) => void;
    vi.mocked(invoke).mockImplementation(
      () => new Promise((resolve) => (resolveInvoke = resolve)),
    );

    render(<OptimizePanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Save Linearized Copy…"));
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(usePdfStore.getState().linearizeProgress).toBe(true);
    expect(screen.getByText("Saving…")).toBeDisabled();

    await act(async () => {
      resolveInvoke(undefined);
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(usePdfStore.getState().linearizeProgress).toBe(false);
    expect(screen.getByText("Save Linearized Copy…")).toBeTruthy();
  });
});

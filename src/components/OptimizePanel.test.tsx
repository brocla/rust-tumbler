import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { save, confirm } from "@tauri-apps/plugin-dialog";
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
  cancelled: false,
};

describe("OptimizePanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(confirm).mockReset();
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "run_optimization_steps") return REPORT;
      if (cmd === "get_conformance") return { declared: [] };
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

  it("saves the optimized copy to the chosen path with a suggested name", async () => {
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
    const saveCall = vi.mocked(invoke).mock.calls.find((c) => c[0] === "save_optimized_copy");
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

  it("Cancel discards the result and returns to the pre-run state", async () => {
    render(<OptimizePanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Run"));
    });
    await waitFor(() => expect(screen.getByText("Save As…")).toBeTruthy());

    fireEvent.click(screen.getByText("Cancel"));

    expect(screen.queryByText("Save As…")).toBeNull();
    expect(screen.queryByText(/Total:/)).toBeNull();
    expect(screen.getByText("Run")).toBeTruthy();
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
});

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { MarginsPanel, uniformScale, smallestMargins } from "./MarginsPanel";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  message: vi.fn(),
  confirm: vi.fn(),
}));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "medley.pdf",
    filePath: "C:\\Users\\test\\medley.pdf",
    pageCount: 1,
    pageDimensions: [{ width: 300, height: 400 }],
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

// One 300×400 page with a 200×200 ink box at (50,100): at the default 0.25″
// (18pt) target margin the uniform scale is min(264/200, 364/200) = 1.32.
const REPORT = {
  pages: [{ pageW: 300, pageH: 400, bbox: { x0: 50, y0: 100, x1: 250, y1: 300 } }],
  cancelled: false,
};

describe("uniformScale / smallestMargins", () => {
  it("computes the padding-limited scale and skips blank pages", () => {
    const pages = [
      ...REPORT.pages,
      { pageW: 300, pageH: 400, bbox: null },
    ];
    expect(uniformScale(pages, 18)).toBeCloseTo(1.32, 5);
    expect(uniformScale(pages, 0)).toBeCloseTo(1.5, 5);
    expect(uniformScale([{ pageW: 300, pageH: 400, bbox: null }], 18)).toBeNull();
  });

  it("reports the smallest margin per side across pages", () => {
    const pages = [
      { pageW: 300, pageH: 400, bbox: { x0: 50, y0: 100, x1: 250, y1: 300 } },
      { pageW: 300, pageH: 400, bbox: { x0: 30, y0: 120, x1: 240, y1: 390 } },
    ];
    expect(smallestMargins(pages)).toEqual({ left: 30, right: 50, top: 10, bottom: 100 });
    expect(smallestMargins([{ pageW: 300, pageH: 400, bbox: null }])).toBeNull();
  });
});

describe("MarginsPanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "analyze_margins") return REPORT;
      if (cmd === "expand_margins") return { scale: 1.32, cancelled: false };
      return undefined;
    });

    usePdfStore.setState({
      tabs: [makeTab()],
      activeTabId: "tab-1",
      activeSidebarTool: "margins",
      sidebarWidth: 250,
    });
  });

  it("analyzes on mount and shows margins and the achievable gain", async () => {
    render(<MarginsPanel />);
    await waitFor(() => expect(screen.getByText(/Smallest margins/)).toBeTruthy());

    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "analyze_margins");
    expect(call![1]).toMatchObject({ docId: "doc-1" });
    // L 50pt = 0.69″; T 100pt = 1.39″.
    expect(screen.getByText(/L 0\.69″/)).toBeTruthy();
    expect(screen.getByText(/T 1\.39″/)).toBeTruthy();
    expect(screen.getByText("+32%")).toBeTruthy();
  });

  it("updates the gain from the padding slider without re-analyzing", async () => {
    render(<MarginsPanel />);
    await waitFor(() => expect(screen.getByText("+32%")).toBeTruthy());

    fireEvent.change(screen.getByRole("slider"), { target: { value: "0" } });
    expect(screen.getByText("+50%")).toBeTruthy();

    const analyzeCalls = vi
      .mocked(invoke)
      .mock.calls.filter((c) => c[0] === "analyze_margins");
    expect(analyzeCalls).toHaveLength(1);
  });

  it("applies with the chosen padding and confirms the result", async () => {
    render(<MarginsPanel />);
    await waitFor(() => expect(screen.getByText("+32%")).toBeTruthy());

    await act(async () => {
      fireEvent.click(screen.getByText("Expand Content"));
    });

    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "expand_margins");
    expect(call![1]).toMatchObject({ docId: "doc-1", paddingPt: 18 });
    await waitFor(() => expect(screen.getByText(/Content enlarged 32%/)).toBeTruthy());
  });

  it("disables Apply when the content already fills the page", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "analyze_margins") {
        return {
          pages: [{ pageW: 300, pageH: 400, bbox: { x0: 18, y0: 18, x1: 282, y1: 382 } }],
          cancelled: false,
        };
      }
      return undefined;
    });
    render(<MarginsPanel />);
    await waitFor(() => expect(screen.getByText(/already fills the page/)).toBeTruthy());
    expect((screen.getByText("Expand Content").closest("button") as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  it("offers a retry after a cancelled analysis", async () => {
    vi.mocked(invoke).mockImplementationOnce(async () => ({ pages: [], cancelled: true }));
    render(<MarginsPanel />);
    await waitFor(() => expect(screen.getByText(/Analysis cancelled/)).toBeTruthy());

    await act(async () => {
      fireEvent.click(screen.getByText("Retry"));
    });
    await waitFor(() => expect(screen.getByText("+32%")).toBeTruthy());
  });
});

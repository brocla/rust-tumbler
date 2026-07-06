import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { RedactPanel } from "./RedactPanel";
import { usePdfStore } from "../store/usePdfStore";
import type { RedactRegion, TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(),
  message: vi.fn(),
}));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "discovery.pdf",
    filePath: "C:\\Users\\test\\discovery.pdf",
    pageCount: 3,
    pageDimensions: [
      { width: 200, height: 200 },
      { width: 200, height: 200 },
      { width: 200, height: 200 },
    ],
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

const MATCHES: RedactRegion[] = [
  { page: 1, rect: { x: 20, y: 30, width: 80, height: 12 } },
  { page: 3, rect: { x: 20, y: 30, width: 80, height: 12 } },
];

const VERIFIED_RESULT = {
  regions: 2,
  pagesFlattened: 2,
  verified: true,
  leaks: [],
  ocrCheckRan: true,
  reocrPages: 2,
  cancelled: false,
};

describe("RedactPanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(save).mockReset();
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "find_redaction_matches") return MATCHES;
      if (cmd === "apply_redactions") return VERIFIED_RESULT;
      if (cmd === "save_redacted_copy") return "C:\\out\\discovery-redacted.pdf";
      return undefined;
    });

    usePdfStore.setState({
      tabs: [makeTab()],
      activeTabId: "tab-1",
      activeSidebarTool: "redact",
      redactDrawMode: false,
      redactProgress: null,
      notice: null,
    });
  });

  function activeTab(): TabState {
    return usePdfStore.getState().tabs[0];
  }

  it("states that redacted pages become images and are re-OCR'd", () => {
    render(<RedactPanel />);
    expect(
      screen.getByText(/converted to\s+images and re-OCR/i),
    ).toBeTruthy();
    expect(screen.getByText(/original file is never modified/i)).toBeTruthy();
  });

  it("find & redact all marks every occurrence and remembers the query", async () => {
    render(<RedactPanel />);
    fireEvent.change(screen.getByPlaceholderText(/find text to redact/i), {
      target: { value: "SECRET" },
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Redact all"));
    });

    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "find_redaction_matches");
    expect(call![1]).toMatchObject({ docId: "doc-1", query: "SECRET" });
    expect(activeTab().redactRegions).toHaveLength(2);
    expect(activeTab().redactQueries).toEqual(["SECRET"]);
    expect(screen.getByText("2 occurrences marked.")).toBeTruthy();
    expect(screen.getByText("2 regions marked")).toBeTruthy();
  });

  it("Apply sends the regions, queries, and DPI, and enters the preview", async () => {
    usePdfStore.getState().addRedactRegions("doc-1", MATCHES);
    usePdfStore.getState().updateTab("tab-1", { redactQueries: ["SECRET"] });
    render(<RedactPanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Apply redactions"));
    });

    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "apply_redactions");
    expect(call![1]).toMatchObject({
      docId: "doc-1",
      regions: MATCHES,
      verifyQueries: ["SECRET"],
      targetDpi: 200,
    });
    expect(activeTab().redactPreview).toEqual({ verified: true });
    expect(screen.getByText(/✓ Verified — nothing recoverable/)).toBeTruthy();
  });

  it("Apply is disabled with no regions", () => {
    render(<RedactPanel />);
    expect((screen.getByText("Apply redactions") as HTMLButtonElement).disabled).toBe(true);
  });

  it("a failed verification shows the loud banner and blocks Save As", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "apply_redactions") {
        return {
          ...VERIFIED_RESULT,
          verified: false,
          leaks: [MATCHES[0]],
        };
      }
      return undefined;
    });
    usePdfStore.getState().addRedactRegions("doc-1", MATCHES);
    render(<RedactPanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Apply redactions"));
    });

    expect(screen.getByText(/Verification FAILED/)).toBeTruthy();
    expect(activeTab().redactPreview).toEqual({ verified: false });
    const saveButton = screen.getByText("Save As…") as HTMLButtonElement;
    expect(saveButton.disabled).toBe(true);
    // The dialog must never even open for an unverified result.
    fireEvent.click(saveButton);
    expect(save).not.toHaveBeenCalled();
  });

  it("Save As suggests <name>-redacted.pdf, saves, and clears the redaction state", async () => {
    vi.mocked(save).mockResolvedValue("C:\\out\\discovery-redacted.pdf");
    usePdfStore.getState().addRedactRegions("doc-1", MATCHES);
    render(<RedactPanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Apply redactions"));
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Save As…"));
    });

    expect(vi.mocked(save).mock.calls[0][0]).toMatchObject({
      defaultPath: "discovery-redacted.pdf",
    });
    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "save_redacted_copy");
    expect(call![1]).toMatchObject({
      docId: "doc-1",
      destPath: "C:\\out\\discovery-redacted.pdf",
    });
    expect(screen.getByText("✓ Redacted copy saved")).toBeTruthy();
    expect(activeTab().redactPreview).toBeNull();
    expect(activeTab().redactRegions).toEqual([]);
  });

  it("Discard drops the staging and exits the preview", async () => {
    usePdfStore.getState().addRedactRegions("doc-1", MATCHES);
    render(<RedactPanel />);
    await act(async () => {
      fireEvent.click(screen.getByText("Apply redactions"));
    });

    await act(async () => {
      fireEvent.click(screen.getByText("Discard"));
    });

    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "discard_redaction");
    expect(call![1]).toMatchObject({ docId: "doc-1" });
    expect(activeTab().redactPreview).toBeNull();
  });

  it("a cancelled run leaves the panel without a verdict or preview", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "apply_redactions") return { ...VERIFIED_RESULT, cancelled: true };
      return undefined;
    });
    usePdfStore.getState().addRedactRegions("doc-1", MATCHES);
    render(<RedactPanel />);

    await act(async () => {
      fireEvent.click(screen.getByText("Apply redactions"));
    });

    expect(screen.queryByText(/Verified|FAILED/)).toBeNull();
    expect(activeTab().redactPreview).toBeUndefined();
  });

  it("removing a region from the list updates the store", () => {
    usePdfStore.getState().addRedactRegions("doc-1", MATCHES);
    render(<RedactPanel />);

    fireEvent.click(screen.getAllByTitle("Remove this region")[0]);
    expect(activeTab().redactRegions).toHaveLength(1);
    expect(activeTab().redactRegions![0].page).toBe(3);

    fireEvent.click(screen.getByText("Clear all"));
    expect(activeTab().redactRegions).toEqual([]);
  });

  it("Draw region toggles the store's draw mode", () => {
    render(<RedactPanel />);
    fireEvent.click(screen.getByText("Draw region"));
    expect(usePdfStore.getState().redactDrawMode).toBe(true);
    fireEvent.click(screen.getByText(/Drawing — click to stop/));
    expect(usePdfStore.getState().redactDrawMode).toBe(false);
  });
});

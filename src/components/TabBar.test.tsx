import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { TabBar } from "./TabBar";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: vi.fn(),
  save: vi.fn(),
  message: vi.fn(),
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

function renderTabBar(tab: TabState) {
  usePdfStore.setState({
    tabs: [tab],
    activeTabId: tab.id,
    unsavedPrompt: null,
  });
  return render(<TabBar onOpenFile={vi.fn()} />);
}

/** Clicks the tab's ×, waits a tick for the prompt to be raised. */
async function clickClose() {
  await act(async () => {
    fireEvent.click(screen.getByTitle("Close"));
    await new Promise((r) => setTimeout(r, 0));
  });
}

/** Answers the pending unsaved-changes prompt and lets the close flow finish. */
async function answerPrompt(choice: "save" | "discard" | "cancel") {
  await act(async () => {
    usePdfStore.getState().resolveUnsaved(choice);
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe("TabBar close guard for unsaved edits (issue #31)", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  it("closes a clean tab without prompting", async () => {
    renderTabBar(makeTab());
    await clickClose();

    expect(usePdfStore.getState().unsavedPrompt).toBeNull();
    expect(invoke).toHaveBeenCalledWith("close_document", { docId: "doc-1" });
    expect(usePdfStore.getState().tabs).toHaveLength(0);
  });

  it("prompts for a dirty tab and keeps it open on Cancel", async () => {
    renderTabBar(makeTab({ isDirty: true }));
    await clickClose();

    expect(usePdfStore.getState().unsavedPrompt?.fileName).toBe("test.pdf");
    await answerPrompt("cancel");

    expect(invoke).not.toHaveBeenCalledWith("close_document", expect.anything());
    expect(usePdfStore.getState().tabs).toHaveLength(1);
  });

  it("discards and closes on Don't Save", async () => {
    renderTabBar(makeTab({ isDirty: true }));
    await clickClose();
    await answerPrompt("discard");

    expect(invoke).not.toHaveBeenCalledWith("save_document", expect.anything());
    expect(invoke).toHaveBeenCalledWith("close_document", { docId: "doc-1" });
    expect(usePdfStore.getState().tabs).toHaveLength(0);
  });

  it("saves then closes on Save", async () => {
    renderTabBar(makeTab({ isDirty: true }));
    await clickClose();
    await answerPrompt("save");

    expect(invoke).toHaveBeenCalledWith("save_document", { docId: "doc-1" });
    expect(invoke).toHaveBeenCalledWith("close_document", { docId: "doc-1" });
    expect(usePdfStore.getState().tabs).toHaveLength(0);
  });

  it("keeps the tab open when Save fails", async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) =>
      cmd === "save_document" ? Promise.reject("disk full") : Promise.resolve(undefined),
    );

    renderTabBar(makeTab({ isDirty: true }));
    await clickClose();
    await answerPrompt("save");

    expect(invoke).not.toHaveBeenCalledWith("close_document", expect.anything());
    expect(usePdfStore.getState().tabs).toHaveLength(1);
  });

  it("shows the dirty dot for a buffer-dirty tab", () => {
    const { container } = renderTabBar(makeTab({ isDirty: true }));
    expect(container.querySelector(".tab-dirty-dot")).not.toBeNull();
  });
});

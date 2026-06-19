import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { MetadataPanel } from "./MetadataPanel";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "test.pdf",
    filePath: "C:\\Users\\test\\test.pdf",
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
    loading: false,
    pagesVersion: 0,
    contentEpoch: 0,
    sidebarScrollPage: 1,
    ...overrides,
  };
}

function metadataFixture(title: string) {
  return {
    title,
    author: "",
    subject: "",
    keywords: "",
    creator: "",
    producer: "",
    creationDate: "",
    modDate: "",
  };
}

describe("MetadataPanel", () => {
  let metadataChangedHandler: ((event: { payload: string[] }) => void) | undefined;

  beforeEach(() => {
    metadataChangedHandler = undefined;

    vi.mocked(listen).mockImplementation((eventName, handler) => {
      if (eventName === "document-metadata-changed") {
        metadataChangedHandler = handler as (event: { payload: string[] }) => void;
      }
      return Promise.resolve(() => {});
    });

    vi.mocked(invoke).mockResolvedValue(metadataFixture("Original Title"));

    usePdfStore.setState({
      tabs: [makeTab()],
      activeTabId: "tab-1",
      activeSidebarTool: "metadata",
      sidebarWidth: 250,
    });
  });

  it("does not discard unsaved edits when another tab saves the same file", async () => {
    render(<MetadataPanel />);

    const titleInput = (await screen.findByLabelText("Title")) as HTMLInputElement;
    expect(titleInput.value).toBe("Original Title");

    // User starts editing but hasn't saved yet.
    fireEvent.change(titleInput, { target: { value: "Edited Title" } });
    expect(titleInput.value).toBe("Edited Title");
    expect(usePdfStore.getState().tabs[0].metadataDirty).toBe(true);

    // A different tab on the same underlying file saves its own metadata,
    // which fires this event for every tab pointing at that file.
    vi.mocked(invoke).mockResolvedValue(metadataFixture("Title From Other Tab"));
    const callsBeforeEvent = vi.mocked(invoke).mock.calls.length;

    await act(async () => {
      metadataChangedHandler?.({ payload: ["doc-1"] });
      await new Promise((r) => setTimeout(r, 0));
    });

    // The unsaved edit must survive — it must not be clobbered by the reload,
    // and the reload must not even happen while edits are pending.
    expect(titleInput.value).toBe("Edited Title");
    expect(vi.mocked(invoke).mock.calls.length).toBe(callsBeforeEvent);
  });

  it("reloads metadata from another tab's save when this tab has no unsaved edits", async () => {
    render(<MetadataPanel />);

    const titleInput = (await screen.findByLabelText("Title")) as HTMLInputElement;
    expect(titleInput.value).toBe("Original Title");

    vi.mocked(invoke).mockResolvedValue(metadataFixture("Title From Other Tab"));

    await act(async () => {
      metadataChangedHandler?.({ payload: ["doc-1"] });
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(titleInput.value).toBe("Title From Other Tab");
  });
});

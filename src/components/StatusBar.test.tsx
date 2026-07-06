import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { StatusBar } from "./StatusBar";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve("2.3.0")),
}));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "test.pdf",
    filePath: "C:\\test.pdf",
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

describe("StatusBar", () => {
  beforeEach(() => {
    usePdfStore.setState({ tabs: [], activeTabId: null });
  });

  it("shows the lock badge for an encrypted active tab (issues #12/#57)", () => {
    usePdfStore.setState({
      tabs: [makeTab({ encrypted: true })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.getByText("Encrypted")).toBeTruthy();
  });

  it("shows no lock badge for an unencrypted active tab", () => {
    usePdfStore.setState({
      tabs: [makeTab({ encrypted: false })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.queryByText("Encrypted")).toBeNull();
  });

  it("shows 'Verified Signed Document' for a verified active tab", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "verified" })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.getByText("Verified Signed Document")).toBeTruthy();
  });

  it("shows a warning label when modified after signing", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "modifiedAfter" })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.getByText("Signed — modified after signing")).toBeTruthy();
  });

  it("shows an invalid label when the signature failed (tamper)", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "invalid" })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.getByText("Signed — signature is invalid")).toBeTruthy();
  });

  it("shows a neutral 'not verified here' label for an unknown/unsupported signature", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "unknown" })],
      activeTabId: "tab-1",
    });
    const { container } = render(<StatusBar />);
    expect(screen.getByText("Signed — not verified here")).toBeTruthy();
    // Neutral styling, not a warning.
    expect(container.querySelector(".signature-badge-info")).not.toBeNull();
  });

  it("shows no badge for an unsigned (or unverified) tab", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "unsigned" })],
      activeTabId: "tab-1",
    });
    const { container } = render(<StatusBar />);
    expect(container.querySelector(".signature-badge")).toBeNull();
  });

  it("reflects the active tab only", () => {
    usePdfStore.setState({
      tabs: [
        makeTab({ id: "tab-1", signatureStatus: "verified" }),
        makeTab({ id: "tab-2", docId: "doc-2", signatureStatus: "invalid" }),
      ],
      activeTabId: "tab-2",
    });
    const { rerender } = render(<StatusBar />);
    // Active is tab-2 (invalid) → its badge, not tab-1's.
    expect(screen.getByText("Signed — signature is invalid")).toBeTruthy();
    expect(screen.queryByText("Verified Signed Document")).toBeNull();

    usePdfStore.setState({ activeTabId: "tab-1" });
    rerender(<StatusBar />);
    expect(screen.getByText("Verified Signed Document")).toBeTruthy();
  });

  it("shows the app version, even with no signature", async () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "unsigned" })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    const version = await screen.findByText("v2.3.0");
    expect(version).toBeTruthy();
    expect(version.className).toContain("status-version");
  });

  it("keeps the version rightmost, with the signing statement to its left", async () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "verified" })],
      activeTabId: "tab-1",
    });
    const { container } = render(<StatusBar />);
    await screen.findByText("v2.3.0");

    const bar = container.querySelector(".app-status-bar")!;
    const children = Array.from(bar.children);
    const badgeIdx = children.findIndex((c) => c.classList.contains("signature-badge"));
    const versionIdx = children.findIndex((c) => c.classList.contains("status-version"));
    // Right-justified flex row: later in DOM order == further right. Version last.
    expect(badgeIdx).toBeGreaterThanOrEqual(0);
    expect(versionIdx).toBe(children.length - 1);
    expect(versionIdx).toBeGreaterThan(badgeIdx);
  });
});

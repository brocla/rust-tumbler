import { create } from "zustand";
import type { SignatureStatus } from "../utils/signature";

export interface PageDimension {
  width: number;
  height: number;
}

// "fit-width-90" is the one-shot zoom used when a document first opens
// (issue #38): the viewer fits the page to 90% of the container width, then
// reverts the tab to "numeric" so later manual zooms behave normally.
export type ZoomMode = "numeric" | "fit-width" | "fit-page" | "fit-width-90";
export type DisplayMode = "normal" | "invert" | "sepia";

export interface TabState {
  id: string;
  docId: string;
  fileName: string;
  filePath: string;
  pageCount: number;
  pageDimensions: PageDimension[];
  currentPage: number;
  scrollTop: number;
  zoom: number;
  zoomMode: ZoomMode;
  displayMode: DisplayMode;
  searchQuery: string;
  searchResults: SearchResult[];
  searchResultIndex: number;
  metadataDirty: boolean;
  // True when the document's in-memory buffer holds unsaved edits. Owned by
  // the backend (DocEntry.dirty) and mirrored here via the
  // "document-dirty-changed" event; drives the Save button, the tab dot, and
  // the close guards. (issue #31)
  isDirty: boolean;
  loading: boolean;
  pagesVersion: number;
  // Bumped to force a content repaint (e.g. a reorder) without remounting page
  // slots. Unlike pagesVersion, a bump here does not evict the render cache, so
  // slots repaint from the (relabeled) cache instead of blanking.
  contentEpoch: number;
  sidebarScrollPage: number;
  // Bumped when OCR populates the cache for this doc (per-page via Search, or
  // whole-doc via "Make Searchable"), so the text overlay re-fetches and the
  // newly-recognized pages become selectable/copyable.
  ocrEpoch: number;
  // Bumped after a form Clear/Reset so the FormLayer overlay re-fetches field
  // values and drops any in-progress local edits. Optional so existing tab
  // construction sites don't need updating.
  formEpoch?: number;
  // Digital-signature status for the bottom status-bar badge and the
  // edit-invalidation guards. Populated on open and refreshed after edits;
  // undefined until the first verification completes. (issue #17)
  signatureStatus?: SignatureStatus;
  // True while the document is password-protected: it is fully editable (the
  // buffer is decrypted at open — issue #57) and Save re-encrypts with the
  // same password. Drives the lock badge and the toolbar's "Remove password"
  // button, which clears it. Optional so existing tab construction sites
  // don't need updating. (issues #12, #57)
  encrypted?: boolean;
  // True while the open file is linearized ("Fast Web View"). Populated on
  // open and mirrored from the backend's DocEntry.linearized via the
  // "document-dirty-changed" event — any edit turns this off, since no edit
  // path preserves the linearized structure. Drives the status-bar badge.
  // Optional so existing tab construction sites don't need updating. (issue #3)
  linearized?: boolean;
  // Regions marked for redaction but not yet applied (issue #1). Drawn as
  // black boxes by RedactLayer; sent to apply_redactions by the Redact panel.
  redactRegions?: RedactRegion[];
  // Queries used by "find & redact all" — passed to apply_redactions so
  // verification can assert the saved output has zero hits for them.
  redactQueries?: string[];
  // Visible metadata-panel fields the last "find & redact all" query hit
  // (issue #87) — drives the "Found in: Author" summary. Regions can be empty
  // while this is not, when the keyword lives only in metadata.
  redactMetadataMatches?: string[];
  // Non-null after Apply: the viewer previews the staged redacted copy
  // (rendered via render_redacted_page — the buffer is untouched) and shows
  // the preview banner. `verified` gates Save As.
  redactPreview?: { verified: boolean } | null;
  // Typewriter notes placed on the document (issue #99). The editable overlay
  // (TypewriterLayer) is authoritative; committing them writes FreeText
  // annotations into the buffer via apply_typewriter. Hydrated from the file on
  // open (read_typewriter) so notes stay re-editable across sessions.
  typewriterAnnots?: TypewriterAnnot[];
}

/**
 * docIds whose next `document-pages-changed` reload has already been applied
 * optimistically on the client (a reorder permuted the store + render cache in
 * place). The App-level reload listener consumes the id and skips the
 * evict-and-bump so the UI doesn't repaint a second time. Module-level (not
 * store state) so reading/consuming it never triggers a re-render.
 */
export const suppressedReloadDocs = new Set<string>();

export interface SearchResult {
  page: number;
  rects: { x: number; y: number; width: number; height: number }[];
}

/**
 * A rectangle marked for redaction (issue #1), mirroring the backend's
 * `RedactRegion`: PDF points, top-left origin, per-page — the same coordinate
 * space as search rects.
 */
export interface RedactRegion {
  page: number;
  rect: { x: number; y: number; width: number; height: number };
}

/**
 * A typewriter note (issue #99), mirroring the backend `TypewriterAnnot`
 * (serde camelCase). The rect is PDF points, top-left origin, per (1-based)
 * page — the same coordinate space as search/redaction rects. `color` is RGB
 * with each component in 0.0..=1.0.
 */
export interface TypewriterAnnot {
  id: string;
  page: number;
  x: number;
  y: number;
  width: number;
  height: number;
  text: string;
  fontFamily: "Helvetica" | "Times" | "Courier";
  bold: boolean;
  italic: boolean;
  fontSize: number;
  color: [number, number, number];
}

/** The style applied to the next new typewriter note (issue #99). */
export interface TypewriterStyle {
  fontFamily: "Helvetica" | "Times" | "Courier";
  bold: boolean;
  italic: boolean;
  fontSize: number;
  color: [number, number, number];
}

/** Progress of an in-flight redaction run (Tauri `redact-progress` events). */
export interface RedactProgress {
  stage: "flatten" | "reocr" | "verify";
  page: number;
  total: number;
}

export type UnsavedChoice = "save" | "discard" | "cancel";

/**
 * A pending "Save changes to <file>?" prompt. Native Tauri dialogs support at
 * most two buttons, so the three-way Save / Don't Save / Cancel choice is an
 * in-app modal (UnsavedChangesDialog) driven by this slice: `askUnsaved`
 * stores the resolver, the dialog's buttons call `resolveUnsaved`.
 */
export interface UnsavedPrompt {
  fileName: string;
  resolve: (choice: UnsavedChoice) => void;
}

/**
 * A pending password prompt for opening a user-password-protected PDF
 * (issue #12). `retry` is true when a previously entered password was rejected,
 * so the dialog can show a "wrong password, try again" state. `resolve` is
 * called with the entered password, or `null` if the user cancels.
 */
export interface PasswordPrompt {
  fileName: string;
  retry: boolean;
  resolve: (password: string | null) => void;
}

export interface CompressProgress {
  step: string;
  stepIndex: number;
  stepCount: number;
  image: number;
  imageTotal: number;
}

interface PdfStore {
  // Tab state
  tabs: TabState[];
  activeTabId: string | null;

  // Global state
  activeSidebarTool:
    | "thumbnails"
    | "search"
    | "metadata"
    | "pages"
    | "optimize"
    | "margins"
    | "redact"
    | "typewriter"
    | null;
  sidebarWidth: number;
  // Progress of an in-flight document-wide OCR run — "Make Searchable" or
  // Export Text's OCR pass (driven by Tauri `ocr-progress` events). Null when
  // none is running. Shared here so the Toolbar (which triggers the run) and
  // App (which renders the overlay) can both reach it.
  ocrProgress: { page: number; total: number } | null;
  // Progress of an in-flight compression run (the Compress panel's "Run"),
  // driven by Tauri `compress-progress` events. Null when none is running.
  // Shared here so the panel triggers the run while App renders the overlay.
  compressProgress: CompressProgress | null;
  // Progress of an in-flight redaction run (the Redact panel's "Apply"),
  // driven by Tauri `redact-progress` events. Null when none is running.
  redactProgress: RedactProgress | null;
  // True while a "Save Web-Optimized Copy" export is in flight. qpdf's C API
  // gives no incremental progress callback, so this drives an indeterminate
  // spinner rather than a page-by-page overlay like OCR/compress/redact.
  linearizeProgress: boolean;
  // True while the Redact panel's "Draw region" mode is armed: RedactLayer
  // captures a marquee drag on the page instead of text selection.
  redactDrawMode: boolean;
  // True while the Typewriter tool is armed: clicking empty page space in
  // TypewriterLayer places a new note. (issue #99)
  typewriterMode: boolean;
  // The style the next new typewriter note is created with; the panel's font
  // controls edit this (or the active note, when one is selected). (issue #99)
  typewriterStyle: TypewriterStyle;
  // The note currently selected/being edited, or null. Drives which note the
  // panel's font controls target and which shows its editing box. (issue #99)
  activeTypewriterId: string | null;
  // Non-null while an unsaved-changes prompt is showing (close guards await it).
  unsavedPrompt: UnsavedPrompt | null;
  // Non-null while a password prompt is showing for an encrypted PDF being
  // opened (the open flow awaits it). (issue #12)
  passwordPrompt: PasswordPrompt | null;
  // A transient status message shown as a dismissible toast (e.g. clicking a
  // form button whose scripted action Tumbler can't run). Null when none.
  notice: string | null;
  // True while the status bar shows the birthday dedication (the Margins
  // panel's easter egg — the tool was a birthday gift; see MarginsPanel).
  birthdayEgg: boolean;

  // Actions
  setActiveTab: (id: string) => void;
  askUnsaved: (fileName: string) => Promise<UnsavedChoice>;
  resolveUnsaved: (choice: UnsavedChoice) => void;
  askPassword: (fileName: string, retry: boolean) => Promise<string | null>;
  resolvePassword: (password: string | null) => void;
  setSidebarTool: (tool: PdfStore["activeSidebarTool"]) => void;
  setSidebarWidth: (width: number) => void;
  setOcrProgress: (progress: { page: number; total: number } | null) => void;
  setCompressProgress: (progress: CompressProgress | null) => void;
  setRedactProgress: (progress: RedactProgress | null) => void;
  setLinearizeProgress: (inProgress: boolean) => void;
  setRedactDrawMode: (on: boolean) => void;
  // Pending-region management, keyed by docId (RedactLayer lives per page and
  // knows the docId, not the tab id).
  addRedactRegions: (docId: string, regions: RedactRegion[]) => void;
  removeRedactRegion: (docId: string, index: number) => void;
  clearRedactRegions: (docId: string) => void;

  // Typewriter note management, keyed by docId (issue #99). TypewriterLayer
  // lives per page and knows the docId, not the tab id.
  setTypewriterMode: (on: boolean) => void;
  setTypewriterStyle: (patch: Partial<TypewriterStyle>) => void;
  setActiveTypewriter: (id: string | null) => void;
  addTypewriterAnnot: (docId: string, annot: TypewriterAnnot) => void;
  updateTypewriterAnnot: (docId: string, id: string, patch: Partial<TypewriterAnnot>) => void;
  removeTypewriterAnnot: (docId: string, id: string) => void;
  setTypewriterAnnots: (docId: string, annots: TypewriterAnnot[]) => void;

  showNotice: (message: string) => void;
  clearNotice: () => void;
  setBirthdayEgg: (on: boolean) => void;
  bumpFormEpoch: (docId: string) => void;

  addTab: (tab: TabState) => void;
  removeTab: (id: string) => void;
  updateTab: (id: string, updates: Partial<TabState>) => void;
  reorderTabs: (fromIndex: number, toIndex: number) => void;

  getActiveTab: () => TabState | undefined;

  // Search navigation
  nextSearchResult: () => void;
  prevSearchResult: () => void;
}

export const usePdfStore = create<PdfStore>((set, get) => ({
  tabs: [],
  activeTabId: null,
  activeSidebarTool: "thumbnails",
  sidebarWidth: 250,
  ocrProgress: null,
  compressProgress: null,
  redactProgress: null,
  linearizeProgress: false,
  redactDrawMode: false,
  typewriterMode: false,
  typewriterStyle: {
    fontFamily: "Helvetica",
    bold: false,
    italic: false,
    fontSize: 12,
    color: [0, 0, 0],
  },
  activeTypewriterId: null,
  unsavedPrompt: null,
  passwordPrompt: null,
  notice: null,
  birthdayEgg: false,

  setActiveTab: (id) => set({ activeTabId: id }),

  showNotice: (message) => set({ notice: message }),
  clearNotice: () => set({ notice: null }),
  setBirthdayEgg: (on) => set({ birthdayEgg: on }),

  bumpFormEpoch: (docId) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId ? { ...t, formEpoch: (t.formEpoch ?? 0) + 1 } : t,
      ),
    })),

  askUnsaved: (fileName) =>
    new Promise((resolve) => set({ unsavedPrompt: { fileName, resolve } })),

  resolveUnsaved: (choice) => {
    get().unsavedPrompt?.resolve(choice);
    set({ unsavedPrompt: null });
  },

  askPassword: (fileName, retry) =>
    new Promise((resolve) => set({ passwordPrompt: { fileName, retry, resolve } })),

  resolvePassword: (password) => {
    get().passwordPrompt?.resolve(password);
    set({ passwordPrompt: null });
  },

  setOcrProgress: (progress) => set({ ocrProgress: progress }),

  setCompressProgress: (progress) => set({ compressProgress: progress }),

  setRedactProgress: (progress) => set({ redactProgress: progress }),

  setLinearizeProgress: (inProgress) => set({ linearizeProgress: inProgress }),

  setRedactDrawMode: (on) => set({ redactDrawMode: on }),

  addRedactRegions: (docId, regions) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId
          ? { ...t, redactRegions: [...(t.redactRegions ?? []), ...regions] }
          : t,
      ),
    })),

  removeRedactRegion: (docId, index) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId
          ? { ...t, redactRegions: (t.redactRegions ?? []).filter((_, i) => i !== index) }
          : t,
      ),
    })),

  clearRedactRegions: (docId) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId
          ? { ...t, redactRegions: [], redactQueries: [], redactMetadataMatches: [] }
          : t,
      ),
    })),

  setTypewriterMode: (on) => set({ typewriterMode: on }),

  setTypewriterStyle: (patch) =>
    set((state) => ({ typewriterStyle: { ...state.typewriterStyle, ...patch } })),

  setActiveTypewriter: (id) => set({ activeTypewriterId: id }),

  addTypewriterAnnot: (docId, annot) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId
          ? { ...t, typewriterAnnots: [...(t.typewriterAnnots ?? []), annot] }
          : t,
      ),
    })),

  updateTypewriterAnnot: (docId, id, patch) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId
          ? {
              ...t,
              typewriterAnnots: (t.typewriterAnnots ?? []).map((a) =>
                a.id === id ? { ...a, ...patch } : a,
              ),
            }
          : t,
      ),
    })),

  removeTypewriterAnnot: (docId, id) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId
          ? { ...t, typewriterAnnots: (t.typewriterAnnots ?? []).filter((a) => a.id !== id) }
          : t,
      ),
    })),

  setTypewriterAnnots: (docId, annots) =>
    set((state) => ({
      tabs: state.tabs.map((t) =>
        t.docId === docId ? { ...t, typewriterAnnots: annots } : t,
      ),
    })),

  setSidebarTool: (tool) =>
    set((state) => ({
      activeSidebarTool: state.activeSidebarTool === tool ? null : tool,
    })),

  setSidebarWidth: (width) => set({ sidebarWidth: width }),

  addTab: (tab) =>
    set((state) => ({
      tabs: [...state.tabs, tab],
      activeTabId: tab.id,
    })),

  removeTab: (id) =>
    set((state) => {
      const idx = state.tabs.findIndex((t) => t.id === id);
      const newTabs = state.tabs.filter((t) => t.id !== id);
      let newActiveId: string | null = null;
      if (newTabs.length > 0) {
        if (state.activeTabId === id) {
          const newIdx = Math.min(idx, newTabs.length - 1);
          newActiveId = newTabs[newIdx].id;
        } else {
          newActiveId = state.activeTabId;
        }
      }
      return { tabs: newTabs, activeTabId: newActiveId };
    }),

  updateTab: (id, updates) =>
    set((state) => ({
      tabs: state.tabs.map((t) => (t.id === id ? { ...t, ...updates } : t)),
    })),

  reorderTabs: (fromIndex, toIndex) =>
    set((state) => {
      if (
        fromIndex === toIndex ||
        fromIndex < 0 ||
        fromIndex >= state.tabs.length ||
        toIndex < 0 ||
        toIndex >= state.tabs.length
      ) {
        return state;
      }
      const tabs = [...state.tabs];
      const [moved] = tabs.splice(fromIndex, 1);
      tabs.splice(toIndex, 0, moved);
      return { tabs };
    }),

  getActiveTab: () => {
    const state = get();
    return state.tabs.find((t) => t.id === state.activeTabId);
  },

  nextSearchResult: () =>
    set((state) => {
      const tab = state.tabs.find((t) => t.id === state.activeTabId);
      if (!tab || tab.searchResults.length === 0) return state;

      // Count total rects across all pages
      const totalRects = tab.searchResults.reduce(
        (sum, r) => sum + r.rects.length,
        0,
      );
      if (totalRects === 0) return state;

      const nextIndex = (tab.searchResultIndex + 1) % totalRects;

      // Find which page this rect belongs to
      let count = 0;
      let targetPage = tab.currentPage;
      for (const result of tab.searchResults) {
        if (count + result.rects.length > nextIndex) {
          targetPage = result.page;
          break;
        }
        count += result.rects.length;
      }

      return {
        tabs: state.tabs.map((t) =>
          t.id === tab.id
            ? { ...t, searchResultIndex: nextIndex, currentPage: targetPage }
            : t,
        ),
      };
    }),

  prevSearchResult: () =>
    set((state) => {
      const tab = state.tabs.find((t) => t.id === state.activeTabId);
      if (!tab || tab.searchResults.length === 0) return state;

      const totalRects = tab.searchResults.reduce(
        (sum, r) => sum + r.rects.length,
        0,
      );
      if (totalRects === 0) return state;

      const prevIndex =
        (tab.searchResultIndex - 1 + totalRects) % totalRects;

      let count = 0;
      let targetPage = tab.currentPage;
      for (const result of tab.searchResults) {
        if (count + result.rects.length > prevIndex) {
          targetPage = result.page;
          break;
        }
        count += result.rects.length;
      }

      return {
        tabs: state.tabs.map((t) =>
          t.id === tab.id
            ? { ...t, searchResultIndex: prevIndex, currentPage: targetPage }
            : t,
        ),
      };
    }),
}));

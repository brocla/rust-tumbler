# Tumbler: Requirements Document

**Product:** Tumbler -- A personal PDF viewer, editor, and printer  
**Version:** 2.0 (rewrite with pdfium)  
**Platform:** Windows 10 and Windows 11  
**Date:** 2026-06-12

---

## 1. Overview

Tumbler is a desktop PDF application built with Tauri v2 (Rust backend, React/TypeScript frontend). It views, searches, annotates metadata, prints, and will eventually edit PDF documents. It is designed to be the user's default PDF application on Windows.

### 1.1 Design Principles

- **Self-sufficient.** Tumbler must not depend on third-party applications for any of its capabilities. It may depend only on its own code and operating system APIs.
- **Print quality first.** PDF rendering for printing must produce output at the printer's native resolution. No intermediate rasterization at screen DPI.
- **OS-native UX.** Use standard Windows UI (common dialogs, system theme, accent colors) rather than custom-built equivalents wherever possible.
- **Responsive.** Large documents (500+ pages) must remain interactive. Rendering, search, and printing must not freeze the UI.

---

## 2. Technology Stack

### 2.1 Application Shell

| Layer | Technology | Role |
|---|---|---|
| Runtime | Tauri v2 | Window management, IPC, file system access, OS integration, installer |
| Frontend | React 18 + TypeScript | UI components, state management |
| State | Zustand | Per-tab and global application state |
| Bundler | Vite | Development server and production builds |
| Testing | Vitest + jsdom | Unit and integration tests |
| Icons | lucide-react | Toolbar and UI icons |

### 2.2 PDF Engine

| Library | Language | Role |
|---|---|---|
| **pdfium** | C/C++ (via Rust FFI) | PDF rendering (screen and printer), text extraction, page geometry, form filling, page manipulation (merge, split, delete, rotate, reorder) |
| **lopdf** | Rust | Low-level PDF object access: metadata writing, CropBox manipulation |

All PDF operations run in the Rust backend. The frontend is a pure UI layer — it sends commands and receives results, never touching PDF bytes directly.

**pdfium** is the PDF rendering engine from Chromium. It provides:
- Page rendering to bitmaps (for screen display via `FPDF_RenderPageBitmap`)
- Page rendering to Windows GDI device contexts (for printing at native printer DPI via `FPDF_RenderPage`)
- Text extraction from all page types (including those with complex encodings)
- Page dimensions and geometry
- Form field enumeration, reading, and filling
- Page import between documents (`FPDF_ImportPages` for merge/split)
- Page deletion (`FPDFPage_Delete`) and rotation (`FPDFPage_SetRotation`)
- Document saving (`FPDF_SaveAsCopy` / `FPDF_SaveWithVersion`)

**Rust integration:** Use `pdfium-render` crate (Rust bindings) or direct FFI to the pdfium shared library. The `pdfium.dll` binary (~5 MB) is bundled with the application installer.

**lopdf** is a pure-Rust PDF library (~100 KB) that provides low-level access to PDF internal objects. It fills two gaps in pdfium's API:
- **Metadata writing:** Access the document info dictionary to set Title, Author, Subject, Keywords, Creator, and date fields.
- **CropBox manipulation:** Read and write page CropBox entries for the crop feature.

lopdf does not render or parse page content — it only reads and writes PDF structure. This keeps the two libraries cleanly separated: pdfium owns rendering and page-level operations, lopdf owns object-level mutations that pdfium cannot perform.

### 2.3 OS Integration

| Component | Technology |
|---|---|
| Print dialog | `PrintDlgExW` (Windows Common Dialog, comdlg32) |
| Print rendering | pdfium `FPDF_RenderPage` to GDI `HDC` from `CreateDC` |
| Accent color | Windows `UISettings` COM API (`UI_ViewManagement`) |
| File association | Tauri NSIS/MSI installer, registered as `.pdf` handler |
| Theme detection | CSS `prefers-color-scheme` and `prefers-contrast` media queries |

---

## 3. Application Structure

### 3.1 Window Layout

```
+--------------------------------------------------------------+
|  Toolbar (42px)                                              |
+--------------------------------------------------------------+
|  Tab Bar (34px)                                              |
+----+----------+----------------------------------------------+
| I  | Sidebar  |                                              |
| c  | Panel    |          Viewer Area                         |
| o  | (resize- |        (continuous scroll)                   |
| n  |  able,   |                                              |
|    | 150-500  |                                              |
| R  |   px)    |                                              |
| a  |          |                                              |
| i  |          |                                              |
| l  |          |                                              |
+----+----------+----------------------------------------------+
```

- **Toolbar:** File open, page navigation, zoom controls, display mode, print button.
- **Tab Bar:** One tab per open document. Drag-to-reorder. Close button with unsaved-changes warning.
- **Icon Rail:** Toggle buttons for sidebar panels (Thumbnails, Search, Metadata).
- **Sidebar Panel:** Context-dependent panel. Resizable with drag handle, width persisted to localStorage.
- **Viewer Area:** Continuous vertical scroll of PDF pages, or empty-state prompt when no document is open.

### 3.2 State Architecture

Each open document is an independent tab with its own:
- Document identifier (references a pdfium `FPDF_DOCUMENT` held in the Rust backend)
- File bytes (held in Rust; frontend receives only rendered bitmaps and extracted data), file name
- Page count, page dimensions
- Current page, scroll position
- Zoom level and zoom mode
- Display mode (normal, invert, sepia)
- Search query, results, and result index
- Metadata dirty flag
- Loading state

Global state (shared across tabs):
- Active tab ID
- Active sidebar tool
- Sidebar width

---

## 4. Core Features

### 4.1 Document Loading

- **Open via toolbar** (Ctrl+O): Native file picker dialog filtered to `*.pdf`.
- **Open via file association:** Double-clicking a `.pdf` in Explorer launches Tumbler with the file path as a command-line argument. If Tumbler is already running, the file opens in a new tab.
- **Validation:** Reject files without a `%PDF` magic header. Display user-friendly error messages for invalid, encrypted, or corrupted files.
- **Loading state:** Toolbar shows "Opening..." and disables interaction during load.

### 4.2 Page Rendering

All page rendering is performed by pdfium.

- **Screen rendering:** `FPDF_RenderPageBitmap` renders pages to BGRA bitmaps at the current zoom level and device pixel ratio. Bitmaps are transferred to HTML canvas elements for display.
- **Virtual rendering:** Only pages within a render radius of the current page are rendered. Pages outside the radius display placeholder elements matching the page's dimensions.
- **Render radius:** 2 pages above and below the current page (configurable).
- **Prefetching:** When the current page changes, neighboring pages are rendered asynchronously and cached.
- **Caching:** LRU cache (20 entries) keyed by `docId:page:zoom:dpr`. ImageBitmaps are reused across re-renders.
- **Resource cleanup:** When a tab closes, its document handle is destroyed and its cache entries are evicted.

### 4.3 Continuous Scroll Viewer

- Pages are stacked vertically with a 16px gap.
- IntersectionObserver tracks the most-visible page and updates current page state.
- `requestJumpToPage(n)` scrolls smoothly to the target page.
- Scroll position is saved per-tab and restored when switching tabs.
- Zoom changes preserve the anchor page's position in the viewport.

### 4.4 Zoom

Three zoom modes:

| Mode | Behavior |
|---|---|
| Numeric | Fixed zoom level from 10% to 400%. Presets: 10, 25, 50, 75, 100, 125, 150, 200, 300, 400%. |
| Fit Width | Page width fills the viewer area. Recomputed on viewer resize. |
| Fit Page | Entire page fits in viewport. Recomputed on viewer resize. |

- **Zoom buttons:** Step to next/previous preset.
- **Zoom dropdown:** Select preset or fit mode. Shows custom percentage if zoom doesn't match a preset.
- **Ctrl+Scroll wheel:** Zoom in/out by 12% per tick.

### 4.5 Text Layer

- Invisible selectable text overlay rendered on top of each page canvas.
- Enables native text selection and copy (Ctrl+C) from rendered pages.
- Text positions are derived from pdfium's text extraction API and mapped to the canvas coordinate system.

### 4.6 Search

- **Input:** Search field in the sidebar panel with live-as-you-type triggering.
- **Scope:** All pages, case-insensitive substring matching.
- **Results:** List of page numbers containing matches, displayed in a paginated sidebar list (20 per page).
- **Navigation:** Prev/Next buttons, Enter/Shift+Enter in the input, or click a result to jump.
- **Highlighting:** Yellow rectangles (rgba(255, 210, 0, 0.35)) overlaid on matching text spans. Highlights span across text item boundaries where a match crosses items.
- **Focus:** Ctrl+F opens the search panel (if not already open), puts keyboard focus in the search input, and selects all existing text so the user can immediately type a new query. If the panel is already open, Ctrl+F still focuses and selects. The handler must intercept Ctrl+F in the capture phase (`capture: true`) with `preventDefault()` and `stopImmediatePropagation()` to suppress WebView2's built-in find dialog, which would otherwise steal the keystroke. The handler must be registered at the application root (not inside the search panel component) so it is active regardless of whether the search panel is currently mounted.

### 4.7 Thumbnails

- Grid of page thumbnails at 18% of natural page size.
- Click a thumbnail to jump to that page.
- Active page has an accent-colored border.
- Rendered lazily on demand using pdfium.

### 4.8 Metadata Editing

- **Editable fields:** Title, Author, Subject, Keywords, Creator.
- **Read-only fields:** Producer, Creation Date, Modification Date.
- **Reading:** pdfium's `FPDF_GetMetaText` retrieves metadata fields from the loaded document.
- **Writing:** lopdf modifies the document's info dictionary and saves the updated bytes.
- **Dirty tracking:** A save button appears when any field is modified. Tab shows a dirty indicator.
- **Save workflow:** Modified metadata is written via a Rust backend command. The document is reloaded in the viewer with the updated bytes.
- **Close warning:** Closing a tab with unsaved metadata changes prompts for confirmation.

### 4.9 Display Modes

Three modes, cycled via toolbar button:

| Mode | Effect |
|---|---|
| Normal | No filter. |
| Invert | CSS `filter: invert(1) hue-rotate(180deg)` on page canvases. Dark backgrounds, light text. |
| Sepia | CSS `filter: sepia(0.6) brightness(0.9)` on page canvases. Warm paper tone. |

Display mode is per-tab state.

### 4.10 Multi-Document Tabs

- Each open document is an independent tab with fully isolated state.
- **New tab:** "+" button or Ctrl+O into an empty tab.
- **Close tab:** "x" button with unsaved-changes confirmation. Closing the last tab returns to the empty state.
- **Switch tab:** Click. Scroll position, zoom, search state, and display mode are preserved and restored.
- **Reorder:** Drag a tab chip and drop at a new position. Visual drop-gap indicator shows the insertion point.
- **Active indicator:** Bold underline in the system accent color.

---

## 5. Printing

### 5.1 Objectives

1. **Print quality:** Output is rendered at the printer's native resolution via `FPDF_RenderPage` to a GDI device context. Text is vector-sharp. Images are reproduced at their embedded resolution. No intermediate rasterization at screen DPI.
2. **Self-sufficient:** Printing uses only pdfium's raw C API (loaded via `libloading`), the Windows GDI, and the printer driver. No external PDF application is involved.
3. **Standard UX:** The Windows common print dialog (`PrintDlgExW`) handles printer selection and job configuration. The "Print to File" checkbox is hidden (`PD_HIDEPRINTTOFILE`) because it is non-functional with GDI rendering.
4. **No print preview.** Print preview was considered and rejected. It would require rendering pages at printer DPI into bitmaps for on-screen display — a substantial implementation effort with little benefit given that the viewer already shows the PDF content. Users configure the print job entirely through `PrintDlgExW`.

### 5.2 Print Architecture

The pdfium-render crate's `bindings()` and `page_handle()` methods are `pub(crate)`, making `FPDF_RenderPage` inaccessible through the high-level API. The solution loads pdfium.dll via the `libloading` crate and calls the raw C functions directly. This also solves thread safety: the STA print thread works with its own document handle, independent of the main pdfium-render instance.

```
Frontend                          Rust Backend
--------                          ------------
Ctrl+P or Print button
  |
  invoke("print_document",     1. Look up file_path from AppState for the doc_id
         { docId })  -------->  2. Resolve pdfium.dll path
                                3. Spawn STA thread (COM requirement for PrintDlgExW):
                                     CoInitializeEx(COINIT_APARTMENTTHREADED)
                                4. Load pdfium.dll via libloading, bind raw C functions
                                5. FPDF_LoadDocument (separate instance from viewer)
                                6. Show PrintDlgExW (PD_HIDEPRINTTOFILE flag set)
                                7. Extract DEVNAMES → printer name, DEVMODE → job settings
                                8. CreateDCW(printer_name, DEVMODE) → HDC
                                9. StartDocW / for each page:
                                     StartPage
                                     Scale page to fit printable area (aspect-ratio preserved, centered)
                                     FPDF_RenderPage(HDC, page, ..., FPDF_PRINTING | FPDF_ANNOT)
                                     EndPage
                                     emit("print-progress", { page, total })
                                10. EndDoc
                                11. RAII guards clean up: DC, DEVMODE/DEVNAMES globals, pdfium document
                                12. CoUninitialize
  <-------- PrintResult         13. Return { printed: true, pages_printed: N }
  Hide progress overlay
```

### 5.3 Print Dialog Settings

`PrintDlgExW` is configured with flags: `PD_ALLPAGES | PD_NOSELECTION | PD_NOCURRENTPAGE | PD_USEDEVMODECOPIESANDCOLLATE | PD_HIDEPRINTTOFILE`.

The following settings are read from the dialog result and honored via the `DEVMODE` structure passed to `CreateDCW`:

| Setting | Source | Applied via |
|---|---|---|
| Printer | DEVNAMES.wDeviceOffset | `CreateDCW` printer name parameter |
| Copies | DEVMODE.dmCopies | Passed to printer driver via DEVMODE |
| Page range | PRINTPAGERANGE | Controls which pages are rendered in the loop |
| Duplex | DEVMODE.dmDuplex | Passed to printer driver via DEVMODE |
| Orientation | DEVMODE.dmOrientation | Passed to printer driver via DEVMODE |
| Paper size | DEVMODE.dmPaperSize | Passed to printer driver via DEVMODE |
| Print quality | DEVMODE.dmPrintQuality | Passed to printer driver via DEVMODE |

### 5.4 Print Rendering

- `FPDF_RenderPage` renders directly to the printer's GDI device context (HDC) with `FPDF_PRINTING | FPDF_ANNOT` flags. GDI drawing commands are sent to the printer driver, which converts them to the printer's native language at full device resolution.
- The page is scaled to fit the printable area (`GetDeviceCaps` `HORZRES`/`VERTRES`) while preserving aspect ratio. The rendered page is centered within the printable area.
- No bitmaps are created on the application side for vector content. Images embedded in the PDF are rendered at their native resolution or the printer's DPI, whichever is lower.

### 5.5 Print Progress

- The Rust print loop emits `print-progress` Tauri events with `{ page, total }` after each page.
- The frontend shows a modal overlay: "Printing page N of M..." which disappears when the command completes.
- Cancellation is not implemented. The print job runs to completion once started. `AbortDoc` is called only if `StartPage` or `EndPage` fails mid-job.

### 5.6 Resource Management

RAII guard types ensure cleanup on all exit paths (success, error, or early return):
- **`DocumentGuard`** — calls `FPDF_CloseDocument` on the separately-loaded pdfium document.
- **`GlobalHandleGuard`** — calls `GlobalFree` on `hDevMode` and `hDevNames` handles allocated by `PrintDlgExW`.
- **`DcGuard`** — calls `DeleteDC` on the printer device context.

### 5.7 Error Handling

- Errors from `PrintDlgExW`, `CreateDCW`, `StartDocW`, or pdfium loading are returned as `AppError` variants, converted to `String` at the Tauri command boundary, and shown to the user via a Tauri message dialog.
- If the user cancels the print dialog (`dwResultAction != PD_RESULT_PRINT`), the command returns `PrintResult { printed: false, pages_printed: 0 }` — no error is raised.

---

## 6. OS Integration

### 6.1 Theme and Accent Color

- **Dark/Light mode:** CSS custom properties switch values based on `prefers-color-scheme`. All UI elements use these properties, never hardcoded colors.
- **Windows accent color:** On startup, the Rust backend reads the user's accent color via the `UISettings` COM API and passes it to the frontend, which overrides `--color-accent` and `--color-accent-dim`.
- **High contrast:** When `prefers-contrast: more` is active, all colors map to Windows system colors (Canvas, ButtonText, Highlight, etc.). Decorative shadows and dark-mode filters are suppressed.

### 6.2 File Association

- The NSIS and MSI installers register Tumbler as a handler for `.pdf` files.
- When launched via file association, the PDF path is passed as a command-line argument and opened in a new tab on startup.
- Tumbler is designed to be the user's default PDF application.

### 6.3 Window

- Default size: 1280x800. Minimum: 900x600. Resizable.
- Title bar shows "Tumbler".
- Application icon: custom Tumbler icon in all required sizes (ICO, ICNS, PNG).

---

## 7. Future Enhancements

These capabilities are planned but not part of the initial implementation. The architecture and data model should accommodate them without major refactoring.

### 7.1 Document Operations

| Operation | Description |
|---|---|
| **Merge** | Combine two or more PDF files into a single document. |
| **Split** | Extract one or more page ranges into separate PDF files. |
| **Add pages** | Insert pages from another PDF at a specified position. |
| **Delete pages** | Remove selected pages from the document. |
| **Reorder pages** | Drag-and-drop page reordering via the thumbnail panel. |
| **Rotate pages** | Rotate selected pages by 90, 180, or 270 degrees. |
| **Crop pages** | Adjust the visible crop box of selected pages. |

These operations are performed by pdfium in the Rust backend (`FPDF_ImportPages`, `FPDFPage_Delete`, `FPDFPage_SetRotation`, `FPDF_SaveAsCopy`). Crop uses lopdf for CropBox manipulation. The user saves the result via File > Save / Save As.

### 7.2 Form Filling

- Detect and enumerate form fields (text, checkbox, radio, dropdown) in a PDF using pdfium's form API.
- Display form fields as interactive overlays on the rendered page.
- User fills in fields. Changes are saved back to the PDF bytes.
- Support for saving filled forms as new PDF files.

### 7.3 Text Extraction

- Extract plain text from any PDF (all pages or a selected range).
- Output as a `.txt` file via File > Export Text.
- Handle PDFs with complex encodings, ligatures, and right-to-left text via pdfium's text extraction API.

### 7.4 Additional Printing Features

- **Print to PDF:** Generate a new PDF from a page range via pdfium.
- **Watermark / stamp:** Overlay text or image on printed output.
- **Booklet printing:** Reorder and impose pages for saddle-stitch booklet folding.

---

## 8. Build and Distribution

### 8.1 Build Pipeline

- `npm run build` compiles TypeScript and bundles the frontend via Vite.
- `cargo tauri build` compiles the Rust backend and packages the application.
- pdfium.dll is included in the bundle resources and loaded at runtime.

### 8.2 Installers

| Format | Purpose |
|---|---|
| NSIS | Standard Windows installer with file association registration |
| MSI | Enterprise/group-policy deployment |

- Per-user install (no admin rights required).
- WebView2 bootstrapper downloaded during install if not already present.

### 8.3 Dependencies Bundled

| Dependency | Size | Notes |
|---|---|---|
| pdfium.dll | ~5 MB | Chromium PDF engine. Bundled, not downloaded at runtime. |
| lopdf | ~100 KB | Pure Rust, compiled into the backend binary. No runtime artifact. |
| WebView2 | ~150 MB (runtime) | Installed separately via bootstrapper. Pre-installed on Windows 11. |

---

## 9. Testing

- **Unit tests:** Pure functions (zoom math, page cache, state reducers) tested via Vitest.
- **Component tests:** React components tested in jsdom with mocked pdfium/Tauri APIs.
- **Rust tests:** Backend commands tested via `cargo test` with mocked Windows APIs where applicable.
- **Manual test matrix:**
  - 1-page, 10-page, 100-page, 500-page PDFs
  - PDFs with forms, images, vector graphics, complex fonts, mixed page sizes
  - Print to physical printer, print to Microsoft Print to PDF
  - Light mode, dark mode, high contrast mode
  - Windows 10, Windows 11
  - 100%, 125%, 150%, 200% display scaling

---

## 10. Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| Ctrl+O | Open PDF file |
| Ctrl+P | Print |
| Ctrl+F | Focus search input |
| Ctrl+Scroll | Zoom in/out |
| Page Up | Previous page |
| Page Down | Next page |
| Enter (search) | Next search result |
| Shift+Enter (search) | Previous search result |
| Enter (page input) | Jump to page number |

# Tumbler: Implementation Plan

**Audience:** An AI coding assistant (Claude Code) starting a fresh context with no prior knowledge of this project's history. This document captures hard-won lessons from a prior implementation attempt and provides a specific, sequenced plan.

**Companion document:** `requirements.md` describes *what* to build. This document describes *how* and *in what order*.

**Developer environment:** Windows 11, PowerShell, VS Code with Claude Code extension. Target platforms: Windows 10 and 11.

---

## Critical: Tool Usage Note

When reading files during implementation, always use the **Read** tool, never `cat`, `head`, `tail`, or `sed` via Bash. The Read tool does not trigger permission prompts in the Claude Code extension. Bash file-reading commands will trigger repeated permission asks that slow development.

Use Bash only for: `cargo build`, `npm run`, `git` commands, and other true shell operations.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Hard-Won Lessons](#2-hard-won-lessons)
3. [Dependency Decisions](#3-dependency-decisions)
4. [Build Phases](#4-build-phases)
5. [Phase 1: Project Scaffold](#phase-1-project-scaffold)
6. [Phase 2: pdfium Integration](#phase-2-pdfium-integration)
7. [Phase 3: Viewer Core](#phase-3-viewer-core)
8. [Phase 4: Text Layer and Search](#phase-4-text-layer-and-search)
9. [Phase 5: Sidebar Panels](#phase-5-sidebar-panels)
10. [Phase 6: Printing](#phase-6-printing)
11. [Phase 7: Metadata Editing](#phase-7-metadata-editing)
12. [Phase 8: Multi-Document Tabs](#phase-8-multi-document-tabs)
13. [Phase 9: Polish and OS Integration](#phase-9-polish-and-os-integration)
14. [Future Phases](#future-phases)

---

## 1. Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                    React Frontend                        │
│  (UI only — no PDF parsing, no PDF bytes)               │
│                                                         │
│  Components:  Toolbar, TabBar, IconRail, Sidebar,       │
│               ContinuousViewer, PageSlot, SearchPanel,  │
│               ThumbnailPanel, MetadataPanel             │
│                                                         │
│  State:       Zustand store (per-tab + global)          │
│                                                         │
│  Rendering:   Receives BGRA bitmaps from Rust,          │
│               displays on <canvas> via ImageBitmap      │
└──────────────────┬──────────────────────────────────────┘
                   │ Tauri IPC (invoke commands)
                   │ - Bitmaps sent as binary (not JSON)
                   │ - Events emitted for progress
┌──────────────────┴──────────────────────────────────────┐
│                    Rust Backend                          │
│                                                         │
│  pdfium-render:   Document loading, page rendering,     │
│                   text extraction, form filling,        │
│                   page manipulation, GDI printing       │
│                                                         │
│  lopdf:           Metadata writing, CropBox mutation    │
│                                                         │
│  windows crate:   PrintDlgExW, CreateDC, GDI calls,    │
│                   UISettings (accent color), COM        │
│                                                         │
│  State:           HashMap<DocId, PdfiumDocument>        │
│                   managed via Tauri state                │
└─────────────────────────────────────────────────────────┘
```

### The IPC Boundary Rule

**PDF bytes never cross the IPC bridge.** This was a key lesson. Tauri's `invoke` serializes via JSON. Sending megabytes of PDF data from Rust to JS (or back) is slow and error-prone. Instead:

- The frontend sends a **file path** to Rust. Rust loads the PDF.
- Rust returns a **document ID** (string). The frontend stores this ID per tab.
- For rendering, the frontend calls `invoke("render_page", { docId, page, scale, dpr })`. Rust returns **raw BGRA bytes** as a binary response (Tauri supports binary return via `Vec<u8>` / `tauri::ipc::Response`).
- For text, search, metadata — the frontend calls Rust commands that return small JSON payloads.
- For saving, the frontend calls `invoke("save_document", { docId, path })`. Rust writes the file.

The frontend never has `Uint8Array` PDF bytes. It only has document IDs and rendered bitmaps.

---

## 2. Hard-Won Lessons

These are specific technical problems encountered during the prior implementation. Each one cost significant debugging time. Do not repeat them.

### 2.1 COM STA Threading for PrintDlgExW

**Problem:** `PrintDlgExW` (the Windows common print dialog) requires COM Single-Threaded Apartment (STA) with a message pump. WebView2 initializes COM as Multi-Threaded Apartment (MTA) on Tauri's main thread AND on its worker threads. Calling `PrintDlgExW` on any Tauri thread fails with "Class not registered" (0x80070006).

**Solution:** Spawn a **dedicated `std::thread`** and call `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` as the first thing on that thread. Run `PrintDlgExW` on that thread. Use an `mpsc::channel` to send the result back to the Tauri async command.

```rust
#[tauri::command]
async fn show_print_dialog(window: tauri::WebviewWindow, page_count: u32) -> Result<PrintSettings, String> {
    let hwnd_raw = window.hwnd().map_err(|e| format!("hwnd() failed: {e}"))?.0 as isize;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // MUST be first call on this thread
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let result = show_print_dialog_impl(page_count, hwnd_raw);
        unsafe { CoUninitialize(); }
        tx.send(result).ok();
    });
    rx.recv().map_err(|e| format!("channel recv failed: {e}"))?
}
```

Do NOT try:
- Calling `PrintDlgExW` on the Tauri main thread (MTA)
- Using `tauri::WebviewWindow::run_on_main_thread` (still MTA)
- Using `tokio::task::spawn_blocking` (Tauri's tokio runtime threads are MTA)

### 2.2 HWND Ownership for PrintDlgExW

**Problem:** `PrintDlgExW` requires `hwndOwner` to be set to a valid window handle. If it's null/zero, the dialog either fails with "The handle is invalid" or appears behind the app window and seems to do nothing.

**Solution:** Get the HWND from Tauri's `WebviewWindow`:

```rust
let hwnd_raw = window.hwnd().map_err(|e| format!("hwnd() failed: {e}"))?.0 as isize;
// Pass to the STA thread, construct HWND there:
hwndOwner: HWND(hwnd_raw as *mut core::ffi::c_void),
```

The `window` parameter in a `#[tauri::command] async fn` is injected by Tauri automatically — you just include `window: tauri::WebviewWindow` in the function signature.

### 2.3 DEVMODE Must Be Passed to CreateDC

**Problem (in prior attempt):** `PrintDlgExW` returns `hDevMode` containing duplex, orientation, paper size, copies, quality settings. The prior implementation freed `hDevMode` without reading it, so all those settings were silently discarded. Users could configure duplex in the dialog but it had no effect.

**Solution:** After `PrintDlgExW` returns, `GlobalLock(hDevMode)` to get a `*const DEVMODEW`, pass it to `CreateDCW` as the `lpInitData` parameter. This applies all user-selected settings to the printer device context.

```rust
let devmode_ptr = GlobalLock(pdx.hDevMode) as *const DEVMODEW;
let hdc = CreateDCW(None, printer_pcwstr, None, Some(devmode_ptr));
GlobalUnlock(pdx.hDevMode);
// ... use hdc for StartDoc/FPDF_RenderPage/EndDoc ...
DeleteDC(hdc);
GlobalFree(Some(pdx.hDevMode));
GlobalFree(Some(pdx.hDevNames));
```

### 2.4 ShellExecuteExW Is Not Printing

**Do not use `ShellExecuteExW` with "print" or "printto" verbs for printing.** This was the prior approach and it has fundamental problems:

- Delegates rendering to whatever application owns the `.pdf` file association (Foxit, Edge, etc.)
- If Tumbler IS the default PDF app, it tries to print via itself — infinite loop or silent failure
- The "printto" verb is broken in many PDF handlers (Foxit confirmed broken)
- DEVMODE settings (duplex, orientation, etc.) cannot be passed to the external handler
- Fire-and-forget: no way to know if printing succeeded or failed

**The correct approach:** Load the PDF in pdfium, render each page to the printer's GDI HDC via `FPDF_RenderPage`. This is self-contained, honors all DEVMODE settings, and reports success/failure.

### 2.5 Tauri Binary IPC

**Problem:** Sending large `Uint8Array` (PDF bytes, rendered bitmaps) via Tauri's JSON-based `invoke` is slow and can fail for large documents.

**Solution for returning bitmaps from Rust:** Use Tauri's binary response support. A `#[tauri::command]` can return `tauri::ipc::Response` wrapping raw bytes, which Tauri transfers efficiently without JSON encoding. On the frontend, `invoke` returns an `ArrayBuffer`.

Research the current Tauri v2 API for binary responses at implementation time — the exact API may be `tauri::ipc::Response::new(bytes)` or a custom serializer. The key point: rendered page bitmaps (BGRA, potentially several MB each) must be transferred as binary, not base64-encoded JSON strings.

### 2.6 GlobalFree API in windows crate 0.61

In `windows` crate v0.61+, `GlobalFree` is in `Win32::Foundation` (not `Win32::System::Memory`), and takes `Option<HGLOBAL>`:

```rust
use windows::Win32::Foundation::GlobalFree;
unsafe { let _ = GlobalFree(Some(handle)); }
```

### 2.7 CoInitializeEx Return Value

`CoInitializeEx` returns `HRESULT`, not `Result`. Check with `.is_err()`:

```rust
let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
if hr.is_err() {
    return Err(format!("CoInitializeEx failed: HRESULT 0x{:08x}", hr.0));
}
```

`S_OK` (0) and `S_FALSE` (1, already initialized) are both success.

---

## 3. Dependency Decisions

### 3.1 pdfium-render

**Crate:** `pdfium-render` (v0.9.x on crates.io)  
**Feature flags:** `pdfium_use_win32` (enables `FPDF_RenderPage` for GDI HDC printing)  
**Loading:** Runtime late-binding via `libloading`. Call `Pdfium::bind_to_library("path/to/pdfium.dll")`.

In Cargo.toml:
```toml
[dependencies]
pdfium-render = { version = "0.9", features = ["pdfium_use_win32"] }
```

### 3.2 pdfium.dll Binary

**Source:** [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) on GitHub.  
**Build to use:** `pdfium-win-x64.tgz` (no V8, no XFA). The V8 build bundles the V8 JavaScript engine and is much larger — only needed for PDFs with embedded JavaScript, which is rare.  
**Size:** ~20-30 MB for the DLL.

**Bundling strategy:**
1. Download the release tarball and extract `bin/pdfium.dll`.
2. Place it in `src-tauri/resources/pdfium.dll`.
3. In `tauri.conf.json`, add to the bundle resources: `"resources": ["resources/pdfium.dll"]`.
4. At runtime, resolve the path: `app.path().resource_dir()?.join("resources/pdfium.dll")`.
5. Pass that path to `Pdfium::bind_to_library(path)`.

For development, place `pdfium.dll` in a known location (e.g., `src-tauri/resources/`) and load it with a relative or absolute path. Test with `cargo build` before `cargo tauri build` to verify linking.

### 3.3 lopdf

**Crate:** `lopdf` (pure Rust, ~100 KB compiled)  
**Role:** Metadata writing (set info dictionary fields) and CropBox manipulation.  
**Usage:** Load PDF bytes into `lopdf::Document`, modify the trailer/page objects, save back to bytes. This is independent of pdfium — the two libraries operate on the same PDF bytes but at different levels (pdfium for rendering/structure, lopdf for raw object mutation).

### 3.4 windows crate

**Crate:** `windows` (v0.61+)  
**Feature flags needed:**

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.61", features = [
    "UI_ViewManagement",           # Accent color (UISettings)
    "Win32_Foundation",            # HWND, HGLOBAL, GlobalFree, etc.
    "Win32_Graphics_Gdi",          # CreateDC, StartDoc, StartPage, EndPage, EndDoc, DeleteDC, DEVMODEW
    "Win32_Graphics_Printing",     # GetDefaultPrinterW (may be needed for fallback)
    "Win32_System_Com",            # CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED
    "Win32_System_Memory",         # GlobalLock, GlobalUnlock
    "Win32_UI_Controls_Dialogs",   # PrintDlgExW, PRINTDLGEXW, DEVNAMES, etc.
] }
```

Note: The feature is `Win32_UI_Controls_Dialogs` (with `_Dialogs` suffix), not `Win32_UI_Controls`. This was a compile error in the prior attempt.

### 3.5 Frontend Dependencies

```json
{
  "dependencies": {
    "@tauri-apps/api": "^2.0.0",
    "@tauri-apps/plugin-dialog": "^2.0.0",
    "@tauri-apps/plugin-fs": "^2.0.0",
    "lucide-react": "^1.17.0",
    "react": "^18.3.1",
    "react-dom": "^18.3.1",
    "zustand": "^5.0.0"
  }
}
```

**Removed from prior version:** `pdfjs-dist`, `pdf-lib`, `@tauri-apps/plugin-shell`. All PDF operations are now Rust-side. The shell plugin was only used for `ShellExecuteExW` printing, which is eliminated.

---

## 4. Build Phases

The implementation is sequenced so that each phase builds on the previous and produces a testable increment. Phases are ordered by dependency: foundational infrastructure first, then viewer, then features that depend on the viewer.

| Phase | Deliverable | Depends on |
|---|---|---|
| 1. Scaffold | Tauri + React project compiles and opens a window | Nothing |
| 2. pdfium | Load a PDF, render one page to bitmap, display on canvas | Phase 1 |
| 3. Viewer | Continuous scroll, virtual rendering, zoom, keyboard nav | Phase 2 |
| 4. Text & Search | Text layer overlay, full-text search, highlighting | Phase 2 |
| 5. Sidebar | Icon rail, thumbnail panel, search panel | Phases 3 + 4 |
| 6. Printing | PrintDlgExW + GDI rendering | Phase 2 |
| 7. Metadata | Read/write metadata via pdfium + lopdf | Phase 2 |
| 8. Tabs | Multi-document tabs with drag reorder | Phase 3 |
| 9. Polish | Theming, accent color, file association, installer | All above |

---

## Phase 1: Project Scaffold

### Goal
Tauri v2 + React + TypeScript project that compiles, opens a window, and renders a "Hello World" React app.

### Steps

1. **Create the Tauri project:**
   ```powershell
   npm create tauri-app@latest tumbler -- --template react-ts
   cd tumbler
   ```

2. **Verify it builds and runs:**
   ```powershell
   npm install
   npm run tauri dev
   ```

3. **Set up the project structure:**
   ```
   src/
     components/       # React components
     store/            # Zustand store
     utils/            # Constants, helpers
     styles/           # CSS
     App.tsx
     main.tsx
   src-tauri/
     src/
       lib.rs          # Tauri commands
     resources/
       pdfium.dll      # Added in Phase 2
     Cargo.toml
     tauri.conf.json
   docs/
     requirements.md
     implementation-plan.md
   ```

4. **Configure `tauri.conf.json`:**
   - `productName`: "Tumbler"
   - `identifier`: "com.brocla.tumbler"
   - Window: 1280x800 default, 900x600 minimum
   - Bundle targets: `["nsis", "msi"]`
   - File associations: `[{ "ext": ["pdf"], "name": "PDF Document" }]`

5. **Set up Zustand store** with initial empty structure (will grow per phase).

6. **Set up CSS** with design tokens (custom properties for colors, spacing). Include `prefers-color-scheme` light/dark and `prefers-contrast` high-contrast media queries from the start.

### Verification
- `npm run tauri dev` opens a window with React content.
- `cargo build` succeeds in `src-tauri/`.

---

## Phase 2: pdfium Integration

### Goal
Load a PDF file via pdfium in Rust, render a single page to BGRA bitmap, transfer to the frontend, and display it on an HTML canvas.

This is the most critical phase. It proves the entire rendering pipeline end-to-end. Get this right before building any UI.

### Steps

1. **Download pdfium.dll:**
   - Go to [bblanchon/pdfium-binaries releases](https://github.com/bblanchon/pdfium-binaries/releases)
   - Download `pdfium-win-x64.tgz` (no V8 build)
   - Extract `bin/pdfium.dll` to `src-tauri/resources/pdfium.dll`
   - Also extract the header files into a reference directory (not needed for compilation, but useful for API reference)

2. **Add Rust dependencies to `src-tauri/Cargo.toml`:**
   ```toml
   [dependencies]
   pdfium-render = { version = "0.9", features = ["pdfium_use_win32"] }
   tauri = { version = "2", features = [] }
   serde = { version = "1", features = ["derive"] }
   serde_json = "1"
   ```

3. **Configure resource bundling in `tauri.conf.json`:**
   ```json
   "bundle": {
     "resources": ["resources/pdfium.dll"]
   }
   ```

4. **Initialize pdfium on app startup** (`lib.rs`):
   ```rust
   use pdfium_render::prelude::*;
   use std::sync::Mutex;

   struct PdfiumState {
       pdfium: Pdfium,
   }

   // In run():
   let pdfium_path = /* resolve path to pdfium.dll */;
   let bindings = Pdfium::bind_to_library(pdfium_path).expect("Failed to load pdfium.dll");
   let pdfium = Pdfium::new(bindings);
   app.manage(Mutex::new(PdfiumState { pdfium }));
   ```

   For `dev` mode, the resource path differs from the bundled path. Use `tauri::App::path()` API to resolve correctly in both cases. Test both `npm run tauri dev` and `cargo tauri build`.

5. **Create a document manager** — a `HashMap<String, PdfDocument>` in Tauri managed state, keyed by UUID. Each entry holds a pdfium document handle.

   Note on thread safety: `pdfium-render` documents are not `Send`. You may need to keep all pdfium operations on a single dedicated thread, or use `Mutex` wrapping. Research `pdfium-render`'s thread safety model at implementation time. If documents aren't `Send`, all pdfium calls must go through a single-threaded executor (similar to the STA thread for printing).

6. **Implement `open_document` command:**
   ```rust
   #[tauri::command]
   fn open_document(state: tauri::State<DocManager>, path: String) -> Result<DocInfo, String> {
       // Load PDF from path via pdfium
       // Store in HashMap with UUID key
       // Return { docId, pageCount, pageDimensions: [{width, height}, ...] }
   }
   ```

7. **Implement `render_page` command:**
   ```rust
   #[tauri::command]
   fn render_page(state: tauri::State<DocManager>, doc_id: String, page: u32, width: u32, height: u32) -> Result<Vec<u8>, String> {
       // Render page to BGRA bitmap at requested dimensions
       // Return raw bytes
   }
   ```

   Research the exact binary return mechanism for Tauri v2 at implementation time. Options:
   - Return `Vec<u8>` (Tauri may base64-encode this in JSON — slow for large bitmaps)
   - Return `tauri::ipc::Response` with raw bytes
   - Use Tauri's binary channel/event system
   
   The bitmap for a single page at 2x DPI can be 4-8 MB. Base64 encoding doubles that. Find the zero-copy or binary transfer path.

8. **Display on the frontend:**
   ```typescript
   // Fetch BGRA bytes from Rust
   const bytes = await invoke<ArrayBuffer>("render_page", { docId, page: 1, width, height });
   
   // BGRA → ImageData → Canvas
   // Note: Canvas expects RGBA, pdfium outputs BGRA. Swap B and R channels,
   // or use ImageBitmap with {premultiplyAlpha, colorSpaceConversion} options.
   const imageData = new ImageData(new Uint8ClampedArray(bgraToRgba(bytes)), width, height);
   ctx.putImageData(imageData, 0, 0);
   ```

   The BGRA→RGBA conversion is a tight loop over the byte array, swapping bytes 0 and 2 in each 4-byte pixel. This is fast even in JS for single-page bitmaps. For production, consider doing the swap in Rust before sending.

### Verification
- Open a PDF via a file picker. One page displays on a canvas.
- The page is sharp at the displayed size (no blurriness from DPI mismatch).
- Try a 1-page PDF, a 100-page PDF (to verify loading doesn't crash), and a PDF with images.

---

## Phase 3: Viewer Core

### Goal
Continuous vertical scroll viewer with virtual rendering, zoom, and keyboard navigation.

### Steps

1. **Page dimensions:** The `open_document` command returns page dimensions (from pdfium) for all pages at scale=1. The frontend stores these per-tab and uses them to calculate total document height, page positions, and placeholder sizes.

2. **ContinuousViewer component:**
   - Renders a scrollable `<div>` containing one `<div class="page-slot">` per page.
   - Each page-slot has explicit width/height from `pageDimensions[i] * zoom * dpr`.
   - Only page-slots within `RENDER_RADIUS` (2) of the current page contain a `<canvas>`. Others are empty placeholders.

3. **PageSlot component:**
   - When entering the render window: calls `invoke("render_page", ...)`, receives BGRA bytes, converts to ImageBitmap, draws on canvas.
   - When exiting the render window: clears canvas, releases ImageBitmap.
   - Cache: LRU cache (20 entries) keyed by `docId:page:zoom:dpr`. Check cache before invoking Rust.

4. **IntersectionObserver:** Attach to each page-slot to detect which page is most visible. Update `currentPage` in the store. Thresholds: `[0, 0.25, 0.5, 0.75, 1.0]`.

5. **Jump to page:** `requestJumpToPage(n)` calculates scroll offset from page dimensions and calls `scrollTo({ top, behavior: 'smooth' })`.

6. **Zoom:**
   - Presets: 10%, 25%, 50%, 75%, 100%, 125%, 150%, 200%, 300%, 400%.
   - Fit-Width: `viewportWidth / pageWidth`. Fit-Page: `min(viewportWidth / pageWidth, viewportHeight / pageHeight)`. Both recomputed on resize via ResizeObserver.
   - Ctrl+Scroll wheel: zoom by +-12% per tick.
   - On zoom change: preserve anchor page position (compute which page is centered, apply new zoom, scroll to keep that page centered).

7. **Toolbar:** Open button, page nav (prev/next/input), zoom controls (in/out/dropdown).

8. **Keyboard shortcuts:** Ctrl+O (open), Page Up/Down (navigate), Ctrl+Scroll (zoom).

### Verification
- Open a 50-page PDF. Scroll smoothly. Only ~5 canvases are rendered at any time.
- Zoom in/out. Pages re-render at new resolution. Scroll position preserved.
- Fit-Width and Fit-Page modes work. Resize the window — fit modes update.
- Page Up/Down navigates. Page input field + Enter jumps.

---

## Phase 4: Text Layer and Search

### Goal
Selectable text overlay on rendered pages, and full-text search with highlighting.

### Steps

1. **Implement `extract_text` command in Rust:**
   ```rust
   #[tauri::command]
   fn extract_page_text(state: ..., doc_id: String, page: u32) -> Result<Vec<TextItem>, String> {
       // Use pdfium text extraction API
       // Return: [{text, x, y, width, height, fontSize}, ...]
       // Coordinates in PDF points (1/72 inch), relative to page origin
   }
   ```

2. **Text layer component:** Transparent `<div>` overlaid on each rendered canvas. Contains absolutely-positioned `<span>` elements for each text item, styled with `color: transparent; user-select: text`. Positions and sizes derived from the text extraction coordinates, scaled by zoom.

3. **Implement `search_document` command in Rust:**
   ```rust
   #[tauri::command]
   fn search_document(state: ..., doc_id: String, query: String) -> Result<Vec<SearchResult>, String> {
       // Search all pages for case-insensitive substring matches
       // Return: [{page, rects: [{x, y, width, height}, ...]}, ...]
       // rects are in PDF points for highlight positioning
   }
   ```

   Consider making this a streaming/progressive search that emits Tauri events per page, so the UI can show partial results for large documents.

4. **Highlight layer:** Sibling `<div>` of the text layer. Contains absolutely-positioned yellow rectangles for each search match, derived from the search result coordinates.

5. **Search state in Zustand:** `searchQuery`, `searchResults`, `searchResultIndex`, `searchFocusToken` per tab.

### Verification
- Select text on a rendered page. Ctrl+C copies it.
- Search for a word. Yellow highlights appear on matching pages.
- Navigate between results. Page jumps to each match.
- Search a 100-page PDF — results appear progressively, UI doesn't freeze.

---

## Phase 5: Sidebar Panels

### Goal
Icon rail with toggle buttons, resizable sidebar with three panels (Thumbnails, Search, Metadata placeholder).

### Steps

1. **Icon rail:** Vertical strip of 30x30 buttons on the far left. Three tools: Thumbnails, Search, Metadata. Click active tool to collapse sidebar; click inactive tool to switch.

2. **Sidebar container:** Resizable panel (150-500px) with a drag handle on the right edge. Width persisted to localStorage. Throttle CSS updates during drag via `requestAnimationFrame`.

3. **Thumbnail panel:** Grid of page thumbnails. Each is rendered by calling `render_page` at 18% scale. Click to jump. Active page has accent border.

4. **Search panel:** Search input + paginated result list + Prev/Next navigation. Wired to the search commands from Phase 4. Enter/Shift+Enter for navigation. Ctrl+F to focus.

5. **Metadata panel:** Read-only display for now (Phase 7 adds editing). Show title, author, subject, etc. from a `get_metadata` Rust command.

### Grid layout for the app shell:
```css
.app-shell {
  display: grid;
  grid-template-rows: var(--toolbar-h) var(--tabbar-h) 1fr;
  grid-template-columns: var(--rail-w) var(--panel-w) 1fr;
  grid-template-areas:
    "toolbar toolbar toolbar"
    "tabbar  tabbar  tabbar"
    "rail    sidebar viewer";
}
```

### Verification
- Click icon rail buttons to toggle sidebar panels.
- Resize sidebar. Width persists across page reload.
- Thumbnails render and navigate on click.
- Search panel finds text and highlights results.

---

## Phase 6: Printing

### Goal
Native Windows printing via PrintDlgExW + pdfium GDI rendering. All DEVMODE settings honored.

### This is the most complex phase. Read section 2 (Hard-Won Lessons) carefully before starting.

### Steps

1. **Add windows crate features** (see section 3.4).

2. **Implement `print_document` Tauri command:**
   This command does everything — shows the dialog, renders to the printer, reports results.

   ```rust
   #[tauri::command]
   async fn print_document(
       window: tauri::WebviewWindow,
       state: tauri::State<'_, DocManager>,
       doc_id: String,
   ) -> Result<PrintResult, String> {
       let hwnd = window.hwnd()?.0 as isize;
       let page_count = /* get from doc manager */;
       
       // Spawn STA thread for PrintDlgExW
       let (tx, rx) = std::sync::mpsc::channel();
       std::thread::spawn(move || {
           // CoInitializeEx(COINIT_APARTMENTTHREADED)
           // PrintDlgExW with hwndOwner
           // Extract DEVNAMES, DEVMODE
           // If not cancelled:
           //   CreateDC with DEVMODE
           //   StartDoc
           //   For each page:
           //     StartPage
           //     FPDF_RenderPage(hdc, page, ...)
           //     EndPage
           //     // Emit progress event
           //   EndDoc
           //   DeleteDC
           // Free DEVNAMES, DEVMODE
           // CoUninitialize
           tx.send(result).ok();
       });
       rx.recv().map_err(|e| format!("{e}"))?
   }
   ```

3. **Page scaling for printing:** The print HDC has its own resolution (e.g., 600 DPI). Get the printable area with `GetDeviceCaps(hdc, HORZRES)` / `VERTRES` (in device pixels) and `PHYSICALWIDTH` / `PHYSICALHEIGHT`. Scale the PDF page to fit the printable area while maintaining aspect ratio.

4. **FPDF_RenderPage call:**
   ```rust
   // Access the raw binding through pdfium-render
   pdfium.bindings().FPDF_RenderPage(
       hdc.0 as *mut c_void,  // HDC as raw pointer
       page_handle,            // FPDF_PAGE handle
       start_x,               // left margin in device coords
       start_y,               // top margin in device coords
       size_x,                // width in device coords
       size_y,                // height in device coords
       0,                     // rotation (0 = normal)
       FPDF_PRINTING,         // render flags for print quality
   );
   ```

   The `FPDF_PRINTING` flag tells pdfium to optimize for print (e.g., no screen-oriented anti-aliasing).

5. **Progress events:** From the print loop, emit Tauri events:
   ```rust
   window.emit("print-progress", PrintProgress { page: i, total: page_count })?;
   ```
   The frontend listens and shows "Printing page 3 of 12..."

6. **Cancellation:** The frontend sends a cancel signal (e.g., sets an `AtomicBool` in shared state). The print loop checks this before each page and calls `AbortDoc(hdc)` if cancelled.

7. **Frontend:** Print button in toolbar + Ctrl+P shortcut. Calls `invoke("print_document", { docId })`. Listens for progress events. Shows cancel UI.

### Thread safety note for pdfium

The print loop needs access to the pdfium document to call `FPDF_RenderPage`. If the document is behind a `Mutex` in the doc manager, the STA thread must acquire the lock. This means rendering is blocked during printing — acceptable for now, since the user isn't interacting with the viewer during a print job.

If pdfium objects are not `Send`, you may need to load a separate copy of the PDF on the STA print thread:
```rust
// On the STA thread:
let pdfium = Pdfium::bind_to_library(pdfium_path)?;
let doc = pdfium.load_pdf_from_file(&pdf_path, None)?;
// Render pages from this independent document instance
```

This avoids threading issues entirely at the cost of loading the PDF twice.

### Verification
- Ctrl+P opens the Windows Print Dialog.
- Select a printer, set page range, copies, duplex, orientation.
- Click Print. Pages print at full printer DPI. Text is vector-sharp.
- All DEVMODE settings are honored (test duplex if you have a duplex printer).
- Cancel mid-print works.
- Print a 1-page, 10-page, and 50-page PDF.
- Verify that printing works when Tumbler IS the default PDF application (no external app dependency).

---

## Phase 7: Metadata Editing

### Goal
Read metadata via pdfium, write via lopdf.

### Steps

1. **Add lopdf to Cargo.toml:**
   ```toml
   lopdf = "0.34"
   ```

2. **Implement `get_metadata` command:**
   Use pdfium's `FPDF_GetMetaText` to read: Title, Author, Subject, Keywords, Creator, Producer, CreationDate, ModDate.

3. **Implement `set_metadata` command:**
   ```rust
   #[tauri::command]
   fn set_metadata(state: ..., doc_id: String, metadata: MetadataUpdate) -> Result<(), String> {
       // Get the current PDF bytes from the doc manager
       // Load into lopdf::Document
       // Modify the info dictionary
       // Save back to bytes
       // Reload into pdfium (close old handle, load new one, same docId)
   }
   ```

4. **Frontend MetadataPanel:** Editable text inputs for Title, Author, Subject, Keywords, Creator. Read-only display for Producer, Created, Modified. Save button appears when dirty.

### Verification
- Open a PDF. Metadata fields display correctly.
- Edit Title and Author. Click Save. Close and reopen the file — changes persist.
- Tab shows dirty indicator when metadata is modified.
- Closing a dirty tab warns before discarding.

---

## Phase 8: Multi-Document Tabs

### Goal
Open multiple PDFs in tabs with independent state.

### Steps

1. **Tab state in Zustand:** `tabs: TabState[]`, `activeTabId: string`. Each `TabState` holds `docId` (not pdfium objects — those are in Rust), `fileName`, `pageCount`, `pageDimensions`, `currentPage`, `zoom`, `zoomMode`, `darkMode`, `scrollTop`, `searchQuery`, `searchResults`, etc.

2. **TabBar component:** Horizontal strip of tab chips. File name (ellipsized), dirty indicator, close button. Active tab underlined with accent color.

3. **Drag-to-reorder:** Track drag state. On drop, compute insertion index from mouse position. Update `tabs` array order in store.

4. **Tab lifecycle:**
   - Open: Create new tab, call `open_document` in Rust, store returned `docId`.
   - Switch: Save scroll position of current tab, restore scroll position of target tab.
   - Close: Call `close_document(docId)` in Rust to release pdfium resources. Remove from store. Auto-switch to adjacent tab.

5. **Rust `close_document` command:** Remove document from the HashMap, drop the pdfium handle.

### Verification
- Open 3 PDFs in separate tabs. Switch between them — each retains its own scroll position, zoom, search state.
- Drag tabs to reorder.
- Close a tab — resources freed, adjacent tab activated.
- Close all tabs — empty state shown.

---

## Phase 9: Polish and OS Integration

### Goal
Theming, accent color, file association, display modes, error handling, installer.

### Steps

1. **Windows accent color:** Rust command reads accent color via `UISettings` COM API (already proven in prior implementation — see `read_windows_accent()` in the original `lib.rs`). Frontend applies to `--color-accent` CSS variable.

2. **Dark/Light mode:** CSS custom properties with `prefers-color-scheme` media query. All components use variables, never hardcoded colors.

3. **High contrast mode:** `prefers-contrast: more` media query maps all colors to system colors (Canvas, ButtonText, Highlight, etc.).

4. **Display modes** (per-tab): Normal, Invert (CSS `filter: invert(1) hue-rotate(180deg)` on canvases), Sepia (CSS `filter: sepia(0.6) brightness(0.9)`).

5. **File association handling:** On startup, check `std::env::args()` for a PDF path. If present, open it in a new tab. Configure NSIS/MSI installer to register `.pdf` association.

6. **Copy-pasteable error dialogs:** Do NOT use Tauri's native `message()` dialog for errors — its text cannot be selected or copied. Instead, build a React modal with selectable text. This was a specific user complaint.

7. **Dev tools:** Open devtools automatically in debug builds:
   ```rust
   #[cfg(debug_assertions)]
   {
       let window = app.get_webview_window("main").unwrap();
       window.open_devtools();
   }
   ```

8. **Remove debug logging:** Do not write args or debug info to temp files in release builds.

### Verification
- App respects system light/dark mode.
- Accent color matches Windows settings.
- High contrast mode is usable.
- Double-clicking a .pdf in Explorer opens it in Tumbler.
- Error messages can be selected and copied.
- Display mode cycling works per-tab.

---

## Future Phases

These are documented in the requirements but not part of the initial build:

- **Phase 10: Document Operations** — Merge, split, add, delete, reorder, rotate, crop pages. Uses pdfium (`FPDF_ImportPages`, `FPDFPage_Delete`, `FPDFPage_SetRotation`) and lopdf (CropBox).
- **Phase 11: Form Filling** — Enumerate form fields via pdfium's form API, render interactive overlays, save filled forms.
- **Phase 12: Text Extraction** — Export plain text from all pages to `.txt` file via pdfium text API.
- **Phase 13: Advanced Printing** — Print to PDF, watermark/stamp, booklet imposition.

---

## Appendix A: File Structure (Target)

```
tumbler/
  docs/
    requirements.md
    implementation-plan.md
  src/
    components/
      Toolbar.tsx
      TabBar.tsx
      IconRail.tsx
      Sidebar.tsx
      ContinuousViewer.tsx
      PageSlot.tsx
      SearchPanel.tsx
      ThumbnailPanel.tsx
      MetadataPanel.tsx
      ViewerArea.tsx
      ErrorDialog.tsx          # Copy-pasteable error modal
    store/
      usePdfStore.ts
    utils/
      zoomConstants.ts
      viewerConstants.ts
      systemTheme.ts
      bgraConvert.ts           # BGRA→RGBA byte conversion
    styles/
      global.css
    App.tsx
    main.tsx
  src-tauri/
    src/
      lib.rs                   # Tauri setup, command registration
      commands/
        document.rs            # open, close, save
        render.rs              # render_page
        text.rs                # extract_text, search
        print.rs               # print_document (STA thread, GDI)
        metadata.rs            # get/set metadata
      state.rs                 # DocManager, PdfiumState
    resources/
      pdfium.dll               # Bundled pdfium binary
    Cargo.toml
    tauri.conf.json
  package.json
  vite.config.ts
  tsconfig.json
```

## Appendix B: Key API Patterns

### Tauri Command with Window Handle
```rust
#[tauri::command]
async fn my_command(window: tauri::WebviewWindow) -> Result<String, String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
    // hwnd is now usable as a Win32 HWND
    Ok("done".into())
}
```

### Managed State Access
```rust
#[tauri::command]
fn my_command(state: tauri::State<'_, Mutex<MyState>>) -> Result<String, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    // Use guard...
    Ok("done".into())
}
```

### Registering Commands
```rust
tauri::Builder::default()
    .manage(Mutex::new(my_state))
    .invoke_handler(tauri::generate_handler![
        cmd_a, cmd_b, cmd_c,
    ])
    .run(tauri::generate_context!())
```

### Frontend invoke
```typescript
import { invoke } from "@tauri-apps/api/core";

const result = await invoke<DocInfo>("open_document", { path: "/some/file.pdf" });
```

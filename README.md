<div align="center">
  <img src="tumbler.png" alt="Tumbler icon" width="128" height="128">

# Tumbler

[![CI](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml/badge.svg)](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml)

A personal PDF viewer for Windows. 

Built with Tauri v2
(Rust backend, React/TypeScript frontend) and pdfium.

</div>

## Features

- Continuous-scroll page viewer with smooth zoom (presets, +/-, and
  Ctrl+scroll)
- Native Windows printing at printer-native resolution, with in-progress cancellation
- Text layer with copy-to-clipboard and full-document search
- OCR for scanned pages — make image-only pages searchable and selectable
- Export all page text to a `.txt` file
- Thumbnail sidebar for quick page navigation
- Document metadata viewing and editing
- Page operations: delete, rotate, reorder (drag-and-drop), merge, and split pages
- Multiple documents open in draggable, reorderable tabs
- Display modes: normal, inverted, and sepia


## UI

### Search

Click the **magnifying-glass icon** in the left rail to open the search panel. Type a query — search runs as you type (300 ms debounce) across the whole document, listing every page that has a hit with its match count and jumping to the first result. Step through matches with **Enter / Shift+Enter** (or the up/down arrows); each match is highlighted on the page.

**OCR for scans:** search reads the PDF's text layer, so a scanned, image-only page has nothing to match. When a search returns no results, an inline prompt appears for the page you're currently viewing:

> Page *N* may be a scan with no text.
> **[ Run OCR on this page ]**

Click it to recognize text on that page — roughly 1–3 seconds (the page is rendered at 300 DPI and handed to the Windows OCR engine). The search then re-runs automatically, so any matches now show up in the results, and the recognized words also become selectable/copyable in the text layer.

Results are cached for the rest of the session, so re-searching that page is instant and the prompt won't reappear for a page you've already processed. Nothing is written to the PDF file — this OCR text lives only inside Tumbler. (To bake recognized text into the exported file, see **Export Text** below.)

**Requirements:** OCR uses the engine built into Windows 10/11, which needs a language pack installed. If none is available you'll see:

> OCR failed: OCR is not available — install an OCR language pack in Windows Settings → Time & Language → Language.

Add one under **Settings → Time & Language → Language → (your language) → Language options → Optional features**.

### Export Text

Click the **scroll icon** in the toolbar (left of the print button) to export the document's text layer to a `.txt` file.

A save dialog opens defaulting to the same folder as the source PDF. Each page is written with a `--- Page N ---` header.

**OCR for scanned pages:** if the document has pages with no text layer (likely scans), after you choose the destination Tumbler asks whether to run OCR on those pages so their recognized text is included in the export. (OCR takes ~1–3s per page, so a progress overlay with a **Cancel** button appears while it runs.) Pages where OCR still finds nothing — and all text-less pages when you decline OCR — get a `[no extractable text]` placeholder so every page is accounted for. OCR results are also cached, so search and copy light up for those pages afterward. A confirmation shows the number of pages exported (and how many came from OCR) when done.

### Page operations

Click the **pocket-knife icon** in the left rail to open the Pages panel.

- **Navigate** — Click any thumbnail to jump to that page.
- **Select pages** — Click a checkbox on any thumbnail to toggle selection. Use **Select All / Deselect All** in the action bar to bulk-select. The trash and rotate actions are enabled only when at least one page is selected. (Reorder is drag-based and needs no selection.)
- **Delete** — Select one or more pages and click the trash icon. The last remaining page cannot be deleted.
- **Rotate** — Select pages and click the rotate-clockwise or rotate-counter-clockwise icon to spin them 90°. Each click adds another 90°.
- **Merge** — Click the import icon to pick a PDF file. Its pages are appended after the last page of the current document.
- **Split** — Click the scissors icon in the action bar, enter a **first** and **last** page number in the fields that appear, then click **Save…** to choose where the extracted pages are written. The original document is not modified.
- **Reorder** — Grab the grip handle on the left of any thumbnail and drag it to a new position. The document is saved in the new order.

All operations save the document immediately and reload every open tab that shares the same file.

## Tech stack

| Layer | Technology |
|---|---|
| Shell | Tauri v2 |
| Frontend | React 18 + TypeScript, Vite, Zustand |
| PDF engine | [pdfium](https://pdfium.googlesource.com/pdfium/) via `pdfium-render`, plus `lopdf` for metadata/CropBox edits |
| Printing/theming | `windows` crate (GDI, `PrintDlgExW`, `UISettings`) |
| Testing | Vitest + jsdom (frontend), `cargo test` (backend) |

## Project structure

```
rust-tumbler/
├── src/                          # React frontend
│   ├── components/
│   │   ├── Toolbar.tsx           # Navigation, zoom, print, export text, display mode
│   │   ├── TabBar.tsx            # Multi-document tabs
│   │   ├── IconRail.tsx          # Sidebar tool switcher
│   │   ├── Sidebar.tsx           # Tab container for panels
│   │   ├── ViewerArea.tsx        # Viewer container
│   │   ├── ContinuousViewer.tsx  # Scrollable page list
│   │   ├── PageSlot.tsx          # Per-page render + canvas
│   │   ├── TextLayer.tsx         # Selectable/copyable text overlay
│   │   ├── HighlightLayer.tsx    # Search-result highlighting
│   │   ├── ThumbnailPanel.tsx    # Page thumbnail strip
│   │   ├── SearchPanel.tsx       # Full-text search UI
│   │   ├── MetadataPanel.tsx     # Document info editor
│   │   └── PagesPanel.tsx        # Page operations (delete/rotate/reorder/merge/split)
│   ├── store/
│   │   └── usePdfStore.ts        # Zustand global state (tabs, zoom, etc.)
│   ├── utils/                    # Bitmap conversion, render cache, etc.
│   ├── styles/
│   │   └── global.css            # Design tokens and layout
│   ├── App.tsx
│   └── main.tsx
├── src-tauri/                    # Rust/Tauri backend
│   ├── src/
│   │   ├── commands/
│   │   │   ├── document.rs       # open/close document
│   │   │   ├── render.rs         # page rendering
│   │   │   ├── text.rs           # text extraction + search (with OCR fallback)
│   │   │   ├── ocr.rs            # OCR via Windows.Media.Ocr (Make Searchable)
│   │   │   ├── metadata.rs       # metadata read/write (lopdf)
│   │   │   ├── pages.rs          # page operations (delete/rotate/reorder/merge/split)
│   │   │   ├── print.rs          # native printing (GDI)
│   │   │   ├── theme.rs          # Windows accent color
│   │   │   └── startup.rs        # file-association startup path
│   │   ├── state.rs              # AppState, document map
│   │   ├── error.rs               # AppError
│   │   ├── lib.rs
│   │   └── main.rs
│   ├── tauri.conf.json
│   └── Cargo.toml
├── .github/
│   └── workflows/
│       └── ci.yml                # Frontend tests + cargo check
├── index.html
├── vite.config.ts
└── package.json
```

## Getting started

### Prerequisites

- Node.js 20+
- Rust (stable) with the Tauri v2 prerequisites for Windows
- A win-x64 `pdfium.dll` (e.g. from the
  [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) releases),
  placed at `src-tauri/resources/pdfium.dll` (not checked into the repo)

## Future

Planned enhancements:

- **Form Filling** — Enumerate form fields via pdfium's form API, render
  interactive overlays, and save filled forms.
- **OCR — Save Searchable Copy** — Persist recognized text as an invisible
  layer so the OCR'd document is searchable in any PDF reader (the in-app
  ephemeral OCR above already ships).
- **Web Optimization** - Compress, Linearize
 


### Setup

```sh
npm install
```

### Run in development

```sh
npm run tauri dev
```

### Build

```sh
npm run tauri build
```

Installers are written to `src-tauri/target/release/bundle/`:
- NSIS: `nsis/Tumbler_<version>_x64-setup.exe`
- MSI: `msi/Tumbler_<version>_x64_en-US.msi`

### Test

```sh
npm test           # frontend (Vitest)
cargo test         # backend (from src-tauri/)
```

## Updating the app version

Version is set in three places — keep them in sync:

- `package.json` → `"version"`
- `src-tauri/tauri.conf.json` → `"version"`
- `src-tauri/Cargo.toml` → `version`


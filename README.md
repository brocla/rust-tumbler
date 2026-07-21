<div align="center">
  <img src="tumbler.png" alt="Tumbler icon" width="128" height="128">

# Tumbler



## A PDF toolbox for Windows. 

Built with Tauri v2
(Rust backend, React/TypeScript frontend) and pdfium. 

[![CI](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml/badge.svg)](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/brocla/rust-tumbler?sort=semver)](https://github.com/brocla/rust-tumbler/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
![Rust Edition](https://img.shields.io/badge/Rust-Edition%202021-orange)
<!-- [![dependency status](https://deps.rs/repo/github/brocla/rust-tumbler/status.svg?path=src-tauri)](https://deps.rs/repo/github/brocla/rust-tumbler?path=src-tauri) -->




</div>

## Features

- Page operations: delete, rotate, reorder (drag-and-drop), merge, and split pages
- Text layer with copy-to-clipboard and full-document search
- OCR for scanned pages — make image-only pages searchable, selectable, copyable, and savable
- Typewriter — type text anywhere on the page (fill ad-hoc forms with underline blanks)
- Extract text to a file
- View and Edit metadata
- Native Windows printing
- Form Filling
- File compression
- Open password-protected files; add, change, or remove a password (AES-256)
- Detect ISO Standard
- Verify Digital Signatures
- Web Optimization - Linearize
- Redaction


## Futures

Planned enhancements:

- **CLI**

 

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

### Make Searchable (whole document)

The per-page prompt above handles one scan at a time. To OCR an entire scanned document at once, click the **Make Text Searchable** button (scan-with-magnifier icon) in the toolbar, left of the Export Text button. Tumbler first checks how many pages lack a text layer; if there are none it says so and stops, otherwise it OCRs every text-less page, showing the same **OCR page X of Y** progress overlay with a **Cancel** button.

Once a document has been made searchable, Export Text reuses those cached results — so it won't prompt you to run OCR again, and the exported `.txt` includes the recognized text automatically.

When it finishes, those pages are searchable, and their text is selectable and copyable directly on the page — drag to select, then Ctrl+C (copied text preserves line breaks). As with per-page OCR this is in-app only and cached for the session; nothing is written to the PDF until you use Export Text or (in a future release) save a searchable copy.

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

### Typewriter

Click the **type icon** in the left rail to open the Typewriter panel. This is for filling ad-hoc forms that use plain underline blanks instead of real form fields.

Opening the panel arms the tool (leaving it disarms). Click anywhere on the page to drop a text box and start typing; drag the box's handle to move it or its corner to resize. Choose the **font** (Helvetica, Times, or Courier), **size**, **color**, and **bold/italic** in the panel — the controls apply to the note you're editing. Click away to finish, or double-click a note to edit it again.

Notes are added as a buffer edit — nothing is written until you **Save / Save As** (or discard by closing without saving). Each note is stored as a standard PDF text annotation, so it prints and opens correctly in other readers, and its text is selectable and searchable in Tumbler.

> Note: so your typed notes show as a single clean layer, Tumbler's viewer does not paint annotation *markup* authored in other tools (highlights, sticky notes, stamps). That markup still prints and appears in other PDF readers.

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
│   │   ├── Toolbar.tsx           # Navigation, zoom, print, make searchable, export text, display mode
│   │   ├── TabBar.tsx            # Multi-document tabs
│   │   ├── IconRail.tsx          # Sidebar tool switcher
│   │   ├── Sidebar.tsx           # Tab container for panels
│   │   ├── ViewerArea.tsx        # Viewer container
│   │   ├── ContinuousViewer.tsx  # Scrollable page list
│   │   ├── PageSlot.tsx          # Per-page render + canvas
│   │   ├── TextLayer.tsx         # Selectable/copyable text overlay
│   │   ├── HighlightLayer.tsx    # Search-result highlighting
│   │   ├── FormLayer.tsx         # Interactive AcroForm controls
│   │   ├── RedactLayer.tsx       # Redaction region overlay
│   │   ├── TypewriterLayer.tsx   # Typewriter note overlay (editable)
│   │   ├── ThumbnailPanel.tsx    # Page thumbnail strip
│   │   ├── SearchPanel.tsx       # Full-text search UI
│   │   ├── MetadataPanel.tsx     # Document info editor
│   │   ├── PagesPanel.tsx        # Page operations (delete/rotate/reorder/merge/split)
│   │   ├── OptimizePanel.tsx     # Compression + Web Optimization
│   │   ├── RedactPanel.tsx       # Redaction find/apply/save
│   │   ├── TypewriterPanel.tsx   # Typewriter font/size/color controls
│   │   └── StatusBar.tsx         # Page/zoom, signature & format badges
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
│   │   │   ├── encryption.rs     # decrypt-on-open, re-encrypt on save, set/remove password
│   │   │   ├── render.rs         # page rendering
│   │   │   ├── text.rs           # text extraction + search (with OCR fallback)
│   │   │   ├── ocr.rs            # OCR via Windows.Media.Ocr (Make Searchable)
│   │   │   ├── text_layer.rs     # embed an invisible OCR text layer
│   │   │   ├── metadata.rs       # metadata read/write (lopdf)
│   │   │   ├── pages.rs          # page operations (delete/rotate/reorder/merge/split)
│   │   │   ├── forms.rs          # AcroForm field discovery + value writes
│   │   │   ├── typewriter.rs     # free-text "typewriter" notes (FreeText annotations)
│   │   │   ├── redact.rs         # redaction (flatten + verify)
│   │   │   ├── optimize.rs       # compression pipeline
│   │   │   ├── linearize.rs      # Web Optimization (qpdf)
│   │   │   ├── signature.rs      # digital-signature verification
│   │   │   ├── conformance.rs    # ISO sub-format detection (PDF/A, /X, …)
│   │   │   ├── save.rs           # Save / Save As (only disk writers)
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
- A win-x64 qpdf DLL (used by "Save Web-Optimized Copy"): download the
  `qpdf-<version>-msvc64.zip` asset from the
  [qpdf releases](https://github.com/qpdf/qpdf/releases), take
  `bin/qpdf30.dll`, and place it at `src-tauri/resources/qpdf.dll`
  (not checked into the repo). The MSVC build depends at runtime on
  `msvcp140.dll`, `vcruntime140.dll`, and `vcruntime140_1.dll` from the
  same zip's `bin/` folder — copy those three alongside `qpdf.dll` in
  `src-tauri/resources/` too, so the app doesn't rely on the target
  machine having the Visual C++ redistributable installed




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

Run `npm version <patch|minor|major>` on `main` after a PR merges. A sync
script propagates the new version to all three files and creates the
`vX.Y.Z` tag:

- `package.json` → `"version"`
- `src-tauri/tauri.conf.json` → `"version"`
- `src-tauri/Cargo.toml` → `version`

Push with `git push --follow-tags` — pushing the tag triggers the Release
workflow, which builds the Windows installers and publishes a GitHub Release.

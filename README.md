<div align="center">
  <img src="tumbler.png" alt="Tumbler icon" width="128" height="128">

# Tumbler

[![CI](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml/badge.svg)](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml)

A personal PDF viewer for Windows, built with Tauri v2
(Rust backend, React/TypeScript frontend) and pdfium.

</div>

## Features

- Continuous-scroll page viewer with smooth zoom (presets, +/-, and
  Ctrl+scroll)
- Text layer with copy-to-clipboard and full-document search
- Thumbnail sidebar for quick page navigation
- Document metadata viewing and editing
- Native Windows printing at printer-native resolution
- Multiple documents open in draggable, reorderable tabs
- Display modes: normal, inverted, and sepia


## Tech stack

| Layer | Technology |
|---|---|
| Shell | Tauri v2 |
| Frontend | React 18 + TypeScript, Vite, Zustand |
| PDF engine | [pdfium](https://pdfium.googlesource.com/pdfium/) via `pdfium-render`, plus `lopdf` for metadata/CropBox edits |
| Printing/theming | `windows` crate (GDI, `PrintDlgExW`, `UISettings`) |
| Testing | Vitest + jsdom (frontend), `cargo test` (backend) |

## Getting started

### Prerequisites

- Node.js 20+
- Rust (stable) with the Tauri v2 prerequisites for Windows
- A win-x64 `pdfium.dll` (e.g. from the
  [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) releases),
  placed at `src-tauri/resources/pdfium.dll` (not checked into the repo)

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

### Test

```sh
npm test           # frontend (Vitest)
cargo test         # backend (from src-tauri/)
```

## Future

Planned enhancements:

- **Document Operations** — Merge, split, add, delete, reorder, rotate, and
  crop pages. Uses pdfium (`FPDF_ImportPages`, `FPDFPage_Delete`,
  `FPDFPage_SetRotation`) and lopdf (CropBox).
- **Form Filling** — Enumerate form fields via pdfium's form API, render
  interactive overlays, and save filled forms.
- **Text Extraction** — Export plain text from all pages to a `.txt` file via
  pdfium's text API.

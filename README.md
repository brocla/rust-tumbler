<div align="center">
  <img src="tumbler.png" alt="Tumbler icon" width="128" height="128">

# Tumbler

[![CI](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml/badge.svg)](https://github.com/brocla/rust-tumbler/actions/workflows/ci.yml)

A personal PDF viewer, editor, and printer for Windows, built with Tauri v2
(Rust backend, React/TypeScript frontend) and pdfium.

**Work in progress.**

</div>

## Features

- Continuous-scroll page viewer with smooth zoom (presets, +/-, and
  Ctrl+scroll)
- Text layer with copy-to-clipboard and full-document search with
  highlighting
- Thumbnail sidebar for quick page navigation
- Document metadata viewing and editing
- Native Windows printing at printer-native resolution via pdfium + GDI
- Multiple documents open in draggable, reorderable tabs
- Display modes: normal, inverted, and sepia
- Windows accent-color theming and PDF file-association support
  (double-click a `.pdf` to open in Tumbler)

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

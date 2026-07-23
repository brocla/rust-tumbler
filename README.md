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

Tumbler is a PDF viewer and toolbox. It is born from my frustration with apps that suddenly want money to do the next little bit. They almost get the job done, but not quite. 

This app has lots of tools for manipulating PDFs. Almost everything I could think of, short of a full editor. It never asks for money. 

This is the new world of AI. Anyone can make their own software, just the way they like it. That is what this is; my PDF toolbox, just the way I like it. That is why you will not find a Settings menu.



## Features

- Page operations: delete, rotate, reorder (drag-and-drop), merge, and split pages
- Text layer with copy-to-clipboard and full-document search
- Expand Margins — enlarge the page content to fill the margins. Sheet music is
  often engraved small inside wide borders; this scales every page up by one
  uniform factor so the notes get bigger without the layout shifting
- OCR — make image-only pages searchable, selectable, copyable, and savable
- Form Filling
- Typewriter — type text anywhere on the page 
- Extract text to a file
- View and Edit metadata
- Native Windows printing
- File compression
- Open password-protected files; add, change, or remove a password (AES-256)
- Detect ISO Standards
- Verify Digital Signatures
- Web Optimization - Linearize
- Redaction


## Tech stack

| Layer | Technology |
|---|---|
| Shell | Tauri v2 |
| Frontend | React 18 + TypeScript, Vite, Zustand |
| PDF engine | [pdfium](https://pdfium.googlesource.com/pdfium/) via `pdfium-render`, plus `lopdf` for metadata/CropBox edits |
| Printing/theming | `windows` crate (GDI, `PrintDlgExW`, `UISettings`) |
| Testing | Vitest + jsdom (frontend), `cargo test` (backend) |

## Getting started

### Install (prebuilt)

The quickest way to run Tumbler is to grab a prebuilt Windows installer from the
[**latest release**](https://github.com/brocla/rust-tumbler/releases/latest)
(all releases are listed on the
[Releases page](https://github.com/brocla/rust-tumbler/releases)). Each release
attaches two installers — use whichever you prefer:

- **NSIS** — `Tumbler_<version>_x64-setup.exe`
- **MSI** — `Tumbler_<version>_x64_en-US.msi`

These are self-contained (the required `pdfium.dll` and `qpdf.dll` are bundled),
so nothing else needs to be installed. The rest of this section is only for
building from source.

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


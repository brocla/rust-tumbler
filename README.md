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
- A command line and an MCP server, `tumbler-cli` (below)


## Scriptable

The installers also put `tumbler-cli.exe` on your `PATH`, beside `tumbler.exe`.
It runs the same engine without the window:

```sh
tumbler-cli info report.pdf                         # summary as JSON
tumbler-cli text scan.pdf --ocr                     # plain text, OCR'ing scanned pages
tumbler-cli find report.pdf "invoice \d+" --regex
tumbler-cli render report.pdf --page 3 --out p3.png
tumbler-cli edit in.pdf --out out.pdf --rotate 1,3:90 --delete 2 --title "Q3"
tumbler-cli combine a.pdf b.pdf c.pdf --out all.pdf
tumbler-cli split big.pdf --out-dir parts --every 10
tumbler-cli ocr scan.pdf --out searchable.pdf
tumbler-cli compress big.pdf --out small.pdf
tumbler-cli linearize report.pdf --out web.pdf
tumbler-cli expand-margins score.pdf --out bigger.pdf
tumbler-cli redact contract.pdf --term "Jane Doe" --rect 2:72,100,200,20 --out public.pdf
```

Underneath, every command is a sequence of calls to a table of tools, which
`tumbler-cli tools` lists (`--json` for their argument schemas). Call one
directly with `tumbler-cli run <tool> key=value ...`, or several in one session
with `tumbler-cli run --script steps.json`. Results go to stdout as JSON; exit
codes are 0 (success), 1 (the operation failed), 2 (a usage error) and 3 (a
redaction that could not be verified, so nothing was written). Redaction always
writes a new file and never changes the original.
Passwords come from `--password-file` or the `TUMBLER_PDF_PASSWORD` environment
variable, never the command line.

A terminal that was already open when Tumbler was installed won't see the new
`PATH` entry; open a new one.

## Built for agents

`tumbler-cli mcp --root <folder>` is a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdin/stdout, so an AI agent can open, inspect, edit and save PDFs
with the same tools. It runs only when an agent starts it, opens no network
port, and can read or write only inside the `--root` folder, which is required.

Claude Code:

```sh
claude mcp add tumbler -- tumbler-cli mcp --root C:\PDFs
```

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "tumbler": {
      "command": "C:\\Program Files\\Tumbler\\tumbler-cli.exe",
      "args": ["mcp", "--root", "C:\\PDFs"]
    }
  }
}
```

The MSI installs to `C:\Program Files\Tumbler`; the NSIS installer installs per
user, to `%LOCALAPPDATA%\Tumbler`.


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
so nothing else needs to be installed. For the command line alone, each release
also attaches `tumbler-cli-<version>-windows-x64.zip`: unzip it anywhere and
run `tumbler-cli.exe` from there. The rest of this section is only for building
from source.

These installers are not yet digitally signed — an application to the SignPath
Foundation is pending. See the [Code Signing Policy](CODE-SIGNING-POLICY.md) for
the intended arrangement and how signatures will be verified.

### Prerequisites

- Node.js 20+
- Rust (stable) with the Tauri v2 prerequisites for Windows
- A win-x64 `pdfium.dll` from the
  [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) release
  `chromium/7881` (the build CI and the release workflow use), placed at
  `src-tauri/resources/pdfium.dll` (not checked into the repo)
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

Both include `tumbler-cli.exe`, which the build compiles alongside the app
(`cargo run` and `npm run tauri dev` still start the app).

### Test

```sh
npm test           # frontend (Vitest)
cargo test         # backend (from src-tauri/)
```


## License

Tumbler is released under the [MIT License](LICENSE).

The Windows installers also redistribute PDFium (BSD-3-Clause), qpdf
(Apache-2.0) and the Microsoft Visual C++ runtime, each under its own terms.
See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

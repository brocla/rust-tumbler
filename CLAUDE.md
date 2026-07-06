# Tumbler — developer context

Tumbler is a personal PDF viewer for Windows built with Tauri v2 (Rust backend, React/TypeScript frontend). This file is the authoritative orientation for anyone — human or AI — implementing a feature or fixing a bug.

## What the app does

Opens PDF files (via file-association or drag-and-drop), displays them in a continuous-scroll viewer with zoom, and provides: full-text search with OCR fallback for scanned pages, text selection/copy, text export, thumbnail sidebar, metadata editing, page operations (delete/rotate/reorder/merge/split), native Windows printing, and a compression pipeline that reduces file size through five lopdf-based transforms.

---

## Tech stack

| Layer | Technology | Notes |
|---|---|---|
| Shell | Tauri v2 | Wraps a WebView2 window; Rust/frontend communicate via typed IPC commands |
| Frontend | React 18, TypeScript, Vite | Single-page app mounted in the WebView |
| State | Zustand | `src/store/usePdfStore.ts` — one global store |
| PDF rendering | pdfium-render (wraps Google's pdfium) | Read-only; renders pages to RGBA bitmaps |
| PDF editing | lopdf | Object-model write operations: metadata, compression, forms, text layer (page ops use pdfium) |
| OCR | Windows.Media.Ocr (WinRT) | Windows 10/11 built-in; requires a language pack |
| Printing | windows crate (GDI / PrintDlgExW) | Native Windows print dialogs and spooler |
| Icons | Lucide React | `lucide-react` package |
| Testing | Vitest + jsdom (frontend), `cargo test` (backend) | |

---

## Repository layout

```
rust-tumbler/
├── src/                          # React frontend
│   ├── components/               # One file per panel or UI region
│   ├── store/usePdfStore.ts      # All global frontend state
│   ├── utils/                    # renderCache, bitmap conversion, etc.
│   └── styles/global.css         # Design tokens and all CSS
├── src-tauri/                    # Rust / Tauri backend
│   ├── src/
│   │   ├── commands/             # One file per feature domain (see below)
│   │   ├── state.rs              # AppState — the shared runtime state
│   │   ├── error.rs              # AppError enum
│   │   ├── lib.rs                # Tauri builder, pdfium init, command registration
│   │   └── main.rs               # Entry point (calls lib::run)
│   ├── tests/fixtures/sample.pdf # Checked-in PDF used by backend tests
│   ├── Cargo.toml
│   └── tauri.conf.json
├── app-icon.svg                  # Placeholder; NOT the current app icon
├── tumbler.png                   # Master icon source (768×768, transparent bg)
└── package.json
```

### Backend command files

| File | Responsibility |
|---|---|
| `document.rs` | open / close documents (opens password-protected PDFs view-only, issue #12) |
| `render.rs` | render a page to a base64 bitmap (pdfium) |
| `text.rs` | extract text, full-document search, export text to `.txt` (pdfium) |
| `ocr.rs` | per-page and whole-document OCR via Windows.Media.Ocr |
| `metadata.rs` | read PDF metadata (pdfium) / write it (lopdf) |
| `pages.rs` | delete, rotate, reorder, merge, split pages (**pdfium** — mutates the `PdfDocument` then `save_to_bytes()`) |
| `save.rs` | Save / Save As — the only commands that write the in-memory buffer to disk |
| `optimize.rs` | five-step compression pipeline (lopdf) |
| `text_layer.rs` | embed an invisible OCR text layer into the document buffer (lopdf; issue #4) |
| `forms.rs` | AcroForm field discovery + inline value writes (lopdf on the buffer; issue #2) |
| `signature.rs` | digital-signature integrity verification, read-only (lopdf `/ByteRange` parse; issue #17) |
| `conformance.rs` | declared ISO sub-format detection — PDF/A, PDF/X, PDF/E, PDF/UA — from the XMP packet (lopdf) |
| `print.rs` | native GDI printing with progress and cancellation |
| `startup.rs` | read the file-association path passed on the command line |
| `theme.rs` | read the Windows accent color for UI theming |
| `app.rs` | app-level info commands |

---

## Frontend → backend communication

The frontend calls Rust functions via Tauri's IPC:

```ts
import { invoke } from "@tauri-apps/api/core";
const results = await invoke<ReturnType>("command_name", { arg1, arg2 });
```

Tauri maps `camelCase` JS keys to `snake_case` Rust parameters automatically. Every command returns `Result<T, String>` on the Rust side; rejected promises surface as strings in the frontend.

Backend-to-frontend events (for progress updates) use `window.emit("event-name", payload)` in Rust and `listen("event-name", handler)` in the frontend (or Tauri's React hooks).

### Registering a new command

1. Write the function in the appropriate `src-tauri/src/commands/*.rs` file with `#[tauri::command]`.
2. Add it to `tauri::generate_handler![...]` in `src-tauri/src/lib.rs`.
3. Call it with `invoke("command_name", { ... })` from the frontend.

---

## AppState

`AppState` (in `src-tauri/src/state.rs`) is Tauri's managed singleton, accessible in every command via `state: State<'_, AppState>`.

Key fields:

| Field | Type | Purpose |
|---|---|---|
| `pdfium` | `&'static Pdfium` | Leaked box; lives for the whole process. One pdfium instance per process. |
| `documents` | `Mutex<HashMap<String, Arc<Mutex<DocEntry>>>>` | Open documents keyed by `doc_id` (a UUID string). The two-level mutex (outer for the map, inner per document) means long operations on one document don't block other tabs. |
| `ocr_cache` | `Arc<Mutex<HashMap<(String,u32), Vec<OcrWord>>>>` | Recognized words keyed by `(doc_id, page_1based)`. Session-only — never written to disk. |
| `ocr_job` / `compress_job` / `print_job` | `Mutex<Option<Arc<AtomicBool>>>` | Cancellation tokens for long-running operations. |

`DocEntry` holds the `PdfDocument<'static>` (pdfium handle), the `file_path` string, `buffer: Vec<u8>` (the authoritative current bytes, including unsaved edits; `document` is always a pdfium render of it) and `dirty: bool` (true exactly when `buffer` differs from disk). Buffer-model edits end with `state.set_buffer_and_refresh(doc_id, bytes)` and emit `document-dirty-changed`; `save_document` / `save_document_as` (in `commands/save.rs`) are the only commands that write the buffer to disk.

Accessing a document safely:
```rust
let entry = state.get_document(&doc_id)?;   // clones the Arc, releases map lock
let entry = lock_mutex(&entry)?;            // locks the per-document mutex
// use entry.document / entry.file_path
// lock drops at end of scope
```

---

## Error handling

Use `AppError` (in `src-tauri/src/error.rs`) inside helper functions. The public `#[tauri::command]` functions convert it to `String` via `.map_err(String::from)` at the IPC boundary.

```rust
// constructors
AppError::pdfium("message", pdfium_err)
AppError::io("message", io_err)
AppError::lopdf("message", lopdf_err)
AppError::NotFound(doc_id)
```

Split each command into a public `#[tauri::command]` wrapper and a private `_impl` function that returns `Result<T, AppError>`. This keeps the impl testable without Tauri machinery.

---

## The two PDF libraries and when to use each

| Library | Use for | Cannot do |
|---|---|---|
| **pdfium** (via `pdfium-render`) | Rendering pages to bitmaps, reading text/coordinates, search, and page ops (delete/rotate/reorder/merge/split) | Object-level structural edits (adding/removing/rewriting individual objects) |
| **lopdf** | Metadata edits, compression, forms, text-layer embedding, signature/conformance parsing | Rendering |

All edits follow the **buffer model** — they read from and write back to `DocEntry.buffer`, never to disk:

- **pdfium-based edits (rotate/delete/reorder/merge):** mutate `entry.document` in place → `entry.document.save_to_bytes()` → `state.set_buffer_and_refresh(doc_id, bytes)`.
- **lopdf-based edits (metadata/compression/text layer):** `lopdf::Document::load_mem(&entry.buffer)` (not from disk) → mutate → serialize to `Vec<u8>` → `state.set_buffer_and_refresh(doc_id, bytes)`.
- Page-operation commands then emit `document-pages-changed` + `document-dirty-changed` via `pages::emit_pages_edited`.
- `save_document` / `save_document_as` in `commands/save.rs` are the only commands that write anything to disk.

---

## Frontend state

`usePdfStore` (Zustand) is the single source of truth.

Key slices:
- `tabs: TabState[]` — one entry per open document tab; holds `docId`, `currentPage`, `searchResults`, `zoom`, `displayMode`, `ocrEpoch`, `pagesVersion`, etc.
- `activeTabId` — which tab is focused.
- `activeSidebarTool` — which panel is open in the sidebar (`"thumbnails" | "search" | "metadata" | "pages" | "optimize" | null`).
- `ocrProgress` / `compressProgress` — shared between the trigger (Toolbar/panel) and the progress overlay (App).

`doc_id` is a UUID string generated on the frontend when a file is opened. It is the key used in all backend `HashMap`s.

---

## Saving files

`save_document` and `save_document_as` in `commands/save.rs` are the only commands that write the document buffer to disk. Both use **write-to-temp then atomic rename** via the `atomic_write` helper:
```rust
let tmp = format!("{dest_path}.tmp-{}", uuid::Uuid::new_v4());
std::fs::write(&tmp, &bytes)?;
std::fs::rename(&tmp, dest_path)?;
```
This ensures a crash or disk-full error cannot leave a truncated file at the destination.

---

## OCR

- OCR runs on the Windows.Media.Ocr engine (built into Windows 10/11).
- Results are cached in `AppState.ocr_cache` for the session only — never written to disk.
- `search_document` and `extract_page_text` both fall back to the OCR cache for pages with no native text layer.
- The cache is cleared when a document is closed or when `set_buffer_and_refresh` applies an edit (page layout may have changed).

---

## Multi-tab / same-file

The single-instance-per-file invariant (enforced on open) guarantees at most one `DocEntry` per file path. Each tab therefore has its own `doc_id` and its own self-contained buffer; no cross-tab sync is needed after an edit, because edits only update the buffer and `save_document` / `save_document_as` are the only disk writes.

---

## Compression pipeline

`src-tauri/src/commands/optimize.rs` implements five steps:

| StepId | What it does |
|---|---|
| `recompress_streams` | Re-deflate content streams (lopdf `compress()`) |
| `prune_unused` | Remove orphaned objects |
| `delete_zero_length` | Drop empty stream objects |
| `strip_extras` | Remove XMP metadata, page thumbnails, JavaScript, embedded files |
| `recompress_images` | Downsample high-DPI images and re-encode as JPEG (lossy) |

The core logic is in the free function `run_optimization_steps_impl`, which has no `AppState` or window dependency and is directly callable from non-Tauri code (e.g., a CLI or MCP server). The compressed bytes are applied to the document buffer via `state.set_buffer_and_refresh` and written to disk only when the user explicitly saves.

---

## Testing

### Frontend
```sh
npm test           # Vitest, runs in jsdom
```
Tests live alongside the components they test (`*.test.tsx`). Use `vi.mock` for Tauri `invoke` calls.

### Backend
```sh
cd src-tauri
cargo test -- --test-threads=1   # serial required; see note below
```
Tests use a shared `test_pdfium()` singleton (pdfium can only be bound once per process). Multi-step pdfium operations (create + edit + save) need the `test_pdfium_guard()` mutex to prevent races. The test teardown occasionally crashes under high concurrency — always run with `--test-threads=1`.

The fixture PDF (`tests/fixtures/sample.pdf`) is a single 200×200 page with the text "Test Fixture" at 24pt, near the top-left. Many backend tests depend on this layout; don't modify it.

---

## Build and run

```sh
# Install JS deps
npm install

# Dev mode (hot-reload frontend, auto-restart backend)
npm run tauri dev

# Production build (NSIS + MSI installers in src-tauri/target/release/bundle/)
npm run tauri build

# Regenerate all icon sizes from tumbler.png
npm run tauri -- icon tumbler.png
```

### Prerequisites

- Node.js 20+
- Rust stable + Tauri v2 prerequisites for Windows
- `src-tauri/resources/pdfium.dll` — win-x64 build from [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (not checked in)

---

## Version

Version is set in three files — keep them in sync:
- `package.json` → `"version"`
- `src-tauri/tauri.conf.json` → `"version"`
- `src-tauri/Cargo.toml` → `version`

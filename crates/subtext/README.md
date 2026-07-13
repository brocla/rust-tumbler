# Subtext — A Redaction Checker

Subtext is a **read-only, tool-agnostic** command-line tool that answers one
question about a PDF:

> *The word (or words, or pattern) I redacted — does it still appear anywhere in
> this file?*

…and, just as importantly:

> *Which of the many places a PDF can hide text did you actually check?*

Redaction fails in dozens of subtle ways: the black box is drawn *over* text
that's still in the content stream; the visible copy is removed but a copy
survives in the document metadata, an annotation, a form field, an attached
file, a superseded incremental-update revision, or an object the cross-reference
table no longer points at. Subtext inverts a redactor: it hunts a term across
**every** vector where a leak can hide and reports each one it finds — and every
vector it *couldn't* inspect, with a reason.

**Completeness is the product.** Subtext never certifies a file "clean." It
reports "no matches found in the N vectors listed below," lists them, and flags
any it had to skip. A false "clean" is the one unacceptable outcome.

---

## Install / build

Subtext is a Rust crate. From the repository root:

```sh
# Portable build (Windows / macOS / Linux) — no native deps beyond pdfium
cargo build --release -p subtext
# → target/release/subtext(.exe)
```

It loads `pdfium` at runtime. Point it at a `pdfium.dll`/`.so`/`.dylib` by
placing one next to the executable, in `./src-tauri/resources/` (the dev
layout), or on the system library path. Prebuilt binaries:
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries).

### The optional OCR pass

The rendered-image OCR vector (`--ocr`) is **Windows-only** and behind a Cargo
feature, because it links Tumbler's `Windows.Media.Ocr` engine:

```sh
cargo build --release -p subtext --features ocr
```

The default build omits it and reports that vector as *not implemented* (an
honest skip, never a false clean). See [Why OCR is opt-in](#why-ocr-is-opt-in).

---

## Usage

```
subtext [OPTIONS] <FILE>...

  --term <WORD>          Term to search for. Repeat for a list. (mutually exclusive with --regex)
  --regex <PATTERN>      Regular expression to search for.
  --case-sensitive       Case-sensitive matching (default: insensitive).
  --whole-word           Whole-word matching only.
  --password <PASSWORD>  Password for encrypted inputs (applied to every FILE).
  --recurse-embedded     Descend into embedded PDFs (attachments), depth-capped.
  --ocr                  Run the rendered-image OCR pass (needs a --features ocr build).
  --json                 Emit the machine-readable JSON report instead of the human summary.
```

**Exit codes** (scriptable):

| code | meaning |
|------|---------|
| `0`  | no matches (clean or warning only) |
| `1`  | a leak was found |
| `2`  | an error (unreadable file, bad arguments) |

### Examples

```sh
# Does "Zanzibar" survive anywhere in this file?
subtext --term Zanzibar report.pdf

# Hunt a pattern (e.g. US SSNs) instead of a literal term
subtext --regex "\d{3}-\d{2}-\d{4}" report.pdf

# The telling before/after pair: a leaky file, then its redacted output
subtext --term Zanzibar leaky.pdf          # → LEAK (exit 1)
subtext --term Zanzibar redacted.pdf       # → no findings (exit 0)

# Encrypted input, and descend into embedded PDFs
subtext --term secret --password hunter2 --recurse-embedded case.pdf

# Full-coverage scan including the pixel pass (OCR build)
subtext --term secret --ocr scan.pdf

# Machine-readable report; a batch emits a JSON array
subtext --term secret --json *.pdf > findings.json
```

---

## What it inspects — the 21 vectors

Every run reports on all 21, so the checklist can never silently drift from
what's implemented:

| # | Vector | What it catches |
|---|--------|-----------------|
| 1 | Page text | Text pdfium can extract from the page |
| 2 | Rendered-image OCR | Image-of-text (scans, flattened boxes) — `--ocr` |
| 3 | Document metadata | Info dictionary + XMP packets |
| 4 | Structure tree | `/StructTreeRoot`, `/ActualText`, `/Alt` |
| 5 | Marked content | `/BDC` property lists, `/ActualText`, `/Alt` |
| 6 | Bookmarks | Outline titles |
| 7 | Page labels | `/PageLabels` prefixes |
| 8 | Named destinations | Destination names |
| 9 | Article threads | `/Threads` bead titles |
| 10 | Annotations | `/Contents`, `/T`, `/Subj`, `/RC`, and `/AP` appearance streams |
| 11 | Redaction annotations | Surviving `/Redact` overlays (fires a signal) |
| 12 | Form fields | AcroForm values |
| 13 | XFA forms | XFA datasets |
| 14 | Attachments | `/EmbeddedFiles`, `/AF`, embedded file bytes |
| 15 | Scripts & actions | JavaScript sources |
| 16 | URIs & web capture | URI actions |
| 17 | Optional content | Layer (`/OCG`) names + hidden-layer content |
| 18 | Signatures | Signature dictionary text |
| 19 | Superseded revisions | Secrets left in a prior incremental-update revision |
| 20 | Orphaned objects | Objects unreachable from the current xref |
| 21 | Raw decompressed scan | The inflate-every-stream backstop |

---

## How it works

- **Two independent parser views.** [pdfium](https://pdfium.googlesource.com/pdfium/)
  supplies rendered pages and extracted text; [lopdf](https://crates.io/crates/lopdf)
  supplies the strict object graph. A file may parse under one and not the other
  (a recovered corrupt xref, an unsupported filter); each check reports `Skipped`
  when it lacks the view it needs, so a partial parse yields a partial-but-honest
  report rather than an error.

- **Skip kinds are scored, not hidden.** A skip is one of:
  - **Unavailable** — a per-file blind spot (encryption without a password, an
    unsupported filter, an unreadable catalog). Caps a no-match report at
    *warning / medium* — the term could be hiding where the file blocked us.
  - **NotRequested** — the tool *can* do it, you opted out this run (`--ocr`,
    `--recurse-embedded`). Disclosed, scored *low*.
  - **NotImplemented** — the pass isn't built into this binary (a portable
    build's OCR). Disclosed, scored *low*.

  A full-coverage **Clean** is only reachable when every vector actually ran —
  which, for OCR-recoverable content, means an `ocr` build run with `--ocr`.

- **Query-independent signals** fire regardless of your search term: a surviving
  `/Redact` annotation (redaction marked but never applied), a page whose
  rendered pixels disagree with its extracted text layer (glyph spoofing, under
  `--ocr`), or a sub-document that couldn't be inspected. Any signal lifts a
  no-match report to *warning*.

## Why OCR is opt-in

Two layers, both deliberate:

- **The `ocr` Cargo feature** keeps the default build **portable** (it's the only
  Windows-linked code) and **fast** (linking it pulls in Tumbler's whole backend).
  OCR is a heavyweight, platform-specific, probabilistic pass — genuinely
  additive, not core.
- **The `--ocr` runtime flag** keeps each scan's cost under your control (a full
  render + recognize per page is far slower than the rest) and keeps coverage
  reporting honest: without it, the OCR vector reports *NotRequested* ("re-run
  with `--ocr`") rather than silently skipping — so a `Clean` verdict always
  means *everything*, including the pixels, was checked.

---

## Testing

```sh
cargo test -p subtext --lib                              # unit tests (no pdfium)
cargo test -p subtext --test corpus -- --test-threads=1  # corpus (needs pdfium)
cargo test -p subtext --features ocr --lib rendered_ocr  # OCR-pass tests (Windows)
```

The integration suite runs the 13-file adversarial pen-test corpus in
`../../src-tauri/tests/fixtures/redaction/`: every attack must be detected, a
clean control must yield nothing, and — closing the loop — Subtext must find
**0** in Tumbler's *redacted* output of each attack.

## Design

The full design contract and vector rationale live in
[`../../doc/redaction-checker-design.md`](../../doc/redaction-checker-design.md).

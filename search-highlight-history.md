# Search Highlight Alignment: Development History

## Attempt 1 — Wrap matches in `<mark>` elements inside spans

**Approach:** Walk every `<span>` in the PDF.js text layer, find spans whose
full text contained the query string, and replace the span's text node by
splitting it into plain text nodes + `<mark>` elements wrapping the matched
substring.

**Problems:**
- `<mark>` has a browser-default `color: MarkText` that overrides the
  inherited `color: transparent`, so the matched text became visible as
  duplicate text rendered over the canvas.
- Inserting child elements into a span disrupts PDF.js's text layout. The
  span's `scaleX` CSS transform is applied to the whole span, so any inline
  child element placed inside it is stretched/shifted proportionally to its
  x-position within the line. This caused a **rightward drift that grew
  linearly with distance from the left margin** — perfect alignment at the
  leftmost character, increasing error further right.

---

## Attempt 2 — `color: transparent` on `<mark>`, same DOM insertion approach

**Approach:** Added `color: transparent` to the `.search-highlight` CSS rule
to suppress the visible duplicate text.

**Problems:**
- Eliminated the duplicate text.
- Horizontal drift remained completely unchanged — it was caused by the
  transform interaction, not the color.

---

## Attempt 3 — Separate highlight layer using `Range.getClientRects()`

**Approach:** Left the text layer spans completely untouched. Instead, used
the DOM `Range` API to select the matched characters, called
`range.getClientRects()` to get their viewport-space bounding boxes, and
placed absolutely-positioned `<div>` highlights in a separate sibling
`<div>` (`.highlightLayer`) outside the transformed text layer.

**Reasoning:** Since `.highlightLayer` has no PDF.js transforms applied to
it, converting viewport coordinates to its local space should be a simple
subtraction of its `getBoundingClientRect()`.

**Problems:**
- The drift persisted, and scaled with zoom level (correct at 125%, wrong at
  100% and 150%). This indicated that `getClientRects()` itself was being
  affected by the PDF.js transform on the `.textLayer` — the Range
  coordinates were in post-transform viewport space but the container rect
  was in its own pre-transform space, and the two did not cancel cleanly
  across zoom levels.

---

## Attempt 4 — PDF viewport coordinate math (no DOM measurement)

**Approach:** Abandoned all DOM measurement. Used the raw `TextItem` data
from `page.getTextContent()` directly. Each `TextItem` has a `transform`
array with its PDF user-space origin `(tx, ty)` and a `width` value. The
match position within the item was approximated as a proportional fraction
of the total width: `xStart = tx + width * (matchIdx / str.length)`.
Called `viewport.convertToViewportPoint()` to convert to canvas pixel
space, which is the exact coordinate space of the highlight layer.

**Problems:**
- Horizontal position was correct on average, but **proportional fonts**
  meant the per-character width assumption was wrong. Narrow characters
  (`,`, `i`, `l`) and wide characters (`W`, `M`) accumulate error. A match
  near the end of a long item drifted noticeably.
- **Cross-item splits:** PDF stores text as independent positioned chunks,
  not as lines. A single word can be split across two `TextItem`s. Searching
  per-item missed these matches entirely.

---

## Attempt 5 — Concatenated search + `measureText` proportional positioning ✓

**Approach (current):** Two core improvements over Attempt 4:

**1. Cross-item search:** All `TextItem` strings are concatenated into one
flat string before searching. A parallel `charMap` array records for each
character position which item index and character offset it belongs to. A
match found in the concatenated string is then mapped back to one or more
items, emitting one highlight rect per item segment spanned.

**2. Proportional font measurement:** The rendered span for each item has its
actual CSS `font` property set by PDF.js (correct family, size, and
transform). An `OffscreenCanvas` context reads that font via
`getComputedStyle(span).font` and calls `measureText()` on the prefix
substring to get the true advance width. The ratio `prefixWidth / totalWidth`
is font-accurate for proportional fonts at any zoom level.

The highlight `<div>`s are still placed in the separate `.highlightLayer`
sibling, using `viewport.convertToViewportPoint()` for coordinates. No DOM
bounding rect measurement is involved — the coordinate system is purely the
PDF viewport, which equals the canvas pixel space.

**Result:** Accurate alignment at all zoom levels, and correct detection of
words split across text items.

---

## What instruction would have gotten here directly

> "Add search highlighting. Use `page.getTextContent()` to get `TextItem`
> objects. Concatenate all items into one string with a character map back
> to item+offset for cross-item matches. For each match, compute the x
> position of the matched characters using `measureText()` on an
> `OffscreenCanvas` with the font from `getComputedStyle()` on the
> corresponding rendered span, proportionally within the item's user-space
> width. Convert coordinates to canvas pixel space with
> `viewport.convertToViewportPoint()`. Place highlight `<div>`s in a
> separate sibling element (`.highlightLayer`) at `inset: 0` inside the
> canvas wrapper — do not place them inside the transformed `.textLayer`."

---

## PDF.js built-in alternatives

PDF.js ships two classes in its `web/` layer (not the core `src/` layer)
that do this work:

### `PDFFindController` (`pdfjs-dist/web/pdf_find_controller`)

A full search engine. Given a `PDFDocumentProxy`, it extracts text from all
pages, finds all matches with `match(query, pageContent, pageIndex)`, tracks
the selected match, and maintains `pageMatches` and `pageMatchesLength`
arrays. It requires a `linkService` (navigation abstraction) and an
`EventBus` to communicate with the rest of the viewer. It does not render
anything itself — it is a data layer only.

### `TextHighlighter` (`pdfjs-dist/web/text_highlighter`)

Takes the match data from `PDFFindController` and renders highlights into
the text layer spans using `_convertMatches()` / `_renderMatches()`. It
manipulates the spans directly (similar to our Attempt 1 approach but
internally correct because it uses the pre-computed match character offsets
from `PDFFindController` rather than a text search). Requires
`setTextMapping(divs, texts)` to be called with the rendered span nodes and
their text strings.

### Why we didn't use them

Both classes are part of the full PDF.js *viewer* (`PDFViewer`,
`PDFPageView`), not the rendering core. They expect the complete viewer
infrastructure: `EventBus`, `IPDFLinkService`, `PDFViewer`. Wiring them up
would mean adopting the full PDF.js viewer stack, which conflicts with the
Tauri + React architecture (we render to a single canvas per page, not the
PDF.js DOM-based multi-page scroller). Adopting `PDFViewer` would also mean
giving up the `OffscreenCanvas` render cache and the Zustand-driven state
model.

Using them is feasible but would represent a larger architectural shift. Our
Attempt 5 implementation covers the same ground at roughly the same accuracy
for Tumbler's needs.

---

## pdf-lib

pdf-lib has no search or text-layer functionality. It is a PDF *document
manipulation* library (create, merge, split, edit metadata, rotate pages) and
operates on the binary PDF structure. Text content is write-only from pdf-lib's
perspective — it can add text annotations and form fields, but it has no API
for searching existing text or computing glyph positions for display purposes.
Highlighting is entirely outside its scope.

---

## What is Zustand and why is it in this project?

Zustand is a minimal React state management library. It stores application
state in a plain JavaScript object outside the React component tree and
provides a hook (`usePdfStore`) that any component can call to read or write
that state. When state changes, only the components that subscribed to the
changed slice re-render.

**Why not just React `useState`?**

The PDF viewer has many components that need the same state simultaneously:

- `Toolbar` reads `currentPage`, `zoom`, `pageCount`, `fileName`, and writes
  all of them via buttons.
- `ViewerArea` reads `currentPage`, `zoom`, `searchQuery`, and the
  `PDFDocumentProxy`.
- `ThumbnailPanel` reads `currentPage` and `pageCount`; each thumbnail writes
  `currentPage` on click.
- `SearchPanel` reads and writes `searchQuery`, `searchResults`, and
  `currentPage`.
- `MetadataPanel` reads and writes `fileBytes`.

With `useState`, this state would have to live in the nearest common
ancestor of all those components — `App.tsx`. Every piece of state would be
passed down as props through every intermediate component, and every state
setter would be threaded back up as a callback prop. This is called "prop
drilling" and becomes unmaintainable quickly.

Zustand solves this by making the store globally accessible. Any component
subscribes directly to exactly the slices it needs:

```ts
// Only re-renders when currentPage or zoom changes — ignores searchQuery etc.
const { currentPage, zoom } = usePdfStore();

// Or even more selective:
const pageCount = usePdfStore((s) => s.pageCount);
```

**Why Zustand specifically over Redux or React Context?**

- Redux requires significant boilerplate (actions, reducers, dispatch).
- React Context re-renders every consumer whenever any part of the context
  value changes — no built-in slice subscriptions.
- Zustand has no boilerplate, slice subscriptions are automatic, the store
  is directly callable outside React (e.g., `usePdfStore.getState()` in
  event handlers), and the entire library is ~1KB.

For a medium-complexity app like Tumbler — single document, shared viewer
state, no server synchronization — Zustand is the right size of tool. It
would become worth reconsidering only if the app grew to multiple open
documents or needed time-travel debugging, at which point Redux Toolkit
would be more appropriate.

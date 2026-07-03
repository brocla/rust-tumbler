/**
 * LRU caches for rendered page bitmaps.
 *
 * Two independent caches share the same implementation:
 *  - the page cache (full-size canvas renders, keyed by zoom + dpr)
 *  - the thumbnail cache (small fixed-scale renders, keyed by dpr only)
 *
 * A pure page reorder does not change any bitmap — it only relabels which page
 * number each bitmap belongs to. `permuteDoc` exploits that: it rewrites cache
 * keys in place so the already-rendered bitmaps stay valid, letting the UI
 * repaint a reorder from cache instead of evicting and re-rendering (which is
 * what caused the blank → "Loading…" flash on the main canvas).
 */

interface CacheEntry {
  key: string;
  bitmap: ImageBitmap;
}

interface Cache {
  get(docId: string, page: number, sig: string): ImageBitmap | null;
  put(docId: string, page: number, sig: string, bitmap: ImageBitmap): void;
  evict(docId: string): void;
  evictPage(docId: string, page: number): void;
  permute(docId: string, newOrder: number[]): void;
}

function makeKey(docId: string, page: number, sig: string): string {
  return `${docId}:${page}:${sig}`;
}

function createCache(maxEntries: number): Cache {
  const entries: CacheEntry[] = [];

  return {
    get(docId, page, sig) {
      const key = makeKey(docId, page, sig);
      const idx = entries.findIndex((e) => e.key === key);
      if (idx === -1) return null;
      // Move to end (most recently used)
      const [entry] = entries.splice(idx, 1);
      entries.push(entry);
      return entry.bitmap;
    },

    put(docId, page, sig, bitmap) {
      const key = makeKey(docId, page, sig);

      // Replace if already exists
      const idx = entries.findIndex((e) => e.key === key);
      if (idx !== -1) {
        entries[idx].bitmap.close();
        entries.splice(idx, 1);
      }

      // Evict oldest if full
      while (entries.length >= maxEntries) {
        const evicted = entries.shift();
        evicted?.bitmap.close();
      }

      entries.push({ key, bitmap });
    },

    evict(docId) {
      for (let i = entries.length - 1; i >= 0; i--) {
        if (entries[i].key.startsWith(docId + ":")) {
          entries[i].bitmap.close();
          entries.splice(i, 1);
        }
      }
    },

    evictPage(docId, page) {
      // Drop every sig (zoom/dpr) variant of this one page.
      const prefix = `${docId}:${page}:`;
      for (let i = entries.length - 1; i >= 0; i--) {
        if (entries[i].key.startsWith(prefix)) {
          entries[i].bitmap.close();
          entries.splice(i, 1);
        }
      }
    },

    permute(docId, newOrder) {
      // newOrder is 1-based: newOrder[i] is the page (old numbering) that
      // becomes page i+1. Build old → new and rewrite keys in place. Because
      // the mapping is a bijection, the rewritten key set has no collisions.
      const oldToNew = new Map<number, number>();
      newOrder.forEach((oldPage, i) => oldToNew.set(oldPage, i + 1));

      const prefix = docId + ":";
      for (const entry of entries) {
        if (!entry.key.startsWith(prefix)) continue;
        const rest = entry.key.slice(prefix.length); // "page:sig…"
        const sep = rest.indexOf(":");
        const page = Number(rest.slice(0, sep));
        const newPage = oldToNew.get(page);
        if (newPage !== undefined) {
          entry.key = `${docId}:${newPage}:${rest.slice(sep + 1)}`;
        }
      }
    },
  };
}

const pageCache = createCache(20);
const thumbCache = createCache(80);

// ── Page cache (full-size canvas) ──────────────────────────────────────────

export function getCached(docId: string, page: number, zoom: number, dpr: number): ImageBitmap | null {
  return pageCache.get(docId, page, `${zoom}:${dpr}`);
}

export function putCached(docId: string, page: number, zoom: number, dpr: number, bitmap: ImageBitmap): void {
  pageCache.put(docId, page, `${zoom}:${dpr}`, bitmap);
}

export function evictDoc(docId: string): void {
  pageCache.evict(docId);
  thumbCache.evict(docId);
}

/**
 * Evict only the full-size page cache, leaving thumbnails intact. Used after an
 * optimistic reorder: the main canvas reconciles to the backend's authoritative
 * render, while the thumbnail cache keeps its relabeled bitmaps so a subsequent
 * reorder can still repaint synchronously (an empty cache would force an async
 * re-fetch and bring back the snap-back flash).
 */
export function evictPages(docId: string): void {
  pageCache.evict(docId);
}

/**
 * Evict a single page's full-size renders (all zoom/dpr variants). Used after a
 * form-field edit on a comb field, whose value is drawn by pdfium onto the page
 * canvas — the cached bitmap is now stale and must be re-rendered.
 */
export function evictPageCache(docId: string, page: number): void {
  pageCache.evictPage(docId, page);
}

// ── Thumbnail cache (small fixed-scale renders) ────────────────────────────

export function getThumb(docId: string, page: number, dpr: number): ImageBitmap | null {
  return thumbCache.get(docId, page, `t:${dpr}`);
}

export function putThumb(docId: string, page: number, dpr: number, bitmap: ImageBitmap): void {
  thumbCache.put(docId, page, `t:${dpr}`, bitmap);
}

// ── Reorder: relabel both caches without re-rendering ──────────────────────

export function permuteDoc(docId: string, newOrder: number[]): void {
  pageCache.permute(docId, newOrder);
  thumbCache.permute(docId, newOrder);
}

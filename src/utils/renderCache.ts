/**
 * LRU cache for rendered page bitmaps.
 * Keys: "docId:page:zoom:dpr"
 * Values: ImageBitmap
 */
const MAX_ENTRIES = 20;

interface CacheEntry {
  key: string;
  bitmap: ImageBitmap;
}

const entries: CacheEntry[] = [];

function makeKey(docId: string, page: number, zoom: number, dpr: number): string {
  return `${docId}:${page}:${zoom}:${dpr}`;
}

export function getCached(docId: string, page: number, zoom: number, dpr: number): ImageBitmap | null {
  const key = makeKey(docId, page, zoom, dpr);
  const idx = entries.findIndex((e) => e.key === key);
  if (idx === -1) return null;
  // Move to end (most recently used)
  const [entry] = entries.splice(idx, 1);
  entries.push(entry);
  return entry.bitmap;
}

export function putCached(docId: string, page: number, zoom: number, dpr: number, bitmap: ImageBitmap): void {
  const key = makeKey(docId, page, zoom, dpr);

  // Replace if already exists
  const idx = entries.findIndex((e) => e.key === key);
  if (idx !== -1) {
    entries[idx].bitmap.close();
    entries.splice(idx, 1);
  }

  // Evict oldest if full
  while (entries.length >= MAX_ENTRIES) {
    const evicted = entries.shift();
    evicted?.bitmap.close();
  }

  entries.push({ key, bitmap });
}

export function evictDoc(docId: string): void {
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].key.startsWith(docId + ":")) {
      entries[i].bitmap.close();
      entries.splice(i, 1);
    }
  }
}

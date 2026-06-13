import { describe, it, expect, beforeEach, vi } from "vitest";
import { getCached, putCached, evictDoc } from "./renderCache";

// Mock ImageBitmap since jsdom doesn't have it
function makeBitmap(id: number): ImageBitmap {
  return { width: 100, height: 100, close: vi.fn(), _id: id } as unknown as ImageBitmap;
}

// Reset cache between tests by evicting everything
beforeEach(() => {
  // Evict some unlikely doc IDs to flush the cache
  for (let i = 0; i < 30; i++) {
    evictDoc(`__reset_${i}`);
  }
  // Put and evict to fully clear
  for (let i = 0; i < 25; i++) {
    putCached("__flush__", i, 100, 1, makeBitmap(9000 + i));
  }
  evictDoc("__flush__");
});

describe("renderCache", () => {
  it("returns null for cache miss", () => {
    expect(getCached("doc1", 1, 100, 1)).toBeNull();
  });

  it("returns bitmap for cache hit", () => {
    const bmp = makeBitmap(1);
    putCached("doc1", 1, 100, 1, bmp);
    expect(getCached("doc1", 1, 100, 1)).toBe(bmp);
  });

  it("distinguishes different zoom levels", () => {
    const bmp100 = makeBitmap(1);
    const bmp200 = makeBitmap(2);
    putCached("doc1", 1, 100, 1, bmp100);
    putCached("doc1", 1, 200, 1, bmp200);

    expect(getCached("doc1", 1, 100, 1)).toBe(bmp100);
    expect(getCached("doc1", 1, 200, 1)).toBe(bmp200);
  });

  it("evicts all entries for a document", () => {
    putCached("doc1", 1, 100, 1, makeBitmap(1));
    putCached("doc1", 2, 100, 1, makeBitmap(2));
    putCached("doc2", 1, 100, 1, makeBitmap(3));

    evictDoc("doc1");

    expect(getCached("doc1", 1, 100, 1)).toBeNull();
    expect(getCached("doc1", 2, 100, 1)).toBeNull();
    expect(getCached("doc2", 1, 100, 1)).not.toBeNull();
  });

  it("evicts oldest entries when cache is full", () => {
    // Fill cache with 20 entries
    for (let i = 0; i < 20; i++) {
      putCached("doc1", i + 1, 100, 1, makeBitmap(i));
    }

    // Add one more — should evict page 1
    putCached("doc1", 21, 100, 1, makeBitmap(20));
    expect(getCached("doc1", 1, 100, 1)).toBeNull();
    expect(getCached("doc1", 21, 100, 1)).not.toBeNull();
  });

  it("calls close() on evicted bitmaps", () => {
    const bitmaps = Array.from({ length: 21 }, (_, i) => makeBitmap(i));
    for (let i = 0; i < 21; i++) {
      putCached("doc1", i + 1, 100, 1, bitmaps[i]);
    }
    // First bitmap should have been evicted and closed
    expect(bitmaps[0].close).toHaveBeenCalled();
  });
});

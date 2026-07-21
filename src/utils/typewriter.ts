import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";
import type { TypewriterAnnot, TypewriterStyle } from "../store/usePdfStore";

/**
 * Typewriter helpers (issue #99): color <-> hex conversion, base-14 → CSS font
 * mapping so the live overlay matches how the note will render, and the commit
 * that writes the current notes into the document buffer as FreeText
 * annotations. The overlay is authoritative for what the user sees; committing
 * is what makes the edit a (dirty) buffer edit the user can Save.
 */

/** RGB (each 0.0..=1.0) → "#rrggbb". */
export function rgbToHex([r, g, b]: [number, number, number]): string {
  const to255 = (v: number) =>
    Math.max(0, Math.min(255, Math.round(v * 255)))
      .toString(16)
      .padStart(2, "0");
  return `#${to255(r)}${to255(g)}${to255(b)}`;
}

/** "#rrggbb" → RGB (each 0.0..=1.0). Falls back to black on a bad string. */
export function hexToRgb(hex: string): [number, number, number] {
  const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
  if (!m) return [0, 0, 0];
  return [
    parseInt(m[1], 16) / 255,
    parseInt(m[2], 16) / 255,
    parseInt(m[3], 16) / 255,
  ];
}

/** CSS `font-family` stack approximating a base-14 family for the overlay. */
export function fontFamilyCss(family: TypewriterAnnot["fontFamily"]): string {
  switch (family) {
    case "Times":
      return '"Times New Roman", Times, serif';
    case "Courier":
      return '"Courier New", Courier, monospace';
    default:
      return "Arial, Helvetica, sans-serif";
  }
}

/** A fresh note at a page point, using the current default style. */
export function newAnnot(
  page: number,
  x: number,
  y: number,
  style: TypewriterStyle,
): TypewriterAnnot {
  return {
    id: crypto.randomUUID(),
    page,
    x,
    y,
    // A sensible starting box; the user can resize it.
    width: 160,
    height: Math.max(style.fontSize * 1.6, 20),
    text: "",
    fontFamily: style.fontFamily,
    bold: style.bold,
    italic: style.italic,
    fontSize: style.fontSize,
    color: style.color,
  };
}

/**
 * Writes the given doc's current notes into its buffer (FreeText annotations),
 * marking it dirty. Empty-text notes are dropped so a placed-but-never-typed
 * box doesn't persist. Best-effort: a failure is surfaced as a notice rather
 * than thrown, so an overlay interaction never rejects.
 */
export async function commitTypewriter(docId: string): Promise<void> {
  const tab = usePdfStore.getState().tabs.find((t) => t.docId === docId);
  if (!tab) return;
  const annots = (tab.typewriterAnnots ?? []).filter((a) => a.text.trim().length > 0);
  try {
    await invoke("apply_typewriter", { docId, annots });
  } catch (err) {
    usePdfStore.getState().showNotice(`Could not save typewriter text: ${String(err)}`);
  }
}

/**
 * Ink Signature (issue #120) — the one colour and the one width.
 *
 * These mirror `INK_RGB` / `INK_WIDTH_PT` in `commands/ink.rs`, which also
 * paints the `/Sig` form-field signatures. The tool is deliberately narrow:
 * a single blue, a single width, undo as the only eraser.
 *
 * `#0B35B8` is "bright ballpoint" — a signature should not photocopy as
 * though it were part of the printed form, and this shade stays visibly
 * blue-grey when desaturated where a blue-black goes nearly as dark as the
 * surrounding print.
 */
export const INK_COLOR = "#0B35B8";

/** Stroke width in PDF points; multiply by the zoom scale to draw on screen. */
export const INK_WIDTH_PT = 1.5;

/** A point in PDF points, top-left origin — the space search rects use. */
export type InkPoint = [number, number];

/** One continuous polyline, from pointer-down to pointer-up. */
export type InkStroke = InkPoint[];

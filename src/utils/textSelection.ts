/**
 * Pure helpers shared by the PDF text layer (line grouping for selection
 * highlights) and the global copy handler (reconstructing line breaks and
 * inter-item gaps when plain-text is copied out of the absolutely-positioned
 * text layer).
 */

export interface LineGroupItem {
  y: number;
  height: number;
}

/**
 * Assigns a line index to each item by grouping items whose vertical bounds
 * overlap with the previous item's. Items on the same visual line can still
 * have different y/height (e.g. a list number set in a larger font than its
 * item text), so a fixed y-tolerance isn't reliable — overlap is.
 */
export function groupIntoLines(items: LineGroupItem[]): number[] {
  let lineIndex = 0;
  let prevTop: number | null = null;
  let prevBottom: number | null = null;

  return items.map((item) => {
    const itemBottom = item.y + item.height;
    if (prevTop !== null && prevBottom !== null) {
      const overlaps = item.y < prevBottom && itemBottom > prevTop;
      if (!overlaps) lineIndex++;
    }
    prevTop = item.y;
    prevBottom = itemBottom;
    return lineIndex;
  });
}

/**
 * The Rust text extractor only splits same-line runs at gaps >= 0.5 *
 * fontSize, so any gap above this threshold is a deliberate spatial gap, not
 * adjacent run fragments. One threshold/character means gaps of varying
 * width (e.g. "1." vs "10.") still align to the same tab stop.
 */
export const GAP_THRESHOLD = 0.2;

/**
 * One entry per node visited while walking a copied selection's DOM
 * fragment, in document order. Text-layer spans (`[data-line]` elements)
 * carry position/line info with empty `text`; plain text nodes carry the
 * actual characters with `line: null`.
 */
export interface CopyToken {
  text: string;
  line: string | null;
  x: number;
  right: number;
  fontSize: number;
}

/**
 * Reconstructs copied plain text with "\n" between lines and "\t" across a
 * real spatial gap on the same line (e.g. between a list number and its item
 * text), matching the visual layout of the PDF.
 */
export function reconstructCopyText(tokens: CopyToken[]): string {
  let result = "";
  let lastLine: string | null = null;
  let prevRight: number | null = null;

  for (const token of tokens) {
    if (token.line !== null) {
      if (lastLine !== null && token.line !== lastLine) {
        result += "\n";
      } else if (lastLine !== null && prevRight !== null && token.fontSize > 0) {
        const gap = token.x - prevRight;
        if (gap > token.fontSize * GAP_THRESHOLD) {
          result += "\t";
        }
      }
      lastLine = token.line;
      prevRight = token.right;
    }
    result += token.text;
  }

  return result;
}

interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface Match {
  rects: Rect[];
}

interface HighlightLayerProps {
  matches: Match[];
  /** Index of the active match within `matches`, or -1 for none. */
  activeIndex: number;
  zoom: number;
}

/**
 * Draws one box per line of each match. The active match highlights all of its
 * boxes — a match broken across a line break is one result, so lighting up only
 * half of it would read as two.
 */
export function HighlightLayer({ matches, activeIndex, zoom }: HighlightLayerProps) {
  if (matches.length === 0) return null;

  const scale = zoom / 100;

  return (
    <div className="highlight-layer">
      {matches.map((match, m) =>
        match.rects.map((rect, i) => (
          <div
            key={`${m}-${i}`}
            className={
              m === activeIndex ? "search-highlight active" : "search-highlight"
            }
            style={{
              position: "absolute",
              left: rect.x * scale,
              top: rect.y * scale,
              width: rect.width * scale,
              height: rect.height * scale,
            }}
          />
        )),
      )}
    </div>
  );
}

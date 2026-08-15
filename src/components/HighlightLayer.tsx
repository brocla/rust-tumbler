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
 * Screen pixels added to every side of a highlight box, and the floor its
 * on-screen size may not drop below.
 *
 * These are deliberately in *screen* pixels rather than PDF points: a box sized
 * purely in points shrinks with the zoom, and the case where a highlight is
 * hardest to spot is exactly low zoom, where a line of 5pt text is about three
 * pixels tall. Padding in points would shrink right along with the problem.
 * The cost is that a highlight bleeds slightly onto its neighbours at high
 * zoom, which is the intended trade.
 */
const PAD = 3;
const MIN_WIDTH = 6;
const MIN_HEIGHT = 14;

/**
 * Converts a search rect (PDF points, page-relative) into the on-screen box to
 * paint, padded and floored. Growth is symmetric about the rect's centre so an
 * enlarged box still sits over the text it marks rather than drifting below it.
 */
export function highlightBox(rect: Rect, scale: number) {
  const width = Math.max(rect.width * scale + PAD * 2, MIN_WIDTH);
  const height = Math.max(rect.height * scale + PAD * 2, MIN_HEIGHT);
  const centerX = (rect.x + rect.width / 2) * scale;
  const centerY = (rect.y + rect.height / 2) * scale;
  return { left: centerX - width / 2, top: centerY - height / 2, width, height };
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
        match.rects.map((rect, i) => {
          const active = m === activeIndex;
          return (
            <div
              key={
                // Folding the active index into the key remounts the box each
                // time the active match moves, which restarts its pulse. Without
                // it, stepping to a match that is already on screen changes
                // nothing the eye can catch.
                active ? `active-${activeIndex}-${m}-${i}` : `${m}-${i}`
              }
              className={active ? "search-highlight active" : "search-highlight"}
              style={{ position: "absolute", ...highlightBox(rect, scale) }}
            />
          );
        }),
      )}
    </div>
  );
}

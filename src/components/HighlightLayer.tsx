interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface HighlightLayerProps {
  rects: Rect[];
  activeIndex: number;
  zoom: number;
}

export function HighlightLayer({ rects, activeIndex, zoom }: HighlightLayerProps) {
  if (rects.length === 0) return null;

  const scale = zoom / 100;

  return (
    <div className="highlight-layer">
      {rects.map((rect, i) => (
        <div
          key={i}
          className={
            i === activeIndex ? "search-highlight active" : "search-highlight"
          }
          style={{
            position: "absolute",
            left: rect.x * scale,
            top: rect.y * scale,
            width: rect.width * scale,
            height: rect.height * scale,
          }}
        />
      ))}
    </div>
  );
}

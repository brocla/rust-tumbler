import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface TextItem {
  text: string;
  x: number;
  y: number;
  width: number;
  height: number;
  fontSize: number;
}

interface TextLayerProps {
  docId: string;
  pageNumber: number;
  zoom: number;
}

export function TextLayer({
  docId,
  pageNumber,
  zoom,
}: TextLayerProps) {
  const [textItems, setTextItems] = useState<TextItem[]>([]);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const items = await invoke<TextItem[]>("extract_page_text", {
          docId,
          page: pageNumber,
        });
        if (!cancelled) setTextItems(items);
      } catch (err) {
        console.error(`Failed to extract text for page ${pageNumber}:`, err);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [docId, pageNumber]);

  const scale = zoom / 100;

  return (
    <div className="text-layer">
      {textItems.map((item, i) => (
        <span
          key={i}
          style={{
            position: "absolute",
            left: item.x * scale,
            top: item.y * scale,
            width: item.width * scale,
            height: item.height * scale,
            fontSize: item.fontSize * scale,
            lineHeight: `${item.height * scale}px`,
            fontFamily: "serif",
            color: "transparent",
            whiteSpace: "pre",
            userSelect: "text",
            WebkitUserSelect: "text",
          }}
        >
          {item.text}
        </span>
      ))}
    </div>
  );
}

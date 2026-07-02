import { useRef, useState } from "react";
import { Plus, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";
import { evictDoc } from "../utils/renderCache";
import { confirmCloseDirtyTab } from "../utils/saveDocument";

interface TabBarProps {
  onOpenFile: () => void;
}

export function TabBar({ onOpenFile }: TabBarProps) {
  const tabs = usePdfStore((s) => s.tabs);
  const activeTabId = usePdfStore((s) => s.activeTabId);
  const setActiveTab = usePdfStore((s) => s.setActiveTab);
  const removeTab = usePdfStore((s) => s.removeTab);
  const reorderTabs = usePdfStore((s) => s.reorderTabs);
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dropGap, setDropGap] = useState<number | null>(null);
  const chipRefs = useRef<(HTMLDivElement | null)[]>([]);

  if (tabs.length === 0) return null;

  const closeTab = async (tab: TabState) => {
    // Unsaved buffer edits (issue #31): Save / Don't Save / Cancel. A failed
    // save also aborts the close so the edits aren't silently lost.
    if (tab.isDirty && !(await confirmCloseDirtyTab(tab))) return;

    if (tab.metadataDirty) {
      const proceed = await confirm(
        `"${tab.fileName}" has unsaved metadata changes. Close anyway?`,
        { title: "Unsaved Changes", kind: "warning" },
      );
      if (!proceed) return;
    }

    try {
      await invoke("close_document", { docId: tab.docId });
    } catch (err) {
      console.error("Failed to close document:", err);
    }
    evictDoc(tab.docId);
    removeTab(tab.id);
  };

  // Determine the insertion gap (0..tabs.length) under the pointer by
  // comparing its x position to the horizontal midpoint of each tab chip.
  const gapForPointer = (clientX: number): number => {
    for (let i = 0; i < tabs.length; i++) {
      const rect = chipRefs.current[i]?.getBoundingClientRect();
      if (rect && clientX < rect.left + rect.width / 2) {
        return i;
      }
    }
    return tabs.length;
  };

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    if (dragIndex === null) return;
    setDropGap(gapForPointer(e.clientX));
  };

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    if (dragIndex !== null && dropGap !== null) {
      const toIndex = dropGap <= dragIndex ? dropGap : dropGap - 1;
      reorderTabs(dragIndex, toIndex);
    }
    setDragIndex(null);
    setDropGap(null);
  };

  return (
    <div className="tabbar" onDragOver={handleDragOver} onDrop={handleDrop}>
      {tabs.map((tab, index) => (
        <div
          key={tab.id}
          ref={(el) => {
            chipRefs.current[index] = el;
          }}
          className={[
            "tab-chip",
            tab.id === activeTabId ? "active" : "",
            dropGap === index ? "drop-before" : "",
            dropGap === tabs.length && index === tabs.length - 1
              ? "drop-after"
              : "",
          ]
            .filter(Boolean)
            .join(" ")}
          draggable
          onDragStart={(e) => {
            e.dataTransfer.effectAllowed = "move";
            e.dataTransfer.setData("text/plain", tab.id);
            setDragIndex(index);
          }}
          onDragEnd={() => {
            setDragIndex(null);
            setDropGap(null);
          }}
          onClick={() => setActiveTab(tab.id)}
          title={tab.fileName}
        >
          {(tab.isDirty || tab.metadataDirty) && <span className="tab-dirty-dot" />}
          <span className="tab-label">{tab.fileName}</span>
          <button
            className="tab-close-button"
            onClick={(e) => {
              e.stopPropagation();
              closeTab(tab);
            }}
            title="Close"
          >
            <X size={14} />
          </button>
        </div>
      ))}
      <button
        className="tab-new-button"
        onClick={onOpenFile}
        title="Open PDF (Ctrl+O)"
      >
        <Plus size={16} />
      </button>
    </div>
  );
}

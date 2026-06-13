import { FileText } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { ContinuousViewer } from "./ContinuousViewer";

export function ViewerArea() {
  const hasActiveTab = usePdfStore(
    (s) => s.activeTabId !== null && s.tabs.some((t) => t.id === s.activeTabId),
  );

  if (!hasActiveTab) {
    return (
      <div className="empty-state">
        <FileText size={64} className="empty-state-icon" />
        <div className="empty-state-text">No document open</div>
        <div className="empty-state-hint">
          Press Ctrl+O or click the open button to load a PDF
        </div>
      </div>
    );
  }

  return <ContinuousViewer />;
}

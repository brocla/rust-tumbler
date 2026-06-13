import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";

interface DocumentMetadata {
  title: string;
  author: string;
  subject: string;
  keywords: string;
  creator: string;
  producer: string;
  creationDate: string;
  modDate: string;
}

export function MetadataPanel() {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const [metadata, setMetadata] = useState<DocumentMetadata | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!activeTab?.docId) return;

    let cancelled = false;

    (async () => {
      try {
        const meta = await invoke<DocumentMetadata>("get_metadata", {
          docId: activeTab.docId,
        });
        if (!cancelled) {
          setMetadata(meta);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setError(String(err));
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [activeTab?.docId]);

  if (error) {
    return <div className="metadata-panel"><div className="metadata-error">{error}</div></div>;
  }

  if (!metadata) {
    return <div className="metadata-panel"><div className="metadata-loading">Loading metadata...</div></div>;
  }

  const fields = [
    { label: "Title", value: metadata.title },
    { label: "Author", value: metadata.author },
    { label: "Subject", value: metadata.subject },
    { label: "Keywords", value: metadata.keywords },
    { label: "Creator", value: metadata.creator },
    { label: "Producer", value: metadata.producer, readOnly: true },
    { label: "Created", value: metadata.creationDate, readOnly: true },
    { label: "Modified", value: metadata.modDate, readOnly: true },
  ];

  return (
    <div className="metadata-panel">
      {fields.map((field) => (
        <div key={field.label} className="metadata-field">
          <label className="metadata-label">{field.label}</label>
          <div className="metadata-value">{field.value || "—"}</div>
        </div>
      ))}
    </div>
  );
}

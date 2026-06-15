import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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

const EDITABLE_FIELDS = [
  { field: "title", label: "Title" },
  { field: "author", label: "Author" },
  { field: "subject", label: "Subject" },
  { field: "keywords", label: "Keywords" },
  { field: "creator", label: "Creator" },
] as const;

type EditableField = (typeof EDITABLE_FIELDS)[number]["field"];
type EditableValues = Record<EditableField, string>;

function pickEditable(meta: DocumentMetadata): EditableValues {
  return {
    title: meta.title,
    author: meta.author,
    subject: meta.subject,
    keywords: meta.keywords,
    creator: meta.creator,
  };
}

export function MetadataPanel() {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const updateTab = usePdfStore((s) => s.updateTab);
  const [metadata, setMetadata] = useState<DocumentMetadata | null>(null);
  const [edited, setEdited] = useState<EditableValues | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!activeTab?.docId) return;

    let cancelled = false;
    const docId = activeTab.docId;
    const tabId = activeTab.id;

    const load = async () => {
      try {
        const meta = await invoke<DocumentMetadata>("get_metadata", { docId });
        if (!cancelled) {
          setMetadata(meta);
          setEdited(pickEditable(meta));
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setError(String(err));
        }
      }
    };

    load();

    // Another tab may have edited metadata for this same underlying file.
    // Skip the refresh if this tab has unsaved edits of its own — otherwise
    // we'd silently overwrite them with the reloaded values.
    const unlisten = listen<string[]>("document-metadata-changed", (event) => {
      if (!event.payload.includes(docId)) return;
      const tab = usePdfStore.getState().tabs.find((t) => t.id === tabId);
      if (tab?.metadataDirty) return;
      load();
    });

    return () => {
      cancelled = true;
      unlisten.then((f) => f());
    };
  }, [activeTab?.docId]);

  if (error) {
    return <div className="metadata-panel"><div className="metadata-error">{error}</div></div>;
  }

  if (!metadata || !edited || !activeTab) {
    return <div className="metadata-panel"><div className="metadata-loading">Loading metadata...</div></div>;
  }

  const dirty = EDITABLE_FIELDS.some(({ field }) => edited[field] !== metadata[field]);

  const handleChange = (field: EditableField, value: string) => {
    const next = { ...edited, [field]: value };
    setEdited(next);
    const nowDirty = EDITABLE_FIELDS.some((f) => next[f.field] !== metadata[f.field]);
    if (nowDirty !== activeTab.metadataDirty) {
      updateTab(activeTab.id, { metadataDirty: nowDirty });
    }
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      const updated = await invoke<DocumentMetadata>("set_metadata", {
        docId: activeTab.docId,
        metadata: edited,
      });
      setMetadata(updated);
      setEdited(pickEditable(updated));
      updateTab(activeTab.id, { metadataDirty: false });
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const readOnlyFields = [
    { label: "Producer", value: metadata.producer },
    { label: "Created", value: metadata.creationDate },
    { label: "Modified", value: metadata.modDate },
  ];

  return (
    <div className="metadata-panel">
      {EDITABLE_FIELDS.map(({ field, label }) => (
        <div key={field} className="metadata-field">
          <label className="metadata-label" htmlFor={`metadata-${field}`}>{label}</label>
          <input
            id={`metadata-${field}`}
            className="metadata-input"
            type="text"
            value={edited[field]}
            onChange={(e) => handleChange(field, e.target.value)}
          />
        </div>
      ))}
      {readOnlyFields.map((field) => (
        <div key={field.label} className="metadata-field">
          <label className="metadata-label">{field.label}</label>
          <div className="metadata-value">{field.value || "—"}</div>
        </div>
      ))}
      {dirty && (
        <button className="metadata-save-button" onClick={handleSave} disabled={saving}>
          {saving ? "Saving..." : "Save"}
        </button>
      )}
    </div>
  );
}

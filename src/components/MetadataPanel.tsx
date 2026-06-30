import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { usePdfStore } from "../store/usePdfStore";
import { confirmBreakingEdit } from "../utils/confirmBreakingEdit";
import {
  isSigned,
  SIGNATURE_EDIT_WARNING,
  type SignatureInfo,
} from "../utils/signature";

/** One read-only line summarising the document's signatures, honest about
 * meaning intact (not trusted). "" when unsigned. */
function describeSignatures(info: SignatureInfo | null): string {
  if (!info || !info.count || !info.signatures) return "";
  return info.signatures
    .map((s) => {
      const who = s.signerName || "Unknown signer";
      const state = !s.integrityOk
        ? "could not be verified"
        : s.modifiedAfter
          ? "intact, but modified after signing"
          : "intact";
      return `Signed by ${who} — ${state}`;
    })
    .join("; ");
}

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

interface ConformanceClaims {
  // Honest, display-ready labels, e.g. "PDF/A-2b". Empty when the file declares
  // no recognized ISO sub-format conformance.
  declared: string[];
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

// One-word plain-language gloss for each ISO sub-format family, matched by code
// prefix, so the Conformance row is meaningful to people who don't know the
// codes. PDF/UA is listed before PDF/A only for readability; the prefixes don't
// overlap ("PDF/UA-1" does not start with "PDF/A").
const STANDARD_GLOSS: { prefix: string; gloss: string }[] = [
  { prefix: "PDF/A", gloss: "archiving" },
  { prefix: "PDF/X", gloss: "print" },
  { prefix: "PDF/UA", gloss: "accessibility" },
  { prefix: "PDF/E", gloss: "engineering" },
];

function describeClaim(code: string): string {
  const match = STANDARD_GLOSS.find((s) => code.startsWith(s.prefix));
  return match ? `${code} (${match.gloss})` : code;
}

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
  const [conformance, setConformance] = useState<string[]>([]);
  const [signature, setSignature] = useState<SignatureInfo | null>(null);
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
      // Declared ISO conformance is derived (read-only) and independent of the
      // editable fields; a failure here shouldn't block metadata display.
      try {
        const claims = await invoke<ConformanceClaims>("get_conformance", { docId });
        if (!cancelled) setConformance(claims?.declared ?? []);
      } catch {
        if (!cancelled) setConformance([]);
      }
      // Signature verification is likewise read-only/derived.
      try {
        const sig = await invoke<SignatureInfo>("get_signature_info", { docId });
        if (!cancelled) setSignature(sig);
      } catch {
        if (!cancelled) setSignature(null);
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
    // Saving rewrites the file, which invalidates any digital signature. Warn
    // first; the warning is overridable.
    if (isSigned(activeTab.signatureStatus)) {
      const proceed = await confirmBreakingEdit(SIGNATURE_EDIT_WARNING);
      if (!proceed) return;
    }
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
      // The file changed — re-verify so the panel and badge reflect that the
      // signature (if any) is now broken.
      try {
        const sig = await invoke<SignatureInfo>("get_signature_info", {
          docId: activeTab.docId,
        });
        setSignature(sig);
        updateTab(activeTab.id, { signatureStatus: sig.status });
      } catch {
        /* best-effort */
      }
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
    // Honest wording: we report only what the file declares, not validated
    // compliance. Joined claims read e.g. "Declares PDF/A-2b, PDF/UA-1".
    {
      label: "Conformance",
      value:
        conformance.length > 0
          ? `Declares ${conformance.map(describeClaim).join(", ")}`
          : "",
    },
    // Read-only signature summary. Honest: "intact" means cryptographically
    // unchanged, not that the signer is trusted.
    { label: "Signature", value: describeSignatures(signature) },
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

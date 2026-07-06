import { useState } from "react";
import { usePdfStore } from "../store/usePdfStore";
import { discardRedaction, saveRedactedCopyAs } from "../utils/redactSave";

/**
 * Banner shown while the viewer is previewing a staged redacted copy
 * (issue #1). The pages on screen are the real flattened output — the burned
 * boxes are in the pixels, not an overlay — but nothing has been written yet:
 * Save As writes the copy, Discard drops it. A failed verification still
 * previews (so leaks can be inspected) but cannot be saved.
 */
export function RedactPreviewBanner() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));
  const [saving, setSaving] = useState(false);

  if (!activeTab?.redactPreview) return null;
  const verified = activeTab.redactPreview.verified;

  const handleSave = async () => {
    setSaving(true);
    try {
      await saveRedactedCopyAs(activeTab);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className={`redact-preview-banner ${verified ? "verified" : "failed"}`} role="status">
      <span className="redact-preview-text">
        {verified
          ? "Previewing redacted copy — verified, nothing recoverable. The original is untouched."
          : "Previewing redacted copy — verification FAILED; saving is blocked."}
      </span>
      <span className="redact-preview-actions">
        <button onClick={handleSave} disabled={saving || !verified}>
          {saving ? "Saving…" : "Save As…"}
        </button>
        <button onClick={() => void discardRedaction(activeTab)} disabled={saving}>
          Discard
        </button>
      </span>
    </div>
  );
}

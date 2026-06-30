import { usePdfStore } from "../store/usePdfStore";
import { signatureBadge } from "../utils/signature";

/**
 * Thin bottom border strip. Currently shows the digital-signature badge for the
 * **active tab only** (issue #17) — it reads the active tab's signatureStatus,
 * so switching tabs swaps the badge and unsigned tabs show nothing.
 */
export function StatusBar() {
  const status = usePdfStore((s) => {
    const tab = s.tabs.find((t) => t.id === s.activeTabId);
    return tab?.signatureStatus;
  });

  const badge = signatureBadge(status);

  return (
    <div className="app-status-bar">
      {badge && (
        <span className={`signature-badge signature-badge-${badge.kind}`}>
          {badge.text}
        </span>
      )}
    </div>
  );
}

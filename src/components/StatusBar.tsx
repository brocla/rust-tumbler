import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";
import { signatureBadge } from "../utils/signature";

/**
 * Thin bottom border strip, right-justified. Shows (right to left): the app
 * version number (always, in the theme accent color), then the digital-signature
 * badge for the **active tab only** (issue #17) when present. The version stays
 * rightmost; the signing statement sits to its left.
 */
export function StatusBar() {
  const status = usePdfStore((s) => {
    const tab = s.tabs.find((t) => t.id === s.activeTabId);
    return tab?.signatureStatus;
  });

  const badge = signatureBadge(status);

  const [version, setVersion] = useState("");
  useEffect(() => {
    invoke<string>("get_app_version")
      .then(setVersion)
      .catch(() => {});
  }, []);

  return (
    <div className="app-status-bar">
      {badge && (
        <span className={`signature-badge signature-badge-${badge.kind}`}>
          {badge.text}
        </span>
      )}
      {version && <span className="status-version">v{version}</span>}
    </div>
  );
}

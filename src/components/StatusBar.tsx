import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Lock } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { signatureBadge } from "../utils/signature";

/**
 * Thin bottom border strip, right-justified. Shows (right to left): the app
 * version number (always, in the theme accent color), then the digital-signature
 * badge and, for a password-protected document (issue #12), a view-only lock
 * badge — both for the **active tab only** (issue #17). The version stays
 * rightmost.
 */
export function StatusBar() {
  const tab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));
  const status = tab?.signatureStatus;
  const encrypted = !!tab?.encrypted;

  const badge = signatureBadge(status);

  const [version, setVersion] = useState("");
  useEffect(() => {
    invoke<string>("get_app_version")
      .then(setVersion)
      .catch(() => {});
  }, []);

  return (
    <div className="app-status-bar">
      {encrypted && (
        <span className="encrypted-badge" title="Password-protected — view only">
          <Lock size={12} />
          Encrypted — view only
        </span>
      )}
      {badge && (
        <span className={`signature-badge signature-badge-${badge.kind}`}>
          {badge.text}
        </span>
      )}
      {version && <span className="status-version">v{version}</span>}
    </div>
  );
}

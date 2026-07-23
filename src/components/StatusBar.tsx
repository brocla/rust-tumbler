import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Lock, Zap } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { signatureBadge } from "../utils/signature";
import { BirthdayNotes } from "./BirthdayNotes";

/**
 * Thin bottom border strip, right-justified. Shows (right to left): the app
 * version number (always, in the theme accent color), then the digital-signature
 * badge, a lock badge for a password-protected document (issues #12/#57), and a
 * "Linearized" badge (issue #3) when the open file carries that structure —
 * all for the **active tab only** (issue #17). The version stays rightmost.
 */
export function StatusBar() {
  const tab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));
  const status = tab?.signatureStatus;
  const encrypted = !!tab?.encrypted;
  const linearized = !!tab?.linearized;
  const birthdayEgg = usePdfStore((s) => s.birthdayEgg);

  const badge = signatureBadge(status);

  const [version, setVersion] = useState("");
  useEffect(() => {
    invoke<string>("get_app_version")
      .then(setVersion)
      .catch(() => {});
  }, []);

  return (
    <div className="app-status-bar">
      {/* Easter egg (see MarginsPanel): the Expand Margins tool was a
          birthday gift — triple-clicking its explainer summons this. The
          greeting slides in and shimmers (CSS) while a burst of musical
          notes rises from the bar. */}
      {birthdayEgg && (
        <>
          <span className="birthday-egg">🎂🎂🎂 Happy Birthday Julie! 🎂🎂🎂</span>
          <BirthdayNotes />
        </>
      )}
      {encrypted && (
        <span
          className="encrypted-badge"
          title="Password-protected — saving keeps the password. Use the toolbar unlock button to remove it."
        >
          <Lock size={12} />
          Encrypted
        </span>
      )}
      {badge && (
        <span className={`signature-badge signature-badge-${badge.kind}`}>
          {badge.text}
        </span>
      )}
      {linearized && (
        <span
          className="linearized-badge"
          title="This file is linearized (Fast Web View) — a viewer streaming it over the web can render page 1 before the rest downloads. Any edit turns this off until you save a new linearized copy."
        >
          <Zap size={12} />
          Linearized
        </span>
      )}
      {version && <span className="status-version">v{version}</span>}
    </div>
  );
}

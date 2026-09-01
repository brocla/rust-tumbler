import { useCallback, useEffect, useRef, useState } from "react";
import { Undo2, Redo2, Check } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { commitOpenInk } from "../utils/inkCommit";
import { confirmBreakingEdit } from "../utils/confirmBreakingEdit";
import { isSigned, SIGNATURE_EDIT_WARNING } from "../utils/signature";

/**
 * Ink Signature panel (issue #120) — the tool's controls, and the owner of
 * every rule about when a stroke group closes.
 *
 * Strokes accumulate in the store while the tool is open, and are flattened
 * into the page content stream when the group closes: switching page, switching
 * tab, leaving the tool, or pressing Done. Esc throws the group away instead.
 * Save commits first, from the save path itself — a signature drawn and not
 * committed would be missing from the written file.
 *
 * This is not a digital signature, and nothing here should suggest it is. It
 * draws ink; that ink breaks any existing cryptographic signature exactly as
 * any other edit does, which is what the warning on activation is for.
 */
export function InkPanel() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));
  const ink = usePdfStore((s) => s.ink);
  const inkUndo = usePdfStore((s) => s.inkUndo);
  const inkRedo = usePdfStore((s) => s.inkRedo);
  const inkDiscard = usePdfStore((s) => s.inkDiscard);
  const setSidebarTool = usePdfStore((s) => s.setSidebarTool);

  const tabId = activeTab?.id ?? "";
  const currentPage = activeTab?.currentPage ?? 1;

  const [error, setError] = useState<string | null>(null);

  const strokeCount = ink?.strokes.length ?? 0;
  const redoCount = ink?.redo.length ?? 0;

  const commit = useCallback(() => {
    commitOpenInk().catch((e) => setError(String(e)));
  }, []);

  // Drawing rewrites the file, which invalidates any digital signature. Warn
  // once when the tool opens — before any ink is drawn — rather than after,
  // when refusing would mean throwing the user's signature away. Declining
  // closes the tool.
  const warnedRef = useRef(false);
  useEffect(() => {
    if (warnedRef.current || !activeTab) return;
    if (!isSigned(activeTab.signatureStatus)) return;
    warnedRef.current = true;
    void confirmBreakingEdit(SIGNATURE_EDIT_WARNING).then((proceed) => {
      if (!proceed) setSidebarTool("ink");
    });
  }, [activeTab, setSidebarTool]);

  // Commit when the page or the tab changes, and when the tool closes. The
  // group carries its own docId and page, so a commit triggered after the view
  // has moved still lands on the page the ink was drawn on.
  useEffect(() => {
    return () => commit();
  }, [currentPage, tabId, commit]);

  // Ctrl+Z / Ctrl+Y, but only while this tool is open and only when the user
  // is not typing somewhere — otherwise undo in a form field or a typewriter
  // note would silently remove a signature stroke instead.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName) || target.isContentEditable)
      ) {
        return;
      }
      if (e.key === "Escape") {
        // Esc abandons the group rather than committing it — the same meaning
        // it already has in the form-field signature canvas.
        inkDiscard();
        return;
      }
      if (!(e.ctrlKey || e.metaKey)) return;
      const key = e.key.toLowerCase();
      if (key === "z") {
        e.preventDefault();
        inkUndo();
      } else if (key === "y") {
        e.preventDefault();
        inkRedo();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [inkUndo, inkRedo, inkDiscard]);

  return (
    <div className="search-panel">
      <p className="ink-panel-hint">
        Draw on page {currentPage} to sign it. One blue, one width — undo is the
        only eraser.
      </p>

      {error && <p className="ink-panel-blocked">Couldn't apply ink: {error}</p>}

      <div className="ink-panel-row">
        <button
          className="toolbar-button"
          onClick={inkUndo}
          disabled={strokeCount === 0}
          title="Undo stroke (Ctrl+Z)"
        >
          <Undo2 size={16} />
        </button>
        <button
          className="toolbar-button"
          onClick={inkRedo}
          disabled={redoCount === 0}
          title="Redo stroke (Ctrl+Y)"
        >
          <Redo2 size={16} />
        </button>
        <button
          className="toolbar-button"
          onClick={commit}
          disabled={strokeCount === 0}
          title="Apply the signature to the page"
        >
          <Check size={16} /> Done
        </button>
      </div>

      <p className="ink-panel-hint">
        {strokeCount === 0
          ? "Nothing drawn yet."
          : `${strokeCount} stroke${strokeCount === 1 ? "" : "s"} pending. Esc discards them.`}
      </p>

      <p className="ink-panel-hint">
        Ink becomes part of the page and cannot be edited afterwards. It is not a
        digital signature, and it invalidates any existing one.
      </p>
    </div>
  );
}

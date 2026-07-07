import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CaseSensitive, WholeWord, Regex } from "lucide-react";
import { message } from "@tauri-apps/plugin-dialog";
import { usePdfStore } from "../store/usePdfStore";
import type { RedactRegion } from "../store/usePdfStore";
import { selectionToRegions } from "../utils/redaction";
import {
  discardRedaction,
  redactPreviewCacheId,
  saveRedactedCopyAs,
} from "../utils/redactSave";
import { evictDoc } from "../utils/renderCache";

/** Mirrors the backend's `RedactionResult` (serde camelCase). */
interface RedactionResult {
  regions: number;
  pagesFlattened: number;
  verified: boolean;
  leaks: RedactRegion[];
  // Failed structural postconditions (fail-closed check 4) — document-level
  // leak vectors like a surviving structure tree. Empty when verified.
  structuralViolations: string[];
  ocrCheckRan: boolean;
  reocrPages: number;
  cancelled: boolean;
}

/** Sorted unique page numbers of the leaked regions, for the failed banner. */
function leakPages(leaks: RedactRegion[]): number[] {
  return [...new Set(leaks.map((l) => l.page))].sort((a, b) => a - b);
}

export function RedactPanel() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));
  const updateTab = usePdfStore((s) => s.updateTab);
  const drawMode = usePdfStore((s) => s.redactDrawMode);
  const setRedactDrawMode = usePdfStore((s) => s.setRedactDrawMode);
  const addRedactRegions = usePdfStore((s) => s.addRedactRegions);
  const removeRedactRegion = usePdfStore((s) => s.removeRedactRegion);
  const clearRedactRegions = usePdfStore((s) => s.clearRedactRegions);

  const [query, setQuery] = useState("");
  const [findCount, setFindCount] = useState<number | null>(null);
  const [matchCase, setMatchCase] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [useRegex, setUseRegex] = useState(false);
  const [targetDpi, setTargetDpi] = useState(200);
  const [running, setRunning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [result, setResult] = useState<RedactionResult | null>(null);
  const [saved, setSaved] = useState(false);

  // Reset per-document state when the active document changes (the panel stays
  // mounted across tab switches — mirror OptimizePanel).
  const activeDocId = activeTab?.docId;
  useEffect(() => {
    setQuery("");
    setFindCount(null);
    setResult(null);
    setSaved(false);
    setRunning(false);
    setSaving(false);
    usePdfStore.getState().setRedactDrawMode(false);
  }, [activeDocId]);

  // Draw mode is a page-interaction mode; disarm it when the panel unmounts
  // (tool switched away) so the viewer isn't left capturing drags.
  useEffect(() => () => usePdfStore.getState().setRedactDrawMode(false), []);

  // Ctrl+release-to-redact: while the panel is open, finishing a text
  // selection with Ctrl held converts it straight to regions — no round-trip
  // to the "Redact selected text" button between selections. (WebView2 has no
  // native multi-range selection, so regions accumulate one drag at a time.)
  const previewing = !!activeTab?.redactPreview;
  const zoom = activeTab?.zoom ?? 100;
  useEffect(() => {
    if (!activeDocId || running || previewing) return;
    const handleMouseUp = (e: MouseEvent) => {
      if (!e.ctrlKey) return;
      const found = selectionToRegions(zoom);
      if (found.length === 0) return; // a plain Ctrl+click, or no selection
      usePdfStore.getState().addRedactRegions(activeDocId, found);
      window.getSelection()?.removeAllRanges();
      setResult(null);
      setSaved(false);
    };
    window.addEventListener("mouseup", handleMouseUp);
    return () => window.removeEventListener("mouseup", handleMouseUp);
  }, [activeDocId, zoom, running, previewing]);

  if (!activeTab) return null;
  const docId = activeTab.docId;
  const regions = activeTab.redactRegions ?? [];

  const handleFind = async () => {
    if (!query.trim()) return;
    try {
      const found = await invoke<RedactRegion[]>("find_redaction_matches", {
        docId,
        query,
        matchCase,
        wholeWord,
        useRegex,
      });
      setFindCount(found.length);
      if (found.length > 0) {
        addRedactRegions(docId, found);
        // Remember the query so verification can assert zero hits for it in
        // the saved output — but only for a plain substring search. A
        // match-case / whole-word / regex find deliberately marks a *subset*
        // of occurrences, and the output check (a case-insensitive literal
        // search) would flag the intentionally-kept ones as leaks and block
        // the save.
        const plainSearch = !matchCase && !wholeWord && !useRegex;
        const queries = activeTab.redactQueries ?? [];
        if (plainSearch && !queries.includes(query)) {
          updateTab(activeTab.id, { redactQueries: [...queries, query] });
        }
        setResult(null);
        setSaved(false);
      }
    } catch (err) {
      await message(String(err), { title: "Find Failed", kind: "error" });
    }
  };

  const handleRedactSelection = () => {
    const found = selectionToRegions(activeTab.zoom);
    if (found.length === 0) {
      usePdfStore.getState().showNotice("Select text in the document first.");
      return;
    }
    addRedactRegions(docId, found);
    window.getSelection()?.removeAllRanges();
    setResult(null);
    setSaved(false);
  };

  const handleApply = async () => {
    if (regions.length === 0) return;
    setRunning(true);
    setSaved(false);
    try {
      const res = await invoke<RedactionResult>("apply_redactions", {
        docId,
        regions,
        verifyQueries: activeTab.redactQueries ?? [],
        targetDpi,
      });
      if (!res.cancelled) {
        setResult(res);
        // Fresh staging replaces any previous one — drop its cached previews,
        // then enter (or refresh) the preview so the user sees the real
        // flattened pages with the boxes burned in.
        evictDoc(redactPreviewCacheId(docId));
        updateTab(activeTab.id, { redactPreview: { verified: res.verified } });
      }
    } catch (err) {
      await message(String(err), { title: "Redaction Failed", kind: "error" });
    } finally {
      setRunning(false);
      usePdfStore.getState().setRedactProgress(null);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      if (await saveRedactedCopyAs(activeTab)) {
        setSaved(true);
        setResult(null);
      }
    } finally {
      setSaving(false);
    }
  };

  const handleDiscard = async () => {
    await discardRedaction(activeTab);
    setResult(null);
  };

  return (
    <div className="redact-panel">
      <div className="redact-explainer">
        Redaction is permanent: marked areas are removed from a <strong>copy</strong> of
        the document, saved under a new name. Redacted pages are converted to
        images and re-OCR&#8217;d for search; tags, bookmarks, and metadata are
        removed from the copy. The original file is never modified.
      </div>

      <div className="redact-find">
        <input
          className="redact-find-input"
          type="text"
          placeholder="Find text to redact…"
          value={query}
          disabled={running || previewing}
          onChange={(e) => {
            setQuery(e.target.value);
            setFindCount(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") void handleFind();
          }}
        />
        <button
          className="redact-find-button"
          onClick={handleFind}
          disabled={running || previewing || !query.trim()}
        >
          Redact all
        </button>
      </div>
      <div className="search-mode-row">
        <button
          className={`toolbar-button${matchCase ? " active" : ""}`}
          onClick={() => setMatchCase((v) => !v)}
          title="Match case"
          aria-pressed={matchCase}
          disabled={running || previewing}
        >
          <CaseSensitive size={16} />
        </button>
        <button
          className={`toolbar-button${wholeWord ? " active" : ""}`}
          onClick={() => setWholeWord((v) => !v)}
          title="Whole word"
          aria-pressed={wholeWord}
          disabled={running || previewing}
        >
          <WholeWord size={16} />
        </button>
        <button
          className={`toolbar-button${useRegex ? " active" : ""}`}
          onClick={() => setUseRegex((v) => !v)}
          title="Regular expression"
          aria-pressed={useRegex}
          disabled={running || previewing}
        >
          <Regex size={16} />
        </button>
      </div>
      {findCount !== null && (
        <div className="redact-find-count">
          {findCount === 0
            ? "No matches found."
            : `${findCount} occurrence${findCount === 1 ? "" : "s"} marked.`}
        </div>
      )}

      <div className="redact-add-buttons">
        <button
          onClick={handleRedactSelection}
          disabled={running || previewing}
          title="Turn the current text selection into redaction boxes"
        >
          Redact selected text
        </button>
        <button
          className={drawMode ? "active" : ""}
          onClick={() => setRedactDrawMode(!drawMode)}
          disabled={running || previewing}
          title="Drag a rectangle on the page to redact an area (images, signatures)"
        >
          {drawMode ? "Drawing — click to stop" : "Draw region"}
        </button>
      </div>
      <div className="redact-hint">
        Tip: hold Ctrl while selecting text to mark it instantly.
      </div>

      <div className="redact-region-list">
        <div className="redact-region-header">
          <span>
            {regions.length} region{regions.length === 1 ? "" : "s"} marked
          </span>
          {regions.length > 0 && (
            <button
              className="redact-clear-button"
              onClick={() => {
                clearRedactRegions(docId);
                setResult(null);
                setFindCount(null);
              }}
              disabled={running || previewing}
            >
              Clear all
            </button>
          )}
        </div>
        {regions.map((region, index) => (
          <div key={index} className="redact-region-row">
            <span>
              Page {region.page} — {Math.round(region.rect.width)}×
              {Math.round(region.rect.height)} pt
            </span>
            <button
              title="Remove this region"
              onClick={() => removeRedactRegion(docId, index)}
              disabled={running || previewing}
            >
              ✕
            </button>
          </div>
        ))}
      </div>

      <div className="redact-dpi optimize-slider">
        <label>Flatten DPI: {targetDpi}</label>
        <input
          type="range"
          min={100}
          max={400}
          step={25}
          value={targetDpi}
          disabled={running || previewing}
          onChange={(e) => setTargetDpi(Number(e.target.value))}
        />
      </div>

      {!previewing && (
        <button
          className="redact-apply-button"
          onClick={handleApply}
          disabled={running || regions.length === 0}
        >
          {running ? "Applying…" : "Apply redactions"}
        </button>
      )}

      {result && (
        <div
          className={`redact-verdict ${result.verified ? "verified" : "failed"}`}
          role="status"
        >
          {result.verified ? (
            <>
              ✓ Verified — nothing recoverable in {result.regions} region
              {result.regions === 1 ? "" : "s"} across {result.pagesFlattened} page
              {result.pagesFlattened === 1 ? "" : "s"}.
              {!result.ocrCheckRan &&
                " (OCR spot-check skipped — no OCR language pack installed.)"}
            </>
          ) : (
            <>
              ✗ Verification FAILED — saving is blocked.
              {result.leaks.length > 0 &&
                ` Recoverable content remains in ${result.leaks.length} region${
                  result.leaks.length === 1 ? "" : "s"
                } (page${leakPages(result.leaks).length === 1 ? "" : "s"} ${leakPages(
                  result.leaks,
                ).join(", ")}).`}
              {result.structuralViolations.length > 0 && (
                <ul className="redact-violations">
                  {result.structuralViolations.map((v, i) => (
                    <li key={i}>{v}</li>
                  ))}
                </ul>
              )}
            </>
          )}
        </div>
      )}

      {saved && <div className="redact-saved">✓ Redacted copy saved</div>}

      {previewing && (
        <div className="redact-actions">
          <button
            className="redact-save-button"
            onClick={handleSave}
            disabled={saving || !activeTab.redactPreview?.verified}
          >
            {saving ? "Saving…" : "Save As…"}
          </button>
          <button className="redact-discard-button" onClick={handleDiscard} disabled={saving}>
            Discard
          </button>
        </div>
      )}
    </div>
  );
}

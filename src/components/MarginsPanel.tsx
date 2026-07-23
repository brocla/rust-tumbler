import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { message } from "@tauri-apps/plugin-dialog";
import { Expand } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { confirmBreakingEdit } from "../utils/confirmBreakingEdit";
import { isSigned, SIGNATURE_EDIT_WARNING } from "../utils/signature";

// Mirrors the Rust `MarginsReport` types (serde camelCase). Boxes are in
// display points, origin bottom-left, y up; `bbox` is null for a blank page.
interface InkBbox {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

interface PageMargins {
  pageW: number;
  pageH: number;
  bbox: InkBbox | null;
}

interface MarginsReport {
  pages: PageMargins[];
  cancelled: boolean;
}

interface ExpandResult {
  scale: number;
  cancelled: boolean;
}

interface MarginsProgress {
  page: number;
  pageCount: number;
}

/// Mirror of the backend's uniform-scale rule, so the padding slider updates
/// the readout live without re-invoking the analysis.
export function uniformScale(pages: PageMargins[], paddingPt: number): number | null {
  let s = Infinity;
  for (const p of pages) {
    if (!p.bbox) continue;
    const bw = p.bbox.x1 - p.bbox.x0;
    const bh = p.bbox.y1 - p.bbox.y0;
    if (bw <= 0 || bh <= 0) continue;
    s = Math.min(
      s,
      Math.max(p.pageW - 2 * paddingPt, 1) / bw,
      Math.max(p.pageH - 2 * paddingPt, 1) / bh,
    );
  }
  return Number.isFinite(s) ? s : null;
}

/// The smallest margin found on any non-blank page, per side, in points.
export function smallestMargins(
  pages: PageMargins[],
): { left: number; right: number; top: number; bottom: number } | null {
  let result: { left: number; right: number; top: number; bottom: number } | null = null;
  for (const p of pages) {
    if (!p.bbox) continue;
    const m = {
      left: p.bbox.x0,
      right: p.pageW - p.bbox.x1,
      top: p.pageH - p.bbox.y1,
      bottom: p.bbox.y0,
    };
    result = result
      ? {
          left: Math.min(result.left, m.left),
          right: Math.min(result.right, m.right),
          top: Math.min(result.top, m.top),
          bottom: Math.min(result.bottom, m.bottom),
        }
      : m;
  }
  return result;
}

function inches(pt: number): string {
  return `${(pt / 72).toFixed(2)}″`;
}

// Below this the fit is a no-op (or a shrink) — nothing worth applying.
const MIN_GAIN = 1.005;

export function MarginsPanel() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));

  const [report, setReport] = useState<MarginsReport | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [applying, setApplying] = useState(false);
  const [progress, setProgress] = useState<MarginsProgress | null>(null);
  const [paddingInches, setPaddingInches] = useState(0.25);
  const [appliedScale, setAppliedScale] = useState<number | null>(null);
  // Bumped to re-run a cancelled/failed analysis on demand.
  const [analysisEpoch, setAnalysisEpoch] = useState(0);

  const activeDocId = activeTab?.docId;
  const pagesVersion = activeTab?.pagesVersion ?? 0;

  useEffect(() => {
    const unlisten = listen<MarginsProgress>("margins-progress", (event) => {
      setProgress(event.payload);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Applied confirmations belong to one document only.
  useEffect(() => {
    setAppliedScale(null);
  }, [activeDocId]);

  // Analyze on document change and after any page edit (pagesVersion bumps on
  // every buffer edit, including our own apply — which conveniently refreshes
  // the readout to its post-fit state).
  const runningRef = useRef(false);
  useEffect(() => {
    if (!activeDocId) {
      setReport(null);
      return;
    }
    let stale = false;
    setAnalyzing(true);
    setReport(null);
    invoke<MarginsReport>("analyze_margins", { docId: activeDocId })
      .then((r) => {
        if (!stale) setReport(r);
      })
      .catch(async (err) => {
        if (!stale) await message(String(err), { title: "Margin Analysis Failed", kind: "error" });
      })
      .finally(() => {
        if (!stale) {
          setAnalyzing(false);
          setProgress(null);
        }
      });
    return () => {
      stale = true;
    };
  }, [activeDocId, pagesVersion, analysisEpoch]);

  if (!activeTab) return null;
  const docId = activeTab.docId;

  const paddingPt = paddingInches * 72;
  const pages = report && !report.cancelled ? report.pages : null;
  const scale = pages ? uniformScale(pages, paddingPt) : null;
  const margins = pages ? smallestMargins(pages) : null;
  const gainPercent = scale ? Math.round((scale - 1) * 100) : 0;
  const canApply = scale !== null && scale > MIN_GAIN && !applying && !analyzing;

  const handleApply = async () => {
    if (runningRef.current) return;
    // Rewriting page content voids an existing digital signature — same
    // overridable warning the other content edits show.
    if (isSigned(activeTab.signatureStatus)) {
      const proceed = await confirmBreakingEdit(SIGNATURE_EDIT_WARNING);
      if (!proceed) return;
    }
    runningRef.current = true;
    setApplying(true);
    setAppliedScale(null);
    try {
      const result = await invoke<ExpandResult>("expand_margins", { docId, paddingPt });
      if (!result.cancelled) setAppliedScale(result.scale);
    } catch (err) {
      await message(String(err), { title: "Expand Margins Failed", kind: "error" });
    } finally {
      runningRef.current = false;
      setApplying(false);
      setProgress(null);
    }
  };

  const handleCancel = () => {
    void invoke("cancel_margins");
  };

  const busy = analyzing || applying;

  return (
    <div className="margins-panel">
      <div className="margins-explainer">
        Enlarges the page content to fill the margins — useful for sheet music
        engraved small inside wide borders. Every page is scaled by the same
        factor (limited by the fullest page), re-centered, and top-aligned. The
        change is applied to the open document; use Save / Save As to keep it.
      </div>

      {busy && (
        <div className="margins-status">
          {progress
            ? `${applying ? "Applying" : "Analyzing"}… page ${progress.page} of ${progress.pageCount}`
            : applying
              ? "Applying…"
              : "Analyzing…"}
          <button className="margins-cancel-button" onClick={handleCancel}>
            Cancel
          </button>
        </div>
      )}

      {!busy && report?.cancelled && (
        <div className="margins-status">
          Analysis cancelled.
          <button
            className="margins-cancel-button"
            onClick={() => setAnalysisEpoch((e) => e + 1)}
          >
            Retry
          </button>
        </div>
      )}

      {!busy && pages && (
        <>
          {margins ? (
            <div className="margins-readout">
              <div className="margins-row">
                <span>Smallest margins</span>
                <span>
                  L {inches(margins.left)} · R {inches(margins.right)} · T{" "}
                  {inches(margins.top)} · B {inches(margins.bottom)}
                </span>
              </div>
              <div className="margins-row">
                <span>Content enlargement</span>
                <span className="margins-gain">
                  {scale !== null && scale > MIN_GAIN
                    ? `+${gainPercent}%`
                    : "already fills the page"}
                </span>
              </div>
            </div>
          ) : (
            <div className="margins-readout">No page content detected.</div>
          )}

          <div className="margins-slider">
            <label>Target margin: {paddingInches.toFixed(2)}″</label>
            <input
              type="range"
              min={0}
              max={0.5}
              step={0.05}
              value={paddingInches}
              onChange={(e) => setPaddingInches(Number(e.target.value))}
            />
          </div>

          <button className="margins-apply-button" onClick={handleApply} disabled={!canApply}>
            <Expand size={16} />
            Expand Content
          </button>

          {appliedScale !== null && (
            <div className="margins-applied">
              ✓ Content enlarged {Math.round((appliedScale - 1) * 100)}% — unsaved
            </div>
          )}
        </>
      )}
    </div>
  );
}

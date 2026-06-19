import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save, message } from "@tauri-apps/plugin-dialog";
import { usePdfStore } from "../store/usePdfStore";

// StepId values mirror the Rust `StepId` enum (serde snake_case).
type StepId =
  | "recompress_streams"
  | "prune_unused"
  | "delete_zero_length"
  | "strip_extras"
  | "recompress_images";

interface StepDef {
  id: StepId;
  label: string;
  description: string;
  /** Image recompression lands in a later commit; rendered but not runnable. */
  comingSoon?: boolean;
}

// The four lopdf-only steps are runnable now; image recompression is staged
// for a later commit and shown disabled so the panel telegraphs what's coming.
const STEPS: StepDef[] = [
  {
    id: "recompress_streams",
    label: "Recompress streams",
    description: "Re-deflate content streams — the cheapest, safest win.",
  },
  {
    id: "prune_unused",
    label: "Prune unused objects",
    description: "Remove orphaned objects left behind by editors.",
  },
  {
    id: "delete_zero_length",
    label: "Delete zero-length streams",
    description: "Drop empty stream objects.",
  },
  {
    id: "strip_extras",
    label: "Strip non-essential extras",
    description: "Remove XMP metadata, thumbnails, JavaScript, and embedded files.",
  },
  {
    id: "recompress_images",
    label: "Downsample images",
    description: "Resize and re-encode images to a target DPI.",
    comingSoon: true,
  },
];

const RUNNABLE_STEPS = STEPS.filter((s) => !s.comingSoon);

interface StepResult {
  step: StepId;
  sizeBefore: number;
  sizeAfter: number;
}

interface SkippedImages {
  reason: string;
  count: number;
}

interface OptimizationReport {
  results: StepResult[];
  skippedImages: SkippedImages[];
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

function percentReduction(before: number, after: number): string {
  if (before <= 0) return "0%";
  return `${(((before - after) / before) * 100).toFixed(1)}%`;
}

const STEP_LABELS: Record<StepId, string> = Object.fromEntries(
  STEPS.map((s) => [s.id, s.label]),
) as Record<StepId, string>;

function suggestName(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  const base = dot > 0 ? fileName.slice(0, dot) : fileName;
  return `${base}-optimized.pdf`;
}

export function OptimizePanel() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));

  // All four safe steps start checked.
  const [checked, setChecked] = useState<Set<StepId>>(
    () => new Set(RUNNABLE_STEPS.map((s) => s.id)),
  );
  const [targetDpi, setTargetDpi] = useState(150);
  const [jpegQuality, setJpegQuality] = useState(80);
  const [running, setRunning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [report, setReport] = useState<OptimizationReport | null>(null);
  const [saved, setSaved] = useState(false);

  // Reset results when the active document changes, so one file's optimization
  // never lingers on another file's panel. The panel stays mounted across tab
  // switches — only the active tab changes — so this can't rely on remounting.
  const activeDocId = activeTab?.docId;
  useEffect(() => {
    setReport(null);
    setSaved(false);
    setRunning(false);
    setSaving(false);
  }, [activeDocId]);

  if (!activeTab) return null;
  const docId = activeTab.docId;

  const toggle = (id: StepId) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
    // Previous results no longer match the selection.
    setReport(null);
    setSaved(false);
  };

  const handleRun = async () => {
    // Preserve the declared step order rather than checkbox-click order.
    const steps = RUNNABLE_STEPS.filter((s) => checked.has(s.id)).map((s) => s.id);
    if (steps.length === 0) return;
    setRunning(true);
    setSaved(false);
    try {
      const result = await invoke<OptimizationReport>("run_optimization_steps", {
        docId,
        steps,
        targetDpi,
        jpegQuality,
      });
      setReport(result);
    } catch (err) {
      await message(String(err), { title: "Optimization Failed", kind: "error" });
    } finally {
      setRunning(false);
    }
  };

  const handleSave = async () => {
    const destPath = await save({
      defaultPath: suggestName(activeTab.fileName),
      filters: [{ name: "PDF Documents", extensions: ["pdf"] }],
    });
    if (!destPath) return;
    setSaving(true);
    try {
      await invoke("save_optimized_copy", { docId, destPath });
      // The staged bytes are consumed by the save, so there's nothing left to
      // save again — hide the Save As button to mark the task complete.
      setSaved(true);
    } catch (err) {
      await message(String(err), { title: "Save Failed", kind: "error" });
    } finally {
      setSaving(false);
    }
  };

  // Discard the optimization result without saving, returning the panel to its
  // pre-run state.
  const handleCancel = () => {
    setReport(null);
    setSaved(false);
  };

  const results = report?.results ?? [];
  const totalBefore = results.length > 0 ? results[0].sizeBefore : 0;
  const totalAfter = results.length > 0 ? results[results.length - 1].sizeAfter : 0;

  return (
    <div className="optimize-panel">
      <div className="optimize-steps">
        {STEPS.map((step) => (
          <label
            key={step.id}
            className={`optimize-step${step.comingSoon ? " disabled" : ""}`}
          >
            <input
              type="checkbox"
              checked={!step.comingSoon && checked.has(step.id)}
              disabled={step.comingSoon || running}
              onChange={() => toggle(step.id)}
            />
            <span className="optimize-step-text">
              <span className="optimize-step-label">
                {step.label}
                {step.comingSoon && <span className="optimize-soon"> (coming soon)</span>}
              </span>
              <span className="optimize-step-desc">{step.description}</span>
            </span>
          </label>
        ))}
      </div>

      {/* Image controls — inert until the image step ships. */}
      <fieldset className="optimize-image-controls" disabled>
        <div className="optimize-slider">
          <label>Target DPI: {targetDpi}</label>
          <input
            type="range"
            min={50}
            max={300}
            value={targetDpi}
            onChange={(e) => setTargetDpi(Number(e.target.value))}
          />
        </div>
        <div className="optimize-slider">
          <label>JPEG quality: {jpegQuality}</label>
          <input
            type="range"
            min={50}
            max={95}
            value={jpegQuality}
            onChange={(e) => setJpegQuality(Number(e.target.value))}
          />
        </div>
      </fieldset>

      <button
        className="optimize-run-button"
        onClick={handleRun}
        disabled={running || checked.size === 0}
      >
        {running ? "Running…" : "Run"}
      </button>

      {report && (
        <div className="optimize-results">
          <table className="optimize-results-table">
            <thead>
              <tr>
                <th>Step</th>
                <th>Before</th>
                <th>After</th>
                <th>Saved</th>
              </tr>
            </thead>
            <tbody>
              {results.map((r) => (
                <tr key={r.step}>
                  <td>{STEP_LABELS[r.step] ?? r.step}</td>
                  <td>{formatBytes(r.sizeBefore)}</td>
                  <td>{formatBytes(r.sizeAfter)}</td>
                  <td>{percentReduction(r.sizeBefore, r.sizeAfter)}</td>
                </tr>
              ))}
            </tbody>
          </table>

          <div className="optimize-total">
            Total: {formatBytes(totalBefore)} → {formatBytes(totalAfter)} (
            {percentReduction(totalBefore, totalAfter)})
          </div>

          {report.skippedImages.length > 0 && (
            <div className="optimize-skipped">
              {report.skippedImages
                .map((s) => `${s.count} image${s.count !== 1 ? "s" : ""} (${s.reason})`)
                .join(", ")}{" "}
              skipped
            </div>
          )}

          {saved ? (
            <div className="optimize-saved">✓ Saved</div>
          ) : (
            <div className="optimize-actions">
              <button
                className="optimize-save-button"
                onClick={handleSave}
                disabled={saving}
              >
                {saving ? "Saving…" : "Save As…"}
              </button>
              <button
                className="optimize-cancel-button"
                onClick={handleCancel}
                disabled={saving}
              >
                Cancel
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

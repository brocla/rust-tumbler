import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save, message } from "@tauri-apps/plugin-dialog";
import { Zap } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import { confirmBreakingEdit } from "../utils/confirmBreakingEdit";
import { isSigned, SIGNATURE_EDIT_WARNING } from "../utils/signature";
import { saveTabAs } from "../utils/saveDocument";

interface ConformanceClaims {
  declared: string[];
}

// Compression strips XMP and re-encodes images, which breaks PDF/A and PDF/X
// conformance (PDF/UA/PDF/E are not guarded here — the structural damage is to
// A/X). Returns the declared A/X claims that an optimization run would void.
function breakingClaims(declared: string[]): string[] {
  return declared.filter((c) => c.startsWith("PDF/A") || c.startsWith("PDF/X"));
}

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
}

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
    description: "Resize and re-encode oversized images to the target DPI (lossy).",
  },
];

// The image step is lossy, so it starts unchecked; the four lopdf-only steps
// are safe and start checked.
const IMAGE_STEP: StepId = "recompress_images";
const DEFAULT_CHECKED: StepId[] = STEPS.filter((s) => s.id !== IMAGE_STEP).map((s) => s.id);

// Backend skip reasons → human-readable labels for the skipped-images notice.
const REASON_LABELS: Record<string, string> = {
  bilevel: "black & white",
  indexed: "indexed color",
  colorspace: "unsupported color",
  predictor: "predictor",
  decode_array: "custom decode array",
  ccitt: "CCITT/fax",
  jpx: "JPEG2000",
  jbig2: "JBIG2",
  crypt: "encrypted",
  unsupported_filter: "unsupported filter",
  decode: "unreadable",
  unreferenced: "not displayed",
};

function reasonLabel(reason: string): string {
  return REASON_LABELS[reason] ?? reason;
}

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
  // Images left alone because they're already at or below the target DPI.
  // Distinct from skippedImages (images we couldn't handle) — this is the
  // step working as intended, and without it a document whose images are
  // already sensibly sized reports 0.00% with no explanation.
  imagesAtTarget: number;
  cancelled: boolean;
}

// One image as the backend's read-only inspector sees it.
interface ImageInfo {
  pages: number[];
  width: number;
  height: number;
  storedBytes: number;
  filter: string;
  colorSpace: string;
  // Null when the image is never drawn on a page — with no draw site there's
  // no displayed size, so no resolution to report.
  dpi: number | null;
  // Null for anything that isn't a plain JPEG. Always an estimate, never a
  // stored fact, so it renders with a leading "~".
  jpegQuality: number | null;
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

// PDF filter names are opaque to anyone who hasn't read the spec.
const FILTER_LABELS: Record<string, string> = {
  DCTDecode: "JPEG",
  JPXDecode: "JPEG 2000",
  FlateDecode: "Flate",
  LZWDecode: "LZW",
  CCITTFaxDecode: "CCITT fax",
  JBIG2Decode: "JBIG2",
  RunLengthDecode: "run-length",
};

function shortFilter(filter: string): string {
  if (!filter) return "uncompressed";
  return filter
    .split("+")
    .map((f) => FILTER_LABELS[f] ?? f)
    .join(" + ");
}

/** "p1", "p2–4", "p1, p3" — compact enough for a narrow sidebar column. */
function formatPages(pages: number[]): string {
  if (pages.length === 0) return "—";
  const runs: string[] = [];
  let start = pages[0];
  let prev = pages[0];
  for (const page of pages.slice(1)) {
    if (page === prev + 1) {
      prev = page;
      continue;
    }
    runs.push(start === prev ? `${start}` : `${start}–${prev}`);
    start = prev = page;
  }
  runs.push(start === prev ? `${start}` : `${start}–${prev}`);
  return `p${runs.join(", ")}`;
}

function suggestName(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  const base = dot > 0 ? fileName.slice(0, dot) : fileName;
  return `${base}-compressed.pdf`;
}

function suggestLinearizedName(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  const base = dot > 0 ? fileName.slice(0, dot) : fileName;
  return `${base}-linearized.pdf`;
}

export function OptimizePanel() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));

  const [checked, setChecked] = useState<Set<StepId>>(() => new Set(DEFAULT_CHECKED));
  const [targetDpi, setTargetDpi] = useState(150);
  const [jpegQuality, setJpegQuality] = useState(80);
  const [running, setRunning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [report, setReport] = useState<OptimizationReport | null>(null);
  // The DPI the displayed report was produced at. The slider stays live after a
  // run, so quoting `targetDpi` in the results would misreport what happened.
  const [reportDpi, setReportDpi] = useState(150);
  const [saved, setSaved] = useState(false);
  const [images, setImages] = useState<ImageInfo[] | null>(null);
  const linearizeProgress = usePdfStore((s) => s.linearizeProgress);
  const setLinearizeProgress = usePdfStore((s) => s.setLinearizeProgress);

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

  // Inspect only while the image step is checked — that's when these numbers
  // are being acted on. Re-inspect after a run, because compression rewrites
  // the buffer the inspector reads from.
  const wantImages = checked.has(IMAGE_STEP);
  useEffect(() => {
    if (!activeDocId || !wantImages) {
      setImages(null);
      return;
    }
    let stale = false;
    invoke<ImageInfo[]>("inspect_images", { docId: activeDocId })
      // Inspection is advisory: it must never take the panel down with it, so
      // anything other than a list is treated as "nothing to report".
      .then((list) => !stale && setImages(Array.isArray(list) ? list : []))
      .catch(() => !stale && setImages([]));
    return () => {
      stale = true;
    };
  }, [activeDocId, wantImages, report]);

  if (!activeTab) return null;
  const docId = activeTab.docId;
  const imageChecked = checked.has(IMAGE_STEP);

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
    const steps = STEPS.filter((s) => checked.has(s.id)).map((s) => s.id);
    if (steps.length === 0) return;

    // Guard: a signed document's optimized output won't carry a valid
    // signature. Warn first (overridable).
    if (isSigned(activeTab.signatureStatus)) {
      const proceed = await confirmBreakingEdit(SIGNATURE_EDIT_WARNING);
      if (!proceed) return;
    }

    // Guard: compressing a file that declares PDF/A or PDF/X will void that
    // conformance (XMP removal + lossy image re-encode). Warn before running;
    // the warning is overridable. The result is applied to the in-memory
    // buffer, not the file, so this is about informed consent rather than
    // preventing the run.
    try {
      const { declared } = await invoke<ConformanceClaims>("get_conformance", { docId });
      const breaking = breakingClaims(declared);
      if (breaking.length > 0) {
        const proceed = await confirmBreakingEdit(
          `This PDF declares conformance with ${breaking.join(", ")}. ` +
            "Optimizing it removes metadata and re-encodes images, so the saved " +
            `copy will no longer be a valid ${breaking.join("/")} file.`,
        );
        if (!proceed) return;
      }
    } catch {
      // If conformance can't be read, don't block compression — proceed.
    }

    setRunning(true);
    setSaved(false);
    try {
      const result = await invoke<OptimizationReport>("run_optimization_steps", {
        docId,
        steps,
        targetDpi,
        jpegQuality,
      });
      // A cancelled run kept no output, so leave the panel in its pre-run state.
      setReport(result.cancelled ? null : result);
      setReportDpi(targetDpi);
    } catch (err) {
      await message(String(err), { title: "Compression Failed", kind: "error" });
    } finally {
      setRunning(false);
      usePdfStore.getState().setCompressProgress(null);
    }
  };

  // The optimized bytes are already applied to the document's buffer (the run
  // marked it dirty), so saving is the ordinary Save As flow — just with a
  // "-compressed" name suggested.
  const handleSave = async () => {
    setSaving(true);
    try {
      if (await saveTabAs(activeTab, suggestName(activeTab.fileName))) {
        setSaved(true);
      }
    } finally {
      setSaving(false);
    }
  };

  // "Save Linearized Copy" (issue #3): export-only — writes a linearized
  // ("Fast Web View") copy via qpdf of the buffer as it currently stands, and
  // never touches the buffer or the original file. Grouped here with Compress
  // because together they're "web-optimization" (see the explainer above),
  // but this is an independent action — it doesn't require a Compress run
  // first, though the explainer's ordering (compress, then linearize) still
  // applies if you do want both: run this last, since linearizing after any
  // further edit (including a later Compress run) would undo it.
  const handleSaveLinearized = async () => {
    const dir = activeTab.filePath.replace(/[\\/][^\\/]*$/, "");
    const destPath = await save({
      filters: [{ name: "PDF", extensions: ["pdf"] }],
      defaultPath: `${dir}/${suggestLinearizedName(activeTab.fileName)}`,
    });
    if (!destPath) return;

    setLinearizeProgress(true);
    try {
      await invoke("export_linearized_copy", { docId: activeTab.docId, destPath });
      // No size comparison — unlike Compress, linearizing isn't about file
      // size (it reorders structure and adds a hint stream).
      const note = activeTab.encrypted
        ? " The copy is unencrypted (linearized copies are written without password protection)."
        : "";
      await message(`Saved linearized copy.${note}`, {
        title: "Save Linearized Copy",
        kind: "info",
      });
    } catch (err) {
      await message(String(err), { title: "Save Linearized Copy", kind: "error" });
    } finally {
      setLinearizeProgress(false);
    }
  };

  const results = report?.results ?? [];
  const totalBefore = results.length > 0 ? results[0].sizeBefore : 0;
  const totalAfter = results.length > 0 ? results[results.length - 1].sizeAfter : 0;

  return (
    <div className="optimize-panel">
      <div className="optimize-explainer">
        Web-optimization prepares a PDF to be accessed efficiently over a network,
        as two separate steps. <strong>Compress</strong> shrinks the file by
        removing redundant data. <strong>Save Linearized Copy</strong>, below,
        reorders the file so a viewer streaming it over the web can render page 1
        before the rest has downloaded. Linearizing must be the last step — run
        it after Compress, since any edit afterward (including a later Compress
        run) undoes it.
      </div>

      <div className="optimize-steps">
        {STEPS.map((step) => (
          <label key={step.id} className="optimize-step">
            <input
              type="checkbox"
              checked={checked.has(step.id)}
              disabled={running}
              onChange={() => toggle(step.id)}
            />
            <span className="optimize-step-text">
              <span className="optimize-step-label">{step.label}</span>
              <span className="optimize-step-desc">{step.description}</span>
            </span>
          </label>
        ))}
      </div>

      {/* DPI/quality apply only to the image step — disabled until it's checked. */}
      <fieldset className="optimize-image-controls" disabled={!imageChecked || running}>
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

      {imageChecked && images !== null && <ImageInspector images={images} targetDpi={targetDpi} />}

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

          {report.imagesAtTarget > 0 && (
            <div className="optimize-skipped">
              {report.imagesAtTarget} image{report.imagesAtTarget !== 1 ? "s" : ""} already at
              or below {reportDpi} DPI — nothing to downsample. Lower the target DPI to
              re-encode {report.imagesAtTarget !== 1 ? "them" : "it"} anyway.
            </div>
          )}

          {report.skippedImages.length > 0 && (
            <div className="optimize-skipped">
              Skipped{" "}
              {report.skippedImages
                .map(
                  (s) =>
                    `${s.count} image${s.count !== 1 ? "s" : ""} (${reasonLabel(s.reason)})`,
                )
                .join(", ")}
            </div>
          )}

          {saved ? (
            <div className="optimize-saved">✓ Saved</div>
          ) : (
            <div className="optimize-actions">
              {/* The result is applied to the document (unsaved); Ctrl+S /
                  toolbar Save also work. This button is the Save As shortcut
                  with a "-compressed" name suggested. */}
              <button
                className="optimize-save-button"
                onClick={handleSave}
                disabled={saving}
              >
                {saving ? "Saving…" : "Save As…"}
              </button>
            </div>
          )}
        </div>
      )}

      <div className="optimize-linearize-section">
        <div className="optimize-linearize-note">
          Writes a new, separate file — the original and this document's buffer
          are untouched. Run this last.
        </div>
        <button
          className="optimize-linearize-button"
          onClick={() => void handleSaveLinearized()}
          disabled={linearizeProgress}
        >
          <Zap size={16} />
          {linearizeProgress ? "Saving…" : "Save Linearized Copy…"}
        </button>
      </div>
    </div>
  );
}

/**
 * Read-only listing of the document's images.
 *
 * The compression panel used to give no way to tell whether a run would do
 * anything, so a file whose images were already at a sensible resolution
 * reported 0.00% and looked broken. This shows the two numbers the decision
 * actually turns on — resolution and encoder quality — before anything runs.
 */
function ImageInspector({ images, targetDpi }: { images: ImageInfo[]; targetDpi: number }) {
  if (images.length === 0) {
    return (
      <div className="optimize-skipped">
        No images in this document — the downsample step has nothing to work on.
      </div>
    );
  }

  const measured = images.filter((img) => img.dpi !== null);
  const above = measured.filter((img) => img.dpi! > targetDpi).length;
  const at = measured.length - above;

  return (
    <div className="optimize-images">
      <div className="optimize-images-header">
        {images.length} image{images.length !== 1 ? "s" : ""} in this document
      </div>
      <ul className="optimize-image-list">
        {images.map((img, i) => (
          <li key={i} className={img.dpi !== null && img.dpi <= targetDpi ? "at-target" : ""}>
            <div className="optimize-image-row">
              <span className="optimize-image-page">{formatPages(img.pages)}</span>
              <span className="optimize-image-bytes">{formatBytes(img.storedBytes)}</span>
            </div>
            <div className="optimize-image-detail">
              {img.width}×{img.height}
              {img.dpi !== null && ` · ${Math.round(img.dpi)} DPI`}
              {/* Recovered from the file's quantization table, so "~". */}
              {img.jpegQuality !== null && ` · ~q${Math.round(img.jpegQuality)}`}
            </div>
            <div className="optimize-image-detail">
              {shortFilter(img.filter)} · {img.colorSpace}
              {img.dpi === null && " · never drawn on a page"}
            </div>
          </li>
        ))}
      </ul>
      <div className="optimize-images-summary">
        At {targetDpi} DPI: {above} to downsample, {at} already small enough
        {measured.length < images.length &&
          `, ${images.length - measured.length} unmeasurable`}
        .
      </div>
    </div>
  );
}

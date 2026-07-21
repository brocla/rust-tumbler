import { useEffect } from "react";
import { Bold, Italic } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import type { TypewriterAnnot, TypewriterStyle } from "../store/usePdfStore";
import { commitTypewriter, hexToRgb, rgbToHex } from "../utils/typewriter";

const FONT_FAMILIES: TypewriterAnnot["fontFamily"][] = ["Helvetica", "Times", "Courier"];

/**
 * Typewriter panel (issue #99): arms the tool and edits the note style. The
 * font controls target the selected note when one is being edited, otherwise
 * they set the style for the next new note. Notes are committed to the buffer
 * as FreeText annotations; the user saves with the ordinary Save / Save As.
 */
export function TypewriterPanel() {
  const activeTab = usePdfStore((s) => s.tabs.find((t) => t.id === s.activeTabId));
  const armed = usePdfStore((s) => s.typewriterMode);
  const style = usePdfStore((s) => s.typewriterStyle);
  const activeId = usePdfStore((s) => s.activeTypewriterId);
  const setTypewriterMode = usePdfStore((s) => s.setTypewriterMode);
  const setTypewriterStyle = usePdfStore((s) => s.setTypewriterStyle);
  const setActiveTypewriter = usePdfStore((s) => s.setActiveTypewriter);
  const updateTypewriterAnnot = usePdfStore((s) => s.updateTypewriterAnnot);
  const removeTypewriterAnnot = usePdfStore((s) => s.removeTypewriterAnnot);
  const setTypewriterAnnots = usePdfStore((s) => s.setTypewriterAnnots);

  const docId = activeTab?.docId;

  // Reset per-document state when the active document changes (the panel stays
  // mounted across tab switches — mirror RedactPanel).
  useEffect(() => {
    setTypewriterMode(false);
    setActiveTypewriter(null);
  }, [docId, setTypewriterMode, setActiveTypewriter]);

  // Disarm when the panel unmounts (tool switched away) so the viewer isn't
  // left placing notes.
  useEffect(
    () => () => {
      usePdfStore.getState().setTypewriterMode(false);
      usePdfStore.getState().setActiveTypewriter(null);
    },
    [],
  );

  if (!activeTab || !docId) return null;

  const annots = activeTab.typewriterAnnots ?? [];
  const activeAnnot = annots.find((a) => a.id === activeId);
  // The controls reflect the selected note, or the default style for new notes.
  const current: TypewriterStyle = activeAnnot
    ? {
        fontFamily: activeAnnot.fontFamily,
        bold: activeAnnot.bold,
        italic: activeAnnot.italic,
        fontSize: activeAnnot.fontSize,
        color: activeAnnot.color,
      }
    : style;

  // Apply a style change to the default (so the next note matches) and, when a
  // note is being edited, to that note too.
  const applyStyle = (patch: Partial<TypewriterStyle>) => {
    setTypewriterStyle(patch);
    if (activeAnnot) {
      updateTypewriterAnnot(docId, activeAnnot.id, patch);
      void commitTypewriter(docId);
    }
  };

  const handleRemove = (id: string) => {
    removeTypewriterAnnot(docId, id);
    if (activeId === id) setActiveTypewriter(null);
    void commitTypewriter(docId);
  };

  const handleClearAll = () => {
    setTypewriterAnnots(docId, []);
    setActiveTypewriter(null);
    void commitTypewriter(docId);
  };

  return (
    <div className="typewriter-panel">
      <div className="typewriter-explainer">
        Type text anywhere on the page — handy for filling forms that use plain
        underline blanks. Click <strong>Add text</strong>, then click where you
        want to type. Double-click a note to edit it again. Notes are saved with
        the document when you Save.
      </div>

      <button
        className={`typewriter-arm-button${armed ? " active" : ""}`}
        onClick={() => setTypewriterMode(!armed)}
      >
        {armed ? "Placing — click to stop" : "Add text"}
      </button>

      <div className="typewriter-style">
        <label className="typewriter-field">
          <span>Font</span>
          <select
            value={current.fontFamily}
            onChange={(e) =>
              applyStyle({ fontFamily: e.target.value as TypewriterAnnot["fontFamily"] })
            }
          >
            {FONT_FAMILIES.map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </select>
        </label>

        <label className="typewriter-field">
          <span>Size</span>
          <input
            type="number"
            min={6}
            max={96}
            value={current.fontSize}
            onChange={(e) => {
              const size = Number(e.target.value);
              if (Number.isFinite(size) && size > 0) applyStyle({ fontSize: size });
            }}
          />
        </label>

        <label className="typewriter-field">
          <span>Color</span>
          <input
            type="color"
            value={rgbToHex(current.color)}
            onChange={(e) => applyStyle({ color: hexToRgb(e.target.value) })}
          />
        </label>

        <div className="typewriter-style-toggles">
          <button
            className={`toolbar-button${current.bold ? " active" : ""}`}
            title="Bold"
            aria-pressed={current.bold}
            onClick={() => applyStyle({ bold: !current.bold })}
          >
            <Bold size={16} />
          </button>
          <button
            className={`toolbar-button${current.italic ? " active" : ""}`}
            title="Italic"
            aria-pressed={current.italic}
            onClick={() => applyStyle({ italic: !current.italic })}
          >
            <Italic size={16} />
          </button>
        </div>
      </div>

      <div className="typewriter-note-list">
        <div className="typewriter-note-header">
          <span>
            {annots.length} note{annots.length === 1 ? "" : "s"}
          </span>
          {annots.length > 0 && (
            <button className="typewriter-clear-button" onClick={handleClearAll}>
              Clear all
            </button>
          )}
        </div>
        {annots.map((annot) => (
          <div
            key={annot.id}
            className={`typewriter-note-row${annot.id === activeId ? " active" : ""}`}
            onClick={() => setActiveTypewriter(annot.id)}
          >
            <span className="typewriter-note-snippet">
              Page {annot.page} — {annot.text.trim() || "(empty)"}
            </span>
            <button
              title="Remove this note"
              onClick={(e) => {
                e.stopPropagation();
                handleRemove(annot.id);
              }}
            >
              ✕
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

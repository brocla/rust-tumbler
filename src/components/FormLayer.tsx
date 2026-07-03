import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { usePdfStore } from "../store/usePdfStore";
import { evictPageCache } from "../utils/renderCache";

type FieldType =
  | "text"
  | "multiline_text"
  | "checkbox"
  | "radio"
  | "dropdown"
  | "button"
  | "unknown";

type ButtonAction = "none" | "reset_form" | "unsupported";

interface FormField {
  id: string;
  name: string;
  fieldType: FieldType;
  value: string;
  exportValue: string;
  rect: { x: number; y: number; width: number; height: number };
  page: number;
  options: string[];
  readOnly: boolean;
  maxLen: number | null;
  comb: boolean;
  label: string;
  buttonAction: ButtonAction;
}

interface FormLayerProps {
  docId: string;
  pageNumber: number;
  zoom: number;
}

/**
 * Overlays interactive AcroForm controls on a rendered page, mirroring
 * `TextLayer`'s absolutely-positioned layout. Fields are fetched per page; each
 * edit is committed to the document buffer (issue #31) via
 * `set_form_field_value` on blur (text/dropdown) or change (checkbox/radio),
 * which marks the tab dirty. Nothing is written to disk until the user saves.
 */
export function FormLayer({ docId, pageNumber, zoom }: FormLayerProps) {
  const [fields, setFields] = useState<FormField[]>([]);
  // Local edits keyed by field id (radio buttons in a group share one id, so
  // selecting one deselects the others).
  const [edits, setEdits] = useState<Record<string, string>>({});
  // The comb field currently being edited, if any. A comb field is a
  // transparent "ghost" at rest (pdfium draws its combed value on the canvas)
  // and flips to an opaque HTML editor only while focused.
  const [focusedComb, setFocusedComb] = useState<string | null>(null);
  const updateTab = usePdfStore((s) => s.updateTab);
  // Re-fetch after a buffer edit (e.g. a page op) rebuilds the document, or
  // after a form Clear/Reset (formEpoch).
  const pagesVersion = usePdfStore(
    (s) => s.tabs.find((t) => t.docId === docId)?.pagesVersion ?? 0,
  );
  const formEpoch = usePdfStore(
    (s) => s.tabs.find((t) => t.docId === docId)?.formEpoch ?? 0,
  );
  const showNotice = usePdfStore((s) => s.showNotice);
  const bumpFormEpoch = usePdfStore((s) => s.bumpFormEpoch);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const result = await invoke<FormField[]>("get_form_fields", {
          docId,
          page: pageNumber,
        });
        if (!cancelled) {
          setFields(result);
          setEdits({});
        }
      } catch (err) {
        // A document with no form, or an XFA-only form, yields an error/empty —
        // either way there's nothing to overlay.
        if (!cancelled) setFields([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [docId, pageNumber, pagesVersion, formEpoch]);

  const scale = zoom / 100;

  const commit = async (id: string, value: string) => {
    setEdits((e) => ({ ...e, [id]: value }));
    try {
      await invoke("set_form_field_value", { docId, fieldId: id, value });
    } catch (err) {
      console.error(`Failed to set form field ${id}:`, err);
    }
  };

  const current = (f: FormField) => edits[f.id] ?? f.value;

  // After committing a comb field, pdfium's canvas render of it is stale — evict
  // the page's cached bitmap and bump contentEpoch so PageSlot repaints with the
  // freshly combed value.
  const repaintPage = () => {
    evictPageCache(docId, pageNumber);
    const tab = usePdfStore.getState().tabs.find((t) => t.docId === docId);
    if (tab) updateTab(tab.id, { contentEpoch: tab.contentEpoch + 1 });
  };

  const clickButton = async (field: FormField) => {
    if (field.buttonAction === "reset_form") {
      try {
        await invoke("reset_form_via_button", { docId, fieldId: field.id });
        bumpFormEpoch(docId);
      } catch (err) {
        console.error(`Failed to reset form via ${field.id}:`, err);
      }
    } else {
      showNotice("This button's action is not supported");
    }
  };

  if (fields.length === 0) return null;

  return (
    <div className="form-layer">
      {fields.map((field, i) => {
        const style = {
          position: "absolute" as const,
          left: field.rect.x * scale,
          top: field.rect.y * scale,
          width: field.rect.width * scale,
          height: field.rect.height * scale,
        };

        if (field.fieldType === "text" || field.fieldType === "multiline_text") {
          // A comb field is a transparent ghost at rest (pdfium shows the combed
          // value) and only opaque while focused.
          const ghost = field.comb && focusedComb !== field.id;
          // Controlled so a Clear/Reset (which re-fetches cleared values) always
          // updates the DOM. Uncontrolled `defaultValue` only applies on mount,
          // so React would keep showing the pre-reset text on the reused node.
          const common = {
            className: `form-field${ghost ? " form-ghost" : ""}`,
            style,
            value: current(field),
            disabled: field.readOnly,
            maxLength: field.maxLen ?? undefined,
            onFocus: () => {
              if (field.comb) setFocusedComb(field.id);
            },
            onChange: (
              e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>,
            ) => setEdits((prev) => ({ ...prev, [field.id]: e.target.value })),
            onKeyDown: (
              e: React.KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>,
            ) => {
              // Enter commits a single-line field by blurring it (which runs the
              // commit and, for a comb field, the ghost repaint). In a textarea
              // Enter must stay a newline.
              if (e.key === "Enter" && field.fieldType !== "multiline_text") {
                e.preventDefault();
                e.currentTarget.blur();
              }
            },
            onBlur: async (
              e: React.FocusEvent<HTMLInputElement | HTMLTextAreaElement>,
            ) => {
              const changed = e.target.value !== field.value;
              if (changed) await commit(field.id, e.target.value);
              if (field.comb) {
                setFocusedComb((cur) => (cur === field.id ? null : cur));
                // Repaint so pdfium's combed render replaces the ghost.
                if (changed) repaintPage();
              }
            },
          };
          return field.fieldType === "multiline_text" ? (
            <textarea key={i} {...common} />
          ) : (
            <input key={i} type="text" {...common} />
          );
        }

        if (field.fieldType === "checkbox") {
          return (
            <input
              key={i}
              type="checkbox"
              className="form-field form-check"
              style={style}
              disabled={field.readOnly}
              checked={current(field) === field.exportValue}
              onChange={(e) =>
                commit(field.id, e.target.checked ? field.exportValue : "Off")
              }
            />
          );
        }

        if (field.fieldType === "radio") {
          return (
            <input
              key={i}
              type="radio"
              className="form-field form-check"
              style={style}
              name={`${docId}-${field.id}`}
              disabled={field.readOnly}
              checked={current(field) === field.exportValue}
              onChange={() => commit(field.id, field.exportValue)}
            />
          );
        }

        if (field.fieldType === "dropdown") {
          return (
            <select
              key={i}
              className="form-field"
              style={style}
              disabled={field.readOnly}
              value={current(field)}
              onChange={(e) => commit(field.id, e.target.value)}
            >
              {/* The current value may not be among /Opt; keep it selectable. */}
              {!field.options.includes(current(field)) && (
                <option value={current(field)}>{current(field)}</option>
              )}
              {field.options.map((opt) => (
                <option key={opt} value={opt}>
                  {opt}
                </option>
              ))}
            </select>
          );
        }

        if (field.fieldType === "button") {
          return (
            <button
              key={i}
              type="button"
              className="form-field form-button"
              style={style}
              disabled={field.readOnly}
              title={
                field.buttonAction === "reset_form"
                  ? "Reset form fields"
                  : "This button's action is not supported"
              }
              onClick={() => clickButton(field)}
            >
              {field.label}
            </button>
          );
        }

        return null;
      })}
    </div>
  );
}

// Display-only formatting for PDF date strings (issue #74). PDFs store dates as
// `D:YYYYMMDDHHmmSSOHH'mm'` (PDF 32000-1 §7.9.4), e.g. `D:20260710143005-04'00'`.
// This turns that into something readable for the Metadata panel; it never
// changes what's stored in the document.

const MONTHS = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];

// Anchored at the start; every field past the year is optional, matching the
// spec (missing month/day default to 01, missing time fields to 00). The offset
// apostrophes are optional so both `-04'00'` and `-0400` parse.
const PDF_DATE_RE =
  /^(\d{4})(\d{2})?(\d{2})?(\d{2})?(\d{2})?(\d{2})?(?:([Z+-])(\d{2})?'?(\d{2})?'?)?/;

/**
 * Formats a raw PDF date string for display, e.g.
 * `D:20260710143005-04'00'` → `July 10, 2026 at 14:30:05 UTC-04:00`.
 *
 * Preserves the timestamp exactly as written (no timezone conversion), so the
 * value shown matches the document rather than the viewer's clock. Returns the
 * input unchanged if it isn't a recognizable PDF date, and "" for empty input.
 */
export function formatPdfDate(raw: string): string {
  if (!raw) return "";
  const s = raw.startsWith("D:") ? raw.slice(2) : raw;
  const m = s.match(PDF_DATE_RE);
  if (!m) return raw;

  const [, year, month = "01", day = "01", hh, mm, ss, tzSign, tzH, tzM] = m;
  const monthIdx = parseInt(month, 10) - 1;
  if (monthIdx < 0 || monthIdx > 11) return raw;

  let out = `${MONTHS[monthIdx]} ${parseInt(day, 10)}, ${year}`;

  if (hh !== undefined) {
    out += ` at ${hh}:${mm ?? "00"}:${ss ?? "00"}`;
    if (tzSign === "Z") {
      out += " UTC";
    } else if (tzSign === "+" || tzSign === "-") {
      out += ` UTC${tzSign}${tzH ?? "00"}:${tzM ?? "00"}`;
    }
  }

  return out;
}

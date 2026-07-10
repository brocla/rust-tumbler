import { describe, it, expect } from "vitest";
import { formatPdfDate } from "./pdfDate";

describe("formatPdfDate", () => {
  it("returns empty string for empty input", () => {
    expect(formatPdfDate("")).toBe("");
  });

  it("formats a full date with a negative UTC offset", () => {
    expect(formatPdfDate("D:20260710143005-04'00'")).toBe(
      "July 10, 2026 at 14:30:05 UTC-04:00",
    );
  });

  it("formats a positive UTC offset", () => {
    expect(formatPdfDate("D:20240101090000+05'30'")).toBe(
      "January 1, 2024 at 09:00:00 UTC+05:30",
    );
  });

  it("renders a Z offset as UTC", () => {
    expect(formatPdfDate("D:20200101000000Z")).toBe(
      "January 1, 2020 at 00:00:00 UTC",
    );
  });

  it("omits timezone when the date carries none", () => {
    expect(formatPdfDate("D:20231225120000")).toBe(
      "December 25, 2023 at 12:00:00",
    );
  });

  it("tolerates a missing D: prefix", () => {
    expect(formatPdfDate("20260710143005-04'00'")).toBe(
      "July 10, 2026 at 14:30:05 UTC-04:00",
    );
  });

  it("shows just the date when no time is present", () => {
    expect(formatPdfDate("D:20260710")).toBe("July 10, 2026");
  });

  it("parses an offset written without apostrophes", () => {
    expect(formatPdfDate("D:20260710143005-0400")).toBe(
      "July 10, 2026 at 14:30:05 UTC-04:00",
    );
  });

  it("returns the raw value when it isn't a PDF date", () => {
    expect(formatPdfDate("not a date")).toBe("not a date");
  });

  it("returns the raw value for an out-of-range month", () => {
    expect(formatPdfDate("D:20261510143005Z")).toBe("D:20261510143005Z");
  });
});

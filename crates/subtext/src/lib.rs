//! Subtext — A Redaction Checker.
//!
//! A **read-only**, tool-agnostic tool that answers one question about a PDF:
//! *"Does the word (or words, or pattern) I redacted still appear anywhere in
//! this file?"* — and, just as importantly, *"which of the many places a PDF
//! can hide text did you actually check?"*
//!
//! Design contract: `doc/redaction-checker-design.md` (status: Ratified).
//! Core principle: **completeness is the product** — the tool never certifies
//! "clean"; it reports "no matches found in the N vectors listed below" and
//! lists them, and every vector it could not inspect is reported as Skipped
//! with a reason, never silently dropped.

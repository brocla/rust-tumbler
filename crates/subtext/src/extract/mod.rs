//! The `VectorCheck` trait and the registry.
//!
//! Every extractor self-describes (`id`, `label`, `vector`, `method`) and is
//! registered in [`REGISTRY`], so the report's `checks` list is *generated from
//! the registry*, never hand-maintained (spec §9). Adding a vector = one file +
//! one `Vector` variant + one registry line; the checks list can never silently
//! drift from the set of implemented extractors.

use crate::query::Query;
use crate::report::{Finding, Vector};
use pdfium_render::prelude::PdfDocument;

pub mod page_text;

/// What one check saw. `Ran` means the check executed (empty ⇒ clean);
/// `Skipped` means it could not run and says why — never silently dropped
/// (spec §1 honesty rule 2).
pub enum CheckOutcome {
    Ran(Vec<Finding>),
    Skipped(String),
}

/// Everything a check needs to inspect one document. The two parser views are
/// each `Option`: a file may parse under pdfium but not lopdf (a recovered
/// corrupt xref) or vice versa, so a check that needs a view it doesn't have
/// returns `Skipped` rather than failing the whole run.
pub struct DocContext<'a, 'p> {
    /// The raw file bytes (for the raw/orphan/revision passes, Phase 3).
    pub bytes: &'a [u8],
    /// lopdf's strict object-graph view (structural vectors).
    pub lopdf: Option<&'a lopdf::Document>,
    /// pdfium's render view (page text, OCR). The document's own borrow of the
    /// process-wide `Pdfium` binding (`'p`) is kept distinct from the borrow of
    /// the document itself (`'a`), so callers can hold the context for a shorter
    /// scope than the binding lives — `PdfDocument<'p>` is invariant in `'p`, and
    /// collapsing the two lifetimes would pin `'a` to the whole process.
    pub pdfium: Option<&'a PdfDocument<'p>>,
}

/// One registered extractor. Object-safe so the registry is a slice of
/// `&dyn VectorCheck`.
pub trait VectorCheck: Sync {
    /// Stable slug, e.g. `"page_text"`.
    fn id(&self) -> &'static str;
    /// Human label, e.g. `"Page text"`.
    fn label(&self) -> &'static str;
    /// The `Vector` this check reports under (one variant per check).
    fn vector(&self) -> Vector;
    /// How it looked, e.g. `"pdfium text extraction"`.
    fn method(&self) -> &'static str;
    /// Run against the document; return findings or a skip reason.
    fn run(&self, ctx: &DocContext, query: &Query) -> CheckOutcome;
}

/// A placeholder for a vector whose extractor has not landed yet. It always
/// `Skipped`s with an honest reason, so the checks list stays complete (all 21
/// vectors present) while later phases fill them in. Keeps the report honest:
/// an un-implemented vector reads as "not inspected", never as clean.
pub struct Pending {
    pub id: &'static str,
    pub label: &'static str,
    pub vector: Vector,
    pub method: &'static str,
    /// Which phase lands this extractor (for the skip message).
    pub phase: &'static str,
}

impl VectorCheck for Pending {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.label
    }
    fn vector(&self) -> Vector {
        self.vector
    }
    fn method(&self) -> &'static str {
        self.method
    }
    fn run(&self, _ctx: &DocContext, _query: &Query) -> CheckOutcome {
        CheckOutcome::Skipped(format!("extractor not yet implemented ({})", self.phase))
    }
}

/// The frozen registry (spec §4.1) — one entry per `Vector` variant, in report
/// order. Phase 1 implements `PageText`; the rest are `Pending` until their
/// phase. The report's checks list is built by iterating this slice.
pub static REGISTRY: &[&dyn VectorCheck] = &[
    &page_text::PageText,
    &Pending {
        id: "rendered_ocr",
        label: "Rendered-image OCR",
        vector: Vector::RenderedOcr,
        method: "OCR engine (feature \"ocr\")",
        phase: "Phase 3, opt-in --ocr",
    },
    &Pending {
        id: "metadata",
        label: "Document metadata",
        vector: Vector::Metadata,
        method: "Info + all /Metadata XMP",
        phase: "Phase 2",
    },
    &Pending {
        id: "structure_tree",
        label: "Structure tree",
        vector: Vector::StructureTree,
        method: "/StructTreeRoot walk",
        phase: "Phase 2",
    },
    &Pending {
        id: "marked_content",
        label: "Marked content",
        vector: Vector::MarkedContent,
        method: "content-stream /ActualText",
        phase: "Phase 2",
    },
    &Pending {
        id: "outlines",
        label: "Bookmarks",
        vector: Vector::Outlines,
        method: "/Outlines walk",
        phase: "Phase 2",
    },
    &Pending {
        id: "page_labels",
        label: "Page labels",
        vector: Vector::PageLabels,
        method: "/PageLabels number tree",
        phase: "Phase 2",
    },
    &Pending {
        id: "destinations",
        label: "Named destinations",
        vector: Vector::Destinations,
        method: "/Names/Dests name tree",
        phase: "Phase 2",
    },
    &Pending {
        id: "article_threads",
        label: "Article threads",
        vector: Vector::ArticleThreads,
        method: "/Threads bead /I",
        phase: "Phase 2",
    },
    &Pending {
        id: "annotations",
        label: "Annotations",
        vector: Vector::Annotations,
        method: "/Annots + /AP appearance streams",
        phase: "Phase 2",
    },
    &Pending {
        id: "redaction_annotations",
        label: "Redaction annotations",
        vector: Vector::RedactionAnnotations,
        method: "/Redact annotation scan",
        phase: "Phase 2",
    },
    &Pending {
        id: "forms",
        label: "Form fields",
        vector: Vector::Forms,
        method: "AcroForm /Fields walk",
        phase: "Phase 2",
    },
    &Pending {
        id: "xfa",
        label: "XFA forms",
        vector: Vector::Xfa,
        method: "/XFA datasets + template",
        phase: "Phase 2",
    },
    &Pending {
        id: "attachments",
        label: "Attachments",
        vector: Vector::Attachments,
        method: "/EmbeddedFiles + /AF",
        phase: "Phase 2",
    },
    &Pending {
        id: "scripts",
        label: "Scripts & actions",
        vector: Vector::Scripts,
        method: "/JavaScript, /OpenAction, /AA",
        phase: "Phase 2",
    },
    &Pending {
        id: "uris",
        label: "URIs & web capture",
        vector: Vector::Uris,
        method: "URI actions + /SpiderInfo",
        phase: "Phase 2",
    },
    &Pending {
        id: "optional_content",
        label: "Optional content",
        vector: Vector::OptionalContent,
        method: "OCG /Name labels",
        phase: "Phase 2",
    },
    &Pending {
        id: "signatures",
        label: "Signatures",
        vector: Vector::Signatures,
        method: "/Contents hex → DER scan",
        phase: "Phase 2",
    },
    &Pending {
        id: "revisions",
        label: "Superseded revisions",
        vector: Vector::Revisions,
        method: "per-revision reparse",
        phase: "Phase 3",
    },
    &Pending {
        id: "orphan_objects",
        label: "Orphaned objects",
        vector: Vector::OrphanObjects,
        method: "N N obj + ObjStm brute-scan",
        phase: "Phase 3",
    },
    &Pending {
        id: "raw_decompressed",
        label: "Raw decompressed scan",
        vector: Vector::RawDecompressed,
        method: "inflate-all + scan",
        phase: "Phase 3",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_one_entry_per_vector_variant() {
        // 21 vectors in the spec §4.1 registry.
        assert_eq!(REGISTRY.len(), 21);
    }

    #[test]
    fn registry_ids_and_vectors_are_unique() {
        let mut ids: Vec<&str> = REGISTRY.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate check id in registry");

        let mut vectors: Vec<u32> = REGISTRY.iter().map(|c| c.vector() as u32).collect();
        vectors.sort_unstable();
        let n = vectors.len();
        vectors.dedup();
        assert_eq!(vectors.len(), n, "duplicate Vector in registry");
    }
}

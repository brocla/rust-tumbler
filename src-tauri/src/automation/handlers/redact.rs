//! redact_find, redact_apply.
//!
//! Redaction is Save As only, as in the GUI: the burned copy goes to a new
//! file and the open document is never redacted, so a plain doc_save can
//! never overwrite the original with it. Output that fails verification is
//! never written.

use super::{cancelled, reply, ToolResult};
use crate::automation::args::{RedactApplyArgs, RedactFindArgs};
use crate::automation::{failed, paths, Automation, ToolError};
use crate::commands::metadata::find_redaction_metadata_matches_impl;
use crate::commands::redact::{
    apply_redactions_impl, find_redaction_matches_impl, save_redacted_copy_impl, RedactProgress, RedactRegion,
};
use crate::commands::text::TextRect;
use crate::state::{lock_mutex, PendingRedaction};
use serde_json::{json, Value};

/// The GUI's default flatten resolution (RedactPanel).
const DEFAULT_DPI: f32 = 200.0;

pub(crate) fn redact_find(auto: &mut Automation, args: RedactFindArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let (mut regions, mut per_term) = (Vec::new(), Vec::new());
    for term in &args.terms {
        let found = find_redaction_matches_impl(
            &auto.state,
            key.clone(),
            term.clone(),
            args.match_case,
            args.whole_word,
            args.regex,
        )?;
        per_term.push(json!({ "term": term, "matches": found.len() }));
        regions.extend(found);
    }
    // As in the GUI, only a plain search looks at the metadata (see
    // verify_queries below).
    let metadata = if plain(args.match_case, args.whole_word, args.regex) {
        find_redaction_metadata_matches_impl(&auto.state, key, args.terms.clone())?
    } else {
        Vec::new()
    };
    reply(json!({
        "doc": args.doc,
        "regions": regions.iter().map(region_json).collect::<Vec<_>>(),
        "terms": per_term,
        "metadata_matches": metadata,
    }))
}

/// The GUI's Apply followed by its Save As, composed: regions from the terms
/// and rects, redaction and verification, staging, then the checked write.
pub(crate) fn redact_apply(auto: &mut Automation, args: RedactApplyArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let out = paths::output_path(auto.root(), &args.out)?;
    let out_existing = dunce::canonicalize(&out).ok();
    let entry = auto.entry(args.doc)?;
    let original = lock_mutex(&entry)?.file_path.clone();
    if out_existing.is_some_and(|p| p.to_string_lossy() == original) {
        return Err(failed(format!(
            "{} is the original file; redaction writes a copy, so give out a different file",
            out.display()
        )));
    }

    let terms = args.terms.unwrap_or_default();
    if terms.iter().any(|t| t.trim().is_empty()) {
        return Err(ToolError::InvalidArgs("redact_apply: a term is empty".into()));
    }
    let mut regions: Vec<RedactRegion> = Vec::new();
    for term in &terms {
        regions.extend(find_redaction_matches_impl(
            &auto.state,
            key.clone(),
            term.clone(),
            args.match_case,
            args.whole_word,
            args.regex,
        )?);
    }
    for r in args.rects.unwrap_or_default() {
        auto.check_page(args.doc, r.page)?;
        if !(r.width > 0.0 && r.height > 0.0) {
            return Err(ToolError::InvalidArgs("redact_apply: a rect's width and height must be positive".into()));
        }
        regions.push(RedactRegion { page: r.page, rect: TextRect { x: r.x, y: r.y, width: r.width, height: r.height } });
    }
    // A plain term must be gone from the output entirely, so it is verified
    // with a literal, case-insensitive search. A case-sensitive, whole-word or
    // regex find deliberately marks only some occurrences, which that check
    // would flag as leaks, so those terms are not verify queries (RedactPanel).
    let verify_queries = if plain(args.match_case, args.whole_word, args.regex) { terms } else { Vec::new() };
    if regions.is_empty() && verify_queries.is_empty() {
        return Err(ToolError::InvalidArgs("redact_apply: give terms or rects to redact".into()));
    }

    let bytes = lock_mutex(&entry)?.buffer.clone();
    let progress = auto.progress();
    let emit = move |p: RedactProgress| progress(p.page, p.total);
    let (result, output) = apply_redactions_impl(
        &emit,
        auto.state.pdfium,
        &bytes,
        &regions,
        &verify_queries,
        args.dpi.unwrap_or(DEFAULT_DPI),
        &auto.state.ocr_engine,
        &auto.cancel_token(),
    )?;
    if result.cancelled {
        return Err(cancelled());
    }
    let summary = json!({
        "doc": args.doc,
        "verified": result.verified,
        "regions": result.regions,
        "pages_flattened": result.pages_flattened,
        "reocr_pages": result.reocr_pages,
        "ocr_check_ran": result.ocr_check_ran,
    });
    let Some(output) = output.filter(|_| result.verified) else {
        let mut summary = summary;
        summary["saved"] = json!(false);
        summary["leaks"] = Value::Array(result.leaks.iter().map(region_json).collect());
        summary["structural_violations"] = json!(result.structural_violations);
        return reply(summary);
    };

    // Stage, then save through the GUI's own Save As path, which re-checks the
    // destination and re-encrypts a protected document's copy.
    let document = auto
        .state
        .pdfium
        .load_pdf_from_byte_vec(output.clone(), None)
        .map_err(|e| crate::error::AppError::pdfium("Failed to load redacted output", e))?;
    auto.state.set_pending_redaction(&key, PendingRedaction { document, bytes: output, verified: true })?;
    let saved = save_redacted_copy_impl(&auto.state, &key, &out.to_string_lossy());
    auto.state.clear_pending_redaction(&key);
    let path = saved?;
    let mut summary = summary;
    summary["saved"] = json!(true);
    summary["path"] = json!(path);
    reply(summary)
}

fn plain(match_case: bool, whole_word: bool, regex: bool) -> bool {
    !match_case && !whole_word && !regex
}

/// A region in the shape redact_apply's rects take, so one can be passed back.
fn region_json(r: &RedactRegion) -> Value {
    json!({ "page": r.page, "x": r.rect.x, "y": r.rect.y, "width": r.rect.width, "height": r.rect.height })
}

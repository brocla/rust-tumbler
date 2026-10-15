//! margins_analyze, margins_expand.

use super::{cancelled, edited, is_signed, reply, snake_json, with, ToolResult};
use crate::automation::args::MarginsArgs;
use crate::automation::Automation;
use crate::commands::margins::{analyze_margins_impl, expand_margins_bytes, uniform_scale};
use crate::state::lock_mutex;
use serde_json::json;

/// The GUI's default: a quarter inch.
const DEFAULT_PADDING: f32 = 18.0;

pub(crate) fn margins_analyze(auto: &mut Automation, args: MarginsArgs) -> ToolResult {
    let entry = auto.entry(args.doc)?;
    let padding = args.padding.unwrap_or(DEFAULT_PADDING);
    let progress = auto.progress();
    let report = analyze_margins_impl(&|done, total| progress(done, total), &entry, &auto.cancel_token())?;
    if report.cancelled {
        return Err(cancelled());
    }
    reply(json!({
        "doc": args.doc,
        "padding": padding,
        // What margins_expand would scale by; null when no page has content.
        "scale": uniform_scale(&report.pages, padding),
        "pages": snake_json(&report.pages)?,
    }))
}

/// The GUI's expand_margins, composed: detection re-run on the current
/// buffer, then the uniform fit, applied as a buffer edit.
pub(crate) fn margins_expand(auto: &mut Automation, args: MarginsArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let entry = auto.entry(args.doc)?;
    let padding = args.padding.unwrap_or(DEFAULT_PADDING);
    let signed = is_signed(auto, &key)?;
    let progress = auto.progress();
    let report = analyze_margins_impl(&|done, total| progress(done, total), &entry, &auto.cancel_token())?;
    if report.cancelled {
        return Err(cancelled());
    }
    let buffer = lock_mutex(&entry)?.buffer.clone();
    let (bytes, scale) = expand_margins_bytes(&buffer, &report.pages, padding)?;
    auto.state.set_buffer_and_refresh(&key, bytes)?;
    reply(with(edited(auto, args.doc, signed)?, json!({ "scale": scale, "padding": padding })))
}

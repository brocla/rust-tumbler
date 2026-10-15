//! doc_audit_images, doc_optimize.

use super::{cancelled, edited, is_signed, reply, snake_json, with, ToolResult};
use crate::automation::args::{DocArgs, DocOptimizeArgs, OptimizeStep};
use crate::automation::Automation;
use crate::commands::conformance::get_conformance_impl;
use crate::commands::optimize::{
    inspect_images_impl, run_optimization_steps_impl, CompressProgress, ImageOptions, StepId,
};
use crate::state::lock_mutex;
use serde_json::json;

/// The GUI's defaults (OptimizePanel): the image step is lossy, so it is
/// opt-in; the four lossless steps run unless told otherwise.
const DEFAULT_STEPS: [OptimizeStep; 4] = [
    OptimizeStep::RecompressStreams,
    OptimizeStep::PruneUnused,
    OptimizeStep::DeleteZeroLength,
    OptimizeStep::StripExtras,
];
const DEFAULT_DPI: f32 = 150.0;
const DEFAULT_QUALITY: u8 = 80;

/// Pipeline order, which the steps always run in.
const ORDER: [OptimizeStep; 5] = [
    OptimizeStep::RecompressStreams,
    OptimizeStep::PruneUnused,
    OptimizeStep::DeleteZeroLength,
    OptimizeStep::StripExtras,
    OptimizeStep::RecompressImages,
];

pub(crate) fn doc_audit_images(auto: &mut Automation, args: DocArgs) -> ToolResult {
    let bytes = buffer(auto, args.doc)?;
    reply(json!({ "images": snake_json(&inspect_images_impl(&bytes)?)? }))
}

pub(crate) fn doc_optimize(auto: &mut Automation, args: DocOptimizeArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let wanted = args.steps.unwrap_or_else(|| DEFAULT_STEPS.to_vec());
    let steps: Vec<StepId> = ORDER.iter().filter(|s| wanted.iter().any(|w| same(w, s))).map(step_id).collect();
    let opts = ImageOptions {
        target_dpi: args.dpi.unwrap_or(DEFAULT_DPI),
        jpeg_quality: args.quality.unwrap_or(DEFAULT_QUALITY),
        reencode_at_target: args.reencode_at_target,
    };

    // What the Compress panel warns about before running: a signature, and a
    // declared PDF/A or PDF/X claim, which optimizing voids.
    let signed = is_signed(auto, &key)?;
    let declared = get_conformance_impl(&auto.state, key.clone())?.declared;
    let breaks: Vec<&String> = declared.iter().filter(|c| c.starts_with("PDF/A") || c.starts_with("PDF/X")).collect();

    let bytes = buffer(auto, args.doc)?;
    let progress = auto.progress();
    let emit = move |p: CompressProgress| progress(p.step_index, p.step_count);
    let (report, output) = run_optimization_steps_impl(&emit, &bytes, steps, opts, &auto.cancel_token())?;
    let Some(output) = output else {
        return Err(cancelled());
    };
    auto.state.set_buffer_and_refresh(&key, output)?;
    reply(with(
        with(edited(auto, args.doc, signed)?, snake_json(&report)?),
        json!({ "breaks_conformance": breaks }),
    ))
}

fn buffer(auto: &Automation, doc: u64) -> Result<Vec<u8>, crate::automation::ToolError> {
    let entry = auto.entry(doc)?;
    let bytes = lock_mutex(&entry)?.buffer.clone();
    Ok(bytes)
}

fn same(a: &OptimizeStep, b: &OptimizeStep) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn step_id(step: &OptimizeStep) -> StepId {
    match step {
        OptimizeStep::RecompressStreams => StepId::RecompressStreams,
        OptimizeStep::PruneUnused => StepId::PruneUnused,
        OptimizeStep::DeleteZeroLength => StepId::DeleteZeroLength,
        OptimizeStep::StripExtras => StepId::StripExtras,
        OptimizeStep::RecompressImages => StepId::RecompressImages,
    }
}

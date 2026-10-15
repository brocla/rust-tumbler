//! doc_linearize.

use super::{reply, ToolResult};
use crate::automation::args::DocLinearizeArgs;
use crate::automation::{paths, Automation};
use crate::commands::linearize::export_linearized_copy_impl;
use serde_json::json;

pub(crate) fn doc_linearize(auto: &mut Automation, args: DocLinearizeArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let target = paths::output_path(auto.root(), &args.path)?;
    let target = target.to_string_lossy().into_owned();
    let result = export_linearized_copy_impl(&auto.state, &key, &target)?;
    reply(json!({
        "doc": args.doc,
        "path": target,
        "original_size": result.original_size,
        "linearized_size": result.linearized_size,
    }))
}

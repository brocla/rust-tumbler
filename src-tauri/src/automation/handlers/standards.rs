//! sign_list, doc_conformance.

use super::{json, snake_json, ToolResult};
use crate::automation::args::DocArgs;
use crate::automation::Automation;
use crate::commands::{conformance, signature};
use serde_json::json;

pub(crate) fn sign_list(auto: &mut Automation, args: DocArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    json(snake_json(&signature::get_signature_info_impl(&auto.state, key)?)?)
}

pub(crate) fn doc_conformance(auto: &mut Automation, args: DocArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let claims = conformance::get_conformance_impl(&auto.state, key)?;
    json(json!({ "declared": claims.declared }))
}

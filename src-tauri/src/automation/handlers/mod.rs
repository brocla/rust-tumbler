//! Tool handlers, one file per command domain as in `commands/`. Each takes
//! its parsed argument struct, calls the existing `_impl` functions, and
//! shapes the JSON result.

pub(super) mod document;
pub(super) mod forms;
pub(super) mod linearize;
pub(super) mod margins;
pub(super) mod metadata;
pub(super) mod ocr;
pub(super) mod optimize;
pub(super) mod pages;
pub(super) mod redact;
pub(super) mod render;
pub(super) mod standards;
pub(super) mod text;
pub(super) mod typewriter;

use super::{failed, Automation, Content, ToolError};
use crate::commands::signature::{get_signature_info_impl, SignatureStatus};
use crate::state::lock_mutex;
use serde::Serialize;
use serde_json::{json, Map, Value};

type ToolResult = Result<Vec<Content>, ToolError>;

/// How a long tool reports that it was cancelled. Cancelled edits apply
/// nothing, so this is a failure rather than a partial result.
const CANCELLED: &str = "cancelled: nothing was changed";

fn cancelled() -> ToolError {
    failed(CANCELLED)
}

fn reply(value: Value) -> ToolResult {
    Ok(vec![Content::Json(value)])
}

/// Whether the document carries a signature that an edit would break. The
/// GUI asks before such an edit (confirmBreakingEdit); headless, the edit
/// goes ahead and its result says so.
fn is_signed(auto: &Automation, key: &str) -> Result<bool, ToolError> {
    Ok(get_signature_info_impl(&auto.state, key.to_string())?.status != SignatureStatus::Unsigned)
}

/// The fields every editing tool reports after its edit, so a caller never
/// has to ask again.
fn edited(auto: &Automation, doc: u64, invalidates_signatures: bool) -> Result<Value, ToolError> {
    let entry = auto.entry(doc)?;
    let entry = lock_mutex(&entry)?;
    Ok(json!({
        "doc": doc,
        "dirty": entry.dirty,
        "page_count": entry.document.pages().len(),
        "encrypted": entry.protection.is_encrypted(),
        "invalidates_signatures": invalidates_signatures,
    }))
}

/// Adds `extra`'s fields to the object `base`.
fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

/// A GUI result type as JSON with snake_case keys. The GUI's types serialize
/// camelCase for TypeScript; tool results use snake_case throughout, matching
/// their argument names.
///
/// Only for struct-shaped values: a map keyed by data (form field names, say)
/// would have its keys rewritten too.
fn snake_json(value: &impl Serialize) -> Result<Value, ToolError> {
    let value = serde_json::to_value(value).map_err(super::failed)?;
    Ok(snake_keys(value))
}

fn snake_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter().map(|(k, v)| (snake_case(&k), snake_keys(v))).collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(snake_keys).collect()),
        other => other,
    }
}

fn snake_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snake_keys_converts_nested_keys_but_not_values() {
        let v = json!({ "modDate": "camelValue", "inner": [{ "byteRange": 1 }], "ok": true });
        assert_eq!(
            snake_keys(v),
            json!({ "mod_date": "camelValue", "inner": [{ "byte_range": 1 }], "ok": true })
        );
    }
}

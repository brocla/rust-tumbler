//! Tool handlers, one file per command domain as in `commands/`. Each takes
//! its parsed argument struct, calls the existing `_impl` functions, and
//! shapes the JSON result.

pub(super) mod document;
pub(super) mod render;
pub(super) mod standards;
pub(super) mod text;

use super::{Content, ToolError};
use serde::Serialize;
use serde_json::{Map, Value};

type ToolResult = Result<Vec<Content>, ToolError>;

fn json(value: Value) -> ToolResult {
    Ok(vec![Content::Json(value)])
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

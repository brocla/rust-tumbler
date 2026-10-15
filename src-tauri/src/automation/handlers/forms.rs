//! form_fields, form_fill, form_reset.

use super::{edited, is_signed, reply, snake_json, with, ToolResult};
use crate::automation::args::{DocPageArgs, FormFillArgs, FormResetArgs};
use crate::automation::{failed, Automation, ToolError};
use crate::commands::forms::{
    get_form_fields_impl, reset_scope_impl, set_form_field_value_impl, FieldType, FormField, ResetScope,
};
use serde_json::json;

pub(crate) fn form_fields(auto: &mut Automation, args: DocPageArgs) -> ToolResult {
    let fields = all_fields(auto, args.doc, args.page)?;
    reply(json!({ "doc": args.doc, "fields": snake_json(&fields)? }))
}

/// Sets several fields at once. Every name and choice is checked first, so
/// one typo writes nothing.
pub(crate) fn form_fill(auto: &mut Automation, args: FormFillArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    if args.values.is_empty() {
        return Err(ToolError::InvalidArgs("form_fill: values is empty".into()));
    }
    let fields = all_fields(auto, args.doc, None)?;
    for (name, value) in &args.values {
        check_fill(&fields, name, value)?;
    }
    let signed = is_signed(auto, &key)?;
    for (name, value) in &args.values {
        set_form_field_value_impl(&auto.state, key.clone(), name.clone(), value.clone())?;
    }
    reply(with(edited(auto, args.doc, signed)?, json!({ "filled": args.values.len() })))
}

pub(crate) fn form_reset(auto: &mut Automation, args: FormResetArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let scope = match args.fields {
        None => ResetScope::All,
        Some(names) => {
            let fields = all_fields(auto, args.doc, None)?;
            if let Some(unknown) = names.iter().find(|n| !fields.iter().any(|f| &f.id == *n)) {
                return Err(failed(format!("no form field named {unknown:?} (form_fields lists them)")));
            }
            ResetScope::Only(names)
        }
    };
    let signed = is_signed(auto, &key)?;
    reset_scope_impl(&auto.state, key, scope)?;
    reply(edited(auto, args.doc, signed)?)
}

/// Every field on `page`, or on every page.
fn all_fields(auto: &Automation, doc: u64, page: Option<u32>) -> Result<Vec<FormField>, ToolError> {
    let key = auto.key(doc)?;
    let pages = match page {
        Some(p) => vec![auto.check_page(doc, p)?],
        None => (1..=auto.page_count(doc)?).collect(),
    };
    let mut fields = Vec::new();
    for p in pages {
        fields.extend(get_form_fields_impl(&auto.state, key.clone(), p)?);
    }
    Ok(fields)
}

/// Whether `value` can go into the field `name`. A radio group lists one
/// FormField per button, all with the same id.
fn check_fill(fields: &[FormField], name: &str, value: &str) -> Result<(), ToolError> {
    let controls: Vec<&FormField> = fields.iter().filter(|f| f.id == name).collect();
    let Some(first) = controls.first() else {
        return Err(failed(format!("no form field named {name:?} (form_fields lists them)")));
    };
    if controls.iter().any(|f| f.read_only) {
        return Err(failed(format!("form field {name:?} is read-only")));
    }
    match first.field_type {
        FieldType::Checkbox | FieldType::Radio => {
            let mut allowed: Vec<&str> = controls.iter().map(|f| f.export_value.as_str()).collect();
            allowed.push("Off");
            if !allowed.contains(&value) {
                return Err(failed(format!("form field {name:?} takes one of: {}", allowed.join(", "))));
            }
        }
        FieldType::Button | FieldType::Signature => {
            return Err(failed(format!("form field {name:?} is a button or signature field, which has no value to fill")));
        }
        _ => {}
    }
    Ok(())
}

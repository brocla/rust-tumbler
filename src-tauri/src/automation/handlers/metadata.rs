//! doc_set_info, doc_protect, doc_unprotect.

use super::{edited, is_signed, reply, snake_json, with, ToolResult};
use crate::automation::args::{DocArgs, DocProtectArgs, DocSetInfoArgs};
use crate::automation::{Automation, ToolError};
use crate::commands::{encryption, metadata};
use serde_json::json;

pub(crate) fn doc_set_info(auto: &mut Automation, args: DocSetInfoArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let given = [&args.title, &args.author, &args.subject, &args.keywords, &args.creator];
    if given.iter().all(|f| f.is_none()) {
        return Err(ToolError::InvalidArgs(
            "doc_set_info: give at least one of title, author, subject, keywords, creator".into(),
        ));
    }
    // set_metadata writes all five fields, so fill the ones not given with
    // their current values.
    let current = metadata::get_metadata_impl(&auto.state, key.clone())?;
    let update = metadata::MetadataUpdate {
        title: args.title.unwrap_or(current.title),
        author: args.author.unwrap_or(current.author),
        subject: args.subject.unwrap_or(current.subject),
        keywords: args.keywords.unwrap_or(current.keywords),
        creator: args.creator.unwrap_or(current.creator),
    };
    let signed = is_signed(auto, &key)?;
    let written = metadata::set_metadata_impl(&auto.state, key, update)?;
    reply(with(edited(auto, args.doc, signed)?, json!({ "metadata": snake_json(&written)? })))
}

pub(crate) fn doc_protect(auto: &mut Automation, args: DocProtectArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let signed = is_signed(auto, &key)?;
    encryption::set_password_impl(&auto.state, &key, &args.password)?;
    reply(edited(auto, args.doc, signed)?)
}

pub(crate) fn doc_unprotect(auto: &mut Automation, args: DocArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let signed = is_signed(auto, &key)?;
    encryption::remove_password_impl(&auto.state, &key)?;
    reply(edited(auto, args.doc, signed)?)
}

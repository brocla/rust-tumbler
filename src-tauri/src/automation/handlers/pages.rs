//! page_delete, page_rotate, page_reorder, page_insert_file, doc_split.

use super::{edited, is_signed, reply, with, ToolResult};
use crate::automation::args::{
    DocSplitArgs, PageDeleteArgs, PageInsertFileArgs, PageReorderArgs, PageRotateArgs,
};
use crate::automation::{failed, paths, Automation, ToolError};
use crate::commands::pages;
use crate::state::lock_mutex;
use serde_json::json;
use std::path::Path;

pub(crate) fn page_delete(auto: &mut Automation, args: PageDeleteArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    for &p in &args.pages {
        auto.check_page(args.doc, p)?;
    }
    let signed = is_signed(auto, &key)?;
    pages::delete_pages_impl(&auto.state, key, args.pages)?;
    reply(edited(auto, args.doc, signed)?)
}

pub(crate) fn page_rotate(auto: &mut Automation, args: PageRotateArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    if args.degrees % 90 != 0 {
        return Err(ToolError::InvalidArgs(format!(
            "page_rotate: degrees must be a multiple of 90, not {}",
            args.degrees
        )));
    }
    let pages = match args.pages {
        Some(pages) => pages.into_iter().map(|p| auto.check_page(args.doc, p)).collect::<Result<Vec<_>, _>>()?,
        None => (1..=auto.page_count(args.doc)?).collect(),
    };
    // -90 is three clockwise quarter turns.
    let turns = (args.degrees / 90).rem_euclid(4) as u32;
    let signed = is_signed(auto, &key)?;
    pages::rotate_pages_impl(&auto.state, key, pages, turns)?;
    reply(edited(auto, args.doc, signed)?)
}

pub(crate) fn page_reorder(auto: &mut Automation, args: PageReorderArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let count = auto.page_count(args.doc)?;
    let mut sorted = args.order.clone();
    sorted.sort_unstable();
    if sorted != (1..=count).collect::<Vec<_>>() {
        return Err(ToolError::InvalidArgs(format!(
            "page_reorder: order must list each of pages 1 to {count} exactly once"
        )));
    }
    let signed = is_signed(auto, &key)?;
    pages::reorder_pages_impl(&auto.state, key, args.order)?;
    reply(edited(auto, args.doc, signed)?)
}

pub(crate) fn page_insert_file(auto: &mut Automation, args: PageInsertFileArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let count = auto.page_count(args.doc)?;
    let after = args.after.unwrap_or(count);
    if after > count {
        return Err(ToolError::InvalidArgs(format!(
            "page_insert_file: after must be 0 to {count}, not {after}"
        )));
    }
    let source = paths::input_path(auto.root(), &args.path)?;
    let signed = is_signed(auto, &key)?;
    let before = count;
    pages::merge_document_impl(&auto.state, key, source.to_string_lossy().into_owned(), after)?;
    let result = edited(auto, args.doc, signed)?;
    let inserted = result["page_count"].as_u64().unwrap_or(0).saturating_sub(before as u64);
    reply(with(result, json!({ "inserted_pages": inserted })))
}

pub(crate) fn doc_split(auto: &mut Automation, args: DocSplitArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let count = auto.page_count(args.doc)?;
    let ranges: Vec<(u32, u32)> = match (args.every, args.ranges) {
        (Some(_), Some(_)) => {
            return Err(ToolError::InvalidArgs("doc_split: give every or ranges, not both".into()))
        }
        (_, Some(ranges)) => ranges.iter().map(|r| (r.first, r.last)).collect(),
        (every, None) => {
            let every = every.unwrap_or(1);
            (1..=count).step_by(every as usize).map(|first| (first, (first + every - 1).min(count))).collect()
        }
    };
    for &(first, last) in &ranges {
        if first > last || last > count {
            return Err(ToolError::InvalidArgs(format!(
                "doc_split: range {first}-{last} is not within pages 1 to {count}"
            )));
        }
    }

    let out_dir = paths::output_dir(auto.root(), &args.out_dir)?;
    let stem = {
        let entry = auto.entry(args.doc)?;
        let path = lock_mutex(&entry)?.file_path.clone();
        Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "split".into())
    };
    let mut targets = Vec::with_capacity(ranges.len());
    for &(first, last) in &ranges {
        let name = if first == last { format!("{stem}-{first}.pdf") } else { format!("{stem}-{first}-{last}.pdf") };
        let target = paths::output_path(auto.root(), &out_dir.join(name).to_string_lossy())?;
        targets.push(target);
    }
    // Split only ever creates files: check them all before writing any.
    if let Some(existing) = targets.iter().find(|t| t.exists()) {
        return Err(failed(format!(
            "{} already exists; doc_split never replaces files",
            existing.display()
        )));
    }

    let mut files = Vec::with_capacity(ranges.len());
    for (&(first, last), target) in ranges.iter().zip(&targets) {
        let target = target.to_string_lossy().into_owned();
        pages::split_document_impl(&auto.state, key.clone(), first, last, target.clone())?;
        files.push(json!({ "path": target, "first_page": first, "last_page": last }));
    }
    reply(json!({ "doc": args.doc, "files": files }))
}

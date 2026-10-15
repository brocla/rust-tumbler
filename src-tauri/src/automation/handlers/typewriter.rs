//! typewriter_list, typewriter_set, ink_add.

use super::{edited, is_signed, reply, snake_json, with, ToolResult};
use crate::automation::args::{DocPageArgs, InkAddArgs, NoteArgs, NoteFont, TypewriterSetArgs};
use crate::automation::{Automation, ToolError};
use crate::commands::ink::apply_ink_impl;
use crate::commands::typewriter::{apply_typewriter_impl, read_typewriter_annots, TypewriterAnnot};
use crate::state::lock_mutex;
use serde_json::json;

const DEFAULT_FONT_SIZE: f32 = 12.0;

pub(crate) fn typewriter_list(auto: &mut Automation, args: DocPageArgs) -> ToolResult {
    if let Some(page) = args.page {
        auto.check_page(args.doc, page)?;
    }
    let notes: Vec<TypewriterAnnot> =
        notes_of(auto, args.doc)?.into_iter().filter(|n| args.page.is_none_or(|p| n.page == p)).collect();
    reply(json!({ "doc": args.doc, "notes": snake_json(&notes)? }))
}

/// Replaces one page's notes. The GUI writes the whole document's notes at
/// once, so the other pages' notes are read back and written with them.
pub(crate) fn typewriter_set(auto: &mut Automation, args: TypewriterSetArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let page = auto.check_page(args.doc, args.page)?;
    for (i, n) in args.notes.iter().enumerate() {
        check_note(i, n)?;
    }
    let mut notes: Vec<TypewriterAnnot> = notes_of(auto, args.doc)?.into_iter().filter(|n| n.page != page).collect();
    let count = args.notes.len();
    notes.extend(args.notes.into_iter().map(|n| TypewriterAnnot {
        id: uuid::Uuid::new_v4().to_string(),
        page,
        x: n.x,
        y: n.y,
        width: n.width,
        height: n.height,
        text: n.text,
        font_family: match n.font_family.unwrap_or(NoteFont::Helvetica) {
            NoteFont::Helvetica => "Helvetica",
            NoteFont::Times => "Times",
            NoteFont::Courier => "Courier",
        }
        .to_string(),
        bold: n.bold,
        italic: n.italic,
        font_size: n.font_size.unwrap_or(DEFAULT_FONT_SIZE),
        color: n.color.unwrap_or([0.0, 0.0, 0.0]),
        // Upright on the page as displayed, as a note the user types is.
        rotation: 0,
    }));
    let signed = is_signed(auto, &key)?;
    let changed = apply_typewriter_impl(&auto.state, key, notes)?;
    reply(with(edited(auto, args.doc, signed && changed)?, json!({ "page": page, "notes": count })))
}

pub(crate) fn ink_add(auto: &mut Automation, args: InkAddArgs) -> ToolResult {
    let key = auto.key(args.doc)?;
    let page = auto.check_page(args.doc, args.page)?;
    if args.strokes.iter().all(|s| s.is_empty()) {
        return Err(ToolError::InvalidArgs("ink_add: the strokes have no points".into()));
    }
    let signed = is_signed(auto, &key)?;
    let changed = apply_ink_impl(&auto.state, key, page, args.strokes)?;
    reply(edited(auto, args.doc, signed && changed)?)
}

fn notes_of(auto: &Automation, doc: u64) -> Result<Vec<TypewriterAnnot>, ToolError> {
    let entry = auto.entry(doc)?;
    let buffer = lock_mutex(&entry)?.buffer.clone();
    Ok(read_typewriter_annots(&buffer)?)
}

fn check_note(i: usize, n: &NoteArgs) -> Result<(), ToolError> {
    let bad = |why: &str| ToolError::InvalidArgs(format!("typewriter_set: note {}: {why}", i + 1));
    if !(n.width > 0.0 && n.height > 0.0) {
        return Err(bad("width and height must be positive"));
    }
    if n.font_size.is_some_and(|s| !(1.0..=200.0).contains(&s)) {
        return Err(bad("font_size must be between 1 and 200"));
    }
    if n.color.is_some_and(|c| c.iter().any(|v| !(0.0..=1.0).contains(v))) {
        return Err(bad("color components must be between 0 and 1"));
    }
    Ok(())
}

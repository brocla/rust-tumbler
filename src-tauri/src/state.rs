use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct DocEntry {
    pub document: PdfDocument<'static>,
    pub file_path: String,
}

pub struct AppState {
    pub pdfium: &'static Pdfium,
    pub documents: Mutex<HashMap<String, DocEntry>>,
}

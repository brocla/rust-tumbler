use crate::commands::ocr::{
    cache_get, ocr_page_into_cache, ocr_words_to_line_groups, ocr_words_to_lines, ocr_words_to_text,
    OcrCache, OcrEngine, OcrLine, OcrProgress,
};
use regex::RegexBuilder;
use crate::error::AppError;
use crate::state::{lock_mutex, AppState, DocEntry};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State, WebviewWindow};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TextRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextItem {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub font_size: f32,
}

/// One occurrence of the query on a page.
///
/// `rects` holds the highlight boxes for that single occurrence — normally one,
/// but a match broken across a line break legitimately needs two. It is *not*
/// one rect per character: see [`merge_line_runs`].
#[derive(Serialize, Debug)]
pub struct SearchMatch {
    pub rects: Vec<TextRect>,
}

#[derive(Serialize, Debug)]
pub struct SearchResult {
    pub page: u32,
    pub matches: Vec<SearchMatch>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextExportResult {
    pub pages: u32,
    /// How many pages contributed text via OCR (vs. a native text layer).
    pub ocr_pages: u32,
    pub cancelled: bool,
}

/// Returns the effective left and bottom origin of the page's bounding box.
/// Most PDFs have origin (0,0), but some have non-zero origins that shift
/// text coordinates relative to the rendered output.
pub(crate) fn page_origin(page: &PdfPage) -> (f32, f32) {
    // Try CropBox first (used for display), fall back to MediaBox
    let bbox = page
        .boundaries()
        .crop()
        .or_else(|_| page.boundaries().media());

    match bbox {
        Ok(b) => (b.bounds.left().value, b.bounds.bottom().value),
        Err(_) => (0.0, 0.0),
    }
}

/// Returns a page's full text by walking its characters in document order and
/// concatenating their Unicode values.
///
/// Prefer this over `PdfPageText::all()` for anything that consumes the text
/// content. `all()` routes through pdfium's `FPDFText_GetBoundedText`, which
/// reconstructs reading order from glyph geometry; that reconstruction is
/// unreliable on rotated / absolutely-positioned multi-column layouts and can
/// silently drop or scramble whole regions of a page (issue #80). Walking
/// characters in the order they are defined in the content stream avoids the
/// geometric sort entirely — it is the correct reading order for well-authored
/// documents and preserves any line breaks encoded into the glyph stream.
pub(crate) fn page_text_in_document_order(text: &PdfPageText) -> String {
    page_text_with_char_index(text).text
}

/// A page's text together with the pdfium character index each piece came from.
///
/// Regex search needs both: the regex reports **byte** offsets into `text`,
/// while pdfium's `segments_subset` — the call that yields highlight rectangles
/// — wants **character indices**. The two do not line up. `text` is built with
/// `filter_map`, silently dropping any character pdfium cannot decode, and
/// every drop shifts the correspondence. Recovering the index by counting
/// characters (`text[..m.start()].chars().count()`) is therefore wrong on
/// exactly the documents most likely to contain odd encodings, and wrong
/// quietly — it returns rectangles for the wrong glyphs rather than failing.
pub(crate) struct PageText {
    pub text: String,
    /// `(byte offset in `text`, pdfium character index)` for every character
    /// actually appended, in ascending order of both.
    offsets: Vec<(usize, usize)>,
}

impl PageText {
    /// Maps a byte range of `text` onto the `(start, count)` character range
    /// `PdfPageText::segments_subset` expects, or `None` if the range covers no
    /// characters.
    ///
    /// The count spans from the first to the last character of the match
    /// *inclusive of anything dropped in between*, since those undecodable
    /// characters still sit physically between the two on the page.
    pub fn pdfium_range(&self, byte_start: usize, byte_end: usize) -> Option<(usize, usize)> {
        let first = self.offsets.partition_point(|(b, _)| *b < byte_start);
        let last = self.offsets.partition_point(|(b, _)| *b < byte_end);
        if first >= last {
            return None;
        }
        let start = self.offsets[first].1;
        let end = self.offsets[last - 1].1;
        Some((start, end - start + 1))
    }
}

/// Builds a page's text in document order, recording where each character came
/// from. See [`PageText`] for why the mapping cannot be reconstructed after the
/// fact, and [`page_text_in_document_order`] for the reading-order rationale.
pub(crate) fn page_text_with_char_index(text: &PdfPageText) -> PageText {
    let mut out = String::new();
    let mut offsets = Vec::new();
    for ch in text.chars().iter() {
        if let Some(c) = ch.unicode_char() {
            // The char's own index, not the loop counter: the two agree today
            // but only the former is pdfium's answer.
            offsets.push((out.len(), ch.index()));
            out.push(c);
        }
    }
    PageText { text: out, offsets }
}

/// Collapses the rects of a *single* match into one box per line.
///
/// pdfium's `FPDFText_CountRects` starts a new rectangle at every text-object
/// change, and a great many real PDFs place each glyph in its own show-text
/// operator — one `Td … Tj` per character inside a shared `BT … ET`. On such a
/// document a search for an eight-letter word comes back as eight rects, which
/// draws as eight gapped boxes and (before this grouping existed) counted as
/// eight separate matches to step through.
///
/// Every rect passed here belongs to the same occurrence of the query, so
/// unioning the ones that share a line is exactly the match's extent on that
/// line. A match that wraps keeps one rect per line, which is what a highlight
/// should look like.
///
/// Lines are found by vertical overlap rather than by equal `y`: per-character
/// boxes are tight, so a capital and a lowercase letter on the same baseline
/// differ in both top edge and height.
pub(crate) fn merge_line_runs(rects: Vec<TextRect>) -> Vec<TextRect> {
    let mut lines: Vec<TextRect> = Vec::new();

    for rect in rects {
        match lines.iter_mut().find(|line| shares_line(line, &rect)) {
            Some(line) => *line = union_rect(line, &rect),
            None => lines.push(rect),
        }
    }

    // Reading order: top to bottom, then left to right.
    lines.sort_by(|a, b| {
        a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x))
    });
    lines
}

/// True when two rects overlap vertically by more than half the shorter one —
/// the test for "on the same line" (y is top-left origin, so a rect spans
/// `y ..= y + height`).
fn shares_line(a: &TextRect, b: &TextRect) -> bool {
    let overlap = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    overlap > 0.5 * a.height.min(b.height)
}

fn union_rect(a: &TextRect, b: &TextRect) -> TextRect {
    let left = a.x.min(b.x);
    let top = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    TextRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }
}

#[tauri::command]
pub fn extract_page_text(
    state: State<'_, AppState>,
    doc_id: String,
    page: u32,
) -> Result<Vec<TextItem>, String> {
    extract_page_text_impl(&state, doc_id, page).map_err(String::from)
}

pub(crate) fn extract_page_text_impl(
    state: &AppState,
    doc_id: String,
    page: u32,
) -> Result<Vec<TextItem>, AppError> {
    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

    let pdf_page = entry
        .document
        .pages()
        .get(page.saturating_sub(1) as i32)
        .map_err(|e| AppError::pdfium(format!("Failed to get page {page}"), e))?;

    let page_height = pdf_page.height().value;
    let (origin_x, origin_y) = page_origin(&pdf_page);

    // A force-re-OCR'd page (issue #97) serves its recognized words *instead
    // of* the native layer — the user has told us that layer is junk, so the
    // usual "native text wins" rule is exactly backwards here.
    if state.ocr_overrides_native_text(&doc_id, page) {
        if let Some(words) = state.get_ocr_words(&doc_id, page) {
            return Ok(ocr_words_to_lines(&words)
                .iter()
                .map(|line| ocr_line_to_text_item(line, page_height))
                .collect());
        }
    }

    let text = pdf_page
        .text()
        .map_err(|e| AppError::pdfium("Failed to get text", e))?;

    let mut items: Vec<TextItem> = Vec::new();
    let mut current_text = String::new();
    let mut current_x: f32 = 0.0;
    let mut current_y: f32 = 0.0;
    let mut current_width: f32 = 0.0;
    let mut current_height: f32 = 0.0;
    let mut current_font_size: f32 = 0.0;
    let mut has_current = false;

    for ch in text.chars().iter() {
        let unicode = match ch.unicode_char() {
            Some(c) => c,
            None => continue,
        };

        // Get character bounds
        let bounds = match ch.loose_bounds() {
            Ok(b) => b,
            Err(_) => continue,
        };

        let font_size = ch.scaled_font_size().value;

        // Convert PDF coordinates (origin bottom-left) to top-left origin,
        // adjusting for any non-zero page origin
        let char_x = bounds.left().value - origin_x;
        let char_y = page_height - (bounds.top().value - origin_y);
        let char_w = bounds.right().value - bounds.left().value;
        let char_h = bounds.top().value - bounds.bottom().value;

        // Group characters into text runs based on proximity and font size
        let same_line = has_current
            && (font_size - current_font_size).abs() < 0.5
            && (char_y - current_y).abs() < current_height * 0.5
            && (char_x - (current_x + current_width)).abs() < font_size * 0.5;

        if same_line {
            current_text.push(unicode);
            current_width = (char_x + char_w) - current_x;
            if char_h > current_height {
                current_height = char_h;
            }
        } else {
            // Flush previous item
            if has_current && !current_text.trim().is_empty() {
                items.push(TextItem {
                    text: current_text.clone(),
                    x: current_x,
                    y: current_y,
                    width: current_width,
                    height: current_height,
                    font_size: current_font_size,
                });
            }
            // Start new run
            current_text = String::from(unicode);
            current_x = char_x;
            current_y = char_y;
            current_width = char_w;
            current_height = char_h;
            current_font_size = font_size;
            has_current = true;
        }
    }

    // Flush last item
    if has_current && !current_text.trim().is_empty() {
        items.push(TextItem {
            text: current_text,
            x: current_x,
            y: current_y,
            width: current_width,
            height: current_height,
            font_size: current_font_size,
        });
    }

    // Fallback: a page with no native text layer (a scan) yields nothing above.
    // If OCR has been run for it, serve the recognized words — grouped into
    // lines — so the overlay has selectable spans whose copied text reads
    // correctly (words joined with spaces, one span per line).
    if items.is_empty() {
        if let Some(words) = state.get_ocr_words(&doc_id, page) {
            return Ok(ocr_words_to_lines(&words)
                .iter()
                .map(|line| ocr_line_to_text_item(line, page_height))
                .collect());
        }
    }

    Ok(items)
}

/// Converts a cached OCR line (PDF user space, origin bottom-left) into a
/// `TextItem` (origin top-left, as the text overlay expects). The font size is
/// approximated from the box height since OCR has no glyph metrics.
fn ocr_line_to_text_item(line: &OcrLine, page_height: f32) -> TextItem {
    TextItem {
        text: line.text.clone(),
        x: line.rect.x,
        y: page_height - (line.rect.y + line.rect.height),
        width: line.rect.width,
        height: line.rect.height,
        font_size: line.rect.height,
    }
}

#[tauri::command]
pub fn search_document(
    state: State<'_, AppState>,
    doc_id: String,
    query: String,
    match_case: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<Vec<SearchResult>, String> {
    search_document_impl(&state, doc_id, query, match_case, whole_word, use_regex)
        .map_err(String::from)
}

pub(crate) fn search_document_impl(
    state: &AppState,
    doc_id: String,
    query: String,
    match_case: bool,
    whole_word: bool,
    use_regex: bool,
) -> Result<Vec<SearchResult>, AppError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    // Compile the regex once (before any page loop) if regex mode is active.
    // An invalid pattern returns an error immediately.
    //
    // Multi-line and CRLF modes are on by default so `^` and `$` mean what a
    // user coming from any text editor expects — the start and end of a *line*
    // — without having to know that a page is matched as one long string.
    // Inline flags still win, so `(?-m)^Invoice` restores whole-page anchoring.
    //
    // CRLF mode is not merely a convenience. pdfium extracts pages with `\r\n`
    // endings, and plain multi-line `$` matches only immediately before the
    // `\n` — i.e. *after* the `\r` — so a line ending in the query never
    // matches at all. It also stops `.` from swallowing the `\r`, which matters
    // because a match carrying a trailing carriage return would map to
    // rectangles that include the line break.
    let regex_pattern = if use_regex {
        Some(
            RegexBuilder::new(&query)
                .multi_line(true)
                .crlf(true)
                .build()
                .map_err(|e| AppError::Other(format!("Invalid regex: {e}")))?,
        )
    } else {
        None
    };

    let entry = state.get_document(&doc_id)?;
    let entry = lock_mutex(&entry)?;

    let page_count = entry.document.pages().len();
    let mut results = Vec::new();

    let options = PdfSearchOptions::new()
        .match_case(match_case)
        .match_whole_word(whole_word);

    for page_idx in 0..page_count {
        let pdf_page = match entry.document.pages().get(page_idx as i32) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let page_height = pdf_page.height().value;
        let (origin_x, origin_y) = page_origin(&pdf_page);

        let text = match pdf_page.text() {
            Ok(t) => t,
            Err(_) => continue,
        };

        let page_num = (page_idx + 1) as u32;
        let mut page_matches: Vec<SearchMatch> = Vec::new();

        // A force-re-OCR'd page (issue #97): the user has declared its native
        // text layer junk, so don't search that layer at all. Leaving
        // `page_matches` empty routes the page into the cached-OCR block below,
        // which is the same path a plain scan takes.
        let search_native = !state.ocr_overrides_native_text(&doc_id, page_num);

        if !search_native {
            // Nothing to do — the OCR fallback below owns this page.
        } else if let Some(ref re) = regex_pattern {
            // Regex mode: match against the page's text, then take each match's
            // rectangles straight from its character offsets.
            //
            // This used to re-search the page for the matched *string* and take
            // whatever pdfium found. That made an anchor decide only whether a
            // page had any hit at all: `^Total` matched one line, then every
            // "Total" on the page lit up, mid-line ones included. Going through
            // the offsets means a match highlights the occurrence the regex
            // actually selected — and the dedup that re-search needed (to stop
            // one string returning its page-wide hits once per match) goes with
            // it, so matches now come back in reading order rather than in
            // whatever order a HashSet produced.
            let page = page_text_with_char_index(&text);
            for m in re.find_iter(&page.text) {
                let Some((start, count)) = page.pdfium_range(m.start(), m.end()) else {
                    continue;
                };
                page_matches.extend(segments_to_match(
                    &text.segments_subset(start, count),
                    page_height,
                    origin_x,
                    origin_y,
                ));
            }
        } else {
            // Non-regex mode: delegate match_case / whole_word to pdfium.
            let search = match text.search(&query, &options) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Each hit yields one PdfPageTextSegments — the rects covering that
            // single occurrence, from FPDFText_GetRect (pdfium's canonical
            // highlight-position function). One hit becomes one SearchMatch.
            for match_segments in search.iter(PdfSearchDirection::SearchForward) {
                page_matches.extend(segments_to_match(
                    &match_segments,
                    page_height,
                    origin_x,
                    origin_y,
                ));
            }
        }

        // No native hits on this page — either it has no text layer, or its
        // layer was skipped as junk above. If OCR words are cached for it (a
        // scanned page made searchable), match the query against them.
        if page_matches.is_empty() {
            if let Some(words) = state.get_ocr_words(&doc_id, page_num) {
                if let Some(ref re) = regex_pattern {
                    // Regex mode: reconstruct page text with per-word byte
                    // offsets so patterns spanning multiple tokens (e.g.
                    // `Test\s+Fixture`) can match across word boundaries.
                    //
                    // Words are laid out in visual lines separated by `\n`, not
                    // strung together with spaces: without the line breaks a
                    // scanned page is one endless line, and `^`/`$` would mean
                    // something different here than on a page with a native
                    // text layer. Same grouping the text overlay uses, so the
                    // lines agree with what the user sees.
                    let mut page_text = String::new();
                    let mut spans: Vec<(usize, usize, &crate::commands::ocr::OcrWord)> = Vec::new();
                    for (i, line) in ocr_words_to_line_groups(&words).into_iter().enumerate() {
                        if i > 0 {
                            page_text.push('\n');
                        }
                        for (j, word) in line.into_iter().enumerate() {
                            if j > 0 {
                                page_text.push(' ');
                            }
                            let start = page_text.len();
                            page_text.push_str(&word.text);
                            spans.push((start, page_text.len(), word));
                        }
                    }
                    // One regex match spanning several word tokens is one
                    // match, holding that run of word boxes.
                    for mat in re.find_iter(&page_text) {
                        let (ms, me) = (mat.start(), mat.end());
                        let rects: Vec<TextRect> = spans
                            .iter()
                            .filter(|&&(ws, we, _)| ws < me && we > ms)
                            .map(|&(_, _, word)| ocr_word_rect(word, page_height))
                            .collect();
                        if !rects.is_empty() {
                            page_matches.push(SearchMatch {
                                rects: merge_line_runs(rects),
                            });
                        }
                    }
                } else {
                    // Non-regex mode: test each word token.
                    // Compute needle once outside the loop to avoid a heap
                    // allocation per word for the lowercased query string.
                    let needle = if match_case {
                        query.clone()
                    } else {
                        query.to_lowercase()
                    };
                    for word in &words {
                        let matches = if match_case {
                            if whole_word {
                                word.text == needle
                            } else {
                                word.text.contains(&needle)
                            }
                        } else {
                            let haystack = word.text.to_lowercase();
                            if whole_word {
                                haystack == needle
                            } else {
                                haystack.contains(&needle)
                            }
                        };
                        if matches {
                            // One matching token is one match, one rect.
                            page_matches.push(SearchMatch {
                                rects: vec![ocr_word_rect(word, page_height)],
                            });
                        }
                    }
                }
            }
        }

        if !page_matches.is_empty() {
            results.push(SearchResult {
                page: page_num,
                matches: page_matches,
            });
        }
    }

    Ok(results)
}

/// Turns one search hit's segments into a [`SearchMatch`], converting each rect
/// from PDF user space (origin bottom-left) into the top-left origin the UI
/// uses, and collapsing per-glyph rects into one box per line.
///
/// Returns `None` for a hit that yields no readable rects, so it can be fed
/// straight to `extend`.
fn segments_to_match(
    segments: &PdfPageTextSegments,
    page_height: f32,
    origin_x: f32,
    origin_y: f32,
) -> Option<SearchMatch> {
    let rects: Vec<TextRect> = (0..segments.len())
        .filter_map(|i| segments.get(i).ok())
        .map(|segment| {
            let bounds = segment.bounds();
            TextRect {
                x: bounds.left().value - origin_x,
                y: page_height - (bounds.top().value - origin_y),
                width: bounds.right().value - bounds.left().value,
                height: bounds.top().value - bounds.bottom().value,
            }
        })
        .collect();

    (!rects.is_empty()).then(|| SearchMatch {
        rects: merge_line_runs(rects),
    })
}

/// Converts a cached OCR word's box (PDF user space, origin bottom-left) into
/// the top-left origin used by search rects.
fn ocr_word_rect(word: &crate::commands::ocr::OcrWord, page_height: f32) -> TextRect {
    TextRect {
        x: word.rect.x,
        y: page_height - (word.rect.y + word.rect.height),
        width: word.rect.width,
        height: word.rect.height,
    }
}

/// Counts pages that would still need OCR: no native text layer **and** no
/// recognized words already in the OCR cache. Drives the frontend's "run OCR on
/// export?" confirmation — so once a page has been made searchable, it no
/// longer triggers the prompt.
#[tauri::command]
pub async fn count_pages_without_text(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<u32, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let cache = state.ocr_cache_handle();
    tauri::async_runtime::spawn_blocking(move || count_pages_without_text_impl(entry, doc_id, cache))
        .await
        .map_err(|e| e.to_string())?
        .map_err(String::from)
}

fn count_pages_without_text_impl(
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    cache: OcrCache,
) -> Result<u32, AppError> {
    let entry = lock_mutex(&entry)?;
    let page_count = entry.document.pages().len();
    let mut count = 0;
    for i in 0..page_count {
        let page_num = (i + 1) as u32;
        let page = entry
            .document
            .pages()
            .get(i)
            .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
        let content = page
            .text()
            .map(|t| page_text_in_document_order(&t))
            .unwrap_or_default();
        // A page already OCR'd (cached) is "covered" even though its native
        // text layer is still empty.
        if content.trim().is_empty() && cache_get(&cache, &doc_id, page_num).is_none() {
            count += 1;
        }
    }
    Ok(count)
}

#[tauri::command]
pub async fn export_text(
    window: WebviewWindow,
    state: State<'_, AppState>,
    doc_id: String,
    dest_path: String,
    use_ocr: bool,
) -> Result<TextExportResult, String> {
    let entry = state.get_document(&doc_id).map_err(String::from)?;
    let engine = state.ocr_engine.clone();
    let cache = state.ocr_cache_handle();
    let cancel = Arc::new(AtomicBool::new(false));
    state.set_ocr_job(cancel.clone());

    // Forward per-page progress to the frontend on the shared `ocr-progress`
    // channel; the impl stays WebviewWindow-free so it's unit-testable with a
    // no-op closure.
    let emit = move |page, total| {
        let _ = window.emit("ocr-progress", OcrProgress { page, total });
    };

    let result = tauri::async_runtime::spawn_blocking(move || {
        export_text_impl(emit, entry, doc_id, dest_path, use_ocr, engine, cache, cancel)
    })
    .await
    .map_err(|e| e.to_string());

    state.take_ocr_job();
    result?.map_err(String::from)
}

#[allow(clippy::too_many_arguments)]
fn export_text_impl(
    emit_progress: impl Fn(u32, u32),
    entry: Arc<Mutex<DocEntry>>,
    doc_id: String,
    dest_path: String,
    use_ocr: bool,
    engine: Arc<dyn OcrEngine>,
    cache: OcrCache,
    cancel: Arc<AtomicBool>,
) -> Result<TextExportResult, AppError> {
    let page_count = lock_mutex(&entry)?.document.pages().len() as u32;

    let mut output = String::new();
    let mut ocr_pages = 0u32;
    let mut processed = 0u32;

    for i in 0..page_count {
        let page_num = i + 1;

        if cancel.load(Ordering::Relaxed) {
            return Ok(TextExportResult {
                pages: processed,
                ocr_pages,
                cancelled: true,
            });
        }
        emit_progress(page_num, page_count);

        // Read native text under a short-lived lock.
        let native = {
            let entry = lock_mutex(&entry)?;
            let page = entry
                .document
                .pages()
                .get(i as i32)
                .map_err(|e| AppError::pdfium(format!("Failed to get page {page_num}"), e))?;
            page.text()
                .map(|t| page_text_in_document_order(&t))
                .unwrap_or_default()
        }; // lock released here

        let page_text = if !native.trim().is_empty() {
            native
        } else {
            // No native text. Always use cached OCR if present (e.g. from
            // "Make Searchable" or a prior search); only run *new* OCR when the
            // user opted in. `ocr_page_into_cache` does the cache-or-recognize
            // step off the doc lock.
            let words = if use_ocr {
                Some(ocr_page_into_cache(&entry, &doc_id, page_num, &engine, &cache)?)
            } else {
                cache_get(&cache, &doc_id, page_num)
            };
            match words {
                Some(words) => {
                    let text = ocr_words_to_text(&words);
                    if !text.trim().is_empty() {
                        ocr_pages += 1;
                    }
                    text
                }
                None => String::new(),
            }
        };

        if page_num > 1 {
            output.push_str("\n\n");
        }
        output.push_str(&format!("--- Page {page_num} ---\n"));
        if page_text.trim().is_empty() {
            output.push_str("[no extractable text]");
        } else {
            output.push_str(&page_text);
        }
        processed = page_num;
    }

    // Write via a temp file then atomic rename so a disk-full or crash does
    // not truncate an existing file at dest_path.
    let tmp_path = format!("{dest_path}.tmp");
    std::fs::write(&tmp_path, output.as_bytes())
        .map_err(|e| AppError::io(format!("Failed to write to {tmp_path}"), e))?;
    std::fs::rename(&tmp_path, &dest_path)
        .map_err(|e| AppError::io(format!("Failed to rename {tmp_path} to {dest_path}"), e))?;

    Ok(TextExportResult {
        pages: page_count,
        ocr_pages,
        cancelled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ocr::OcrWord;
    use crate::state::DocEntry;

    /// Loads the checked-in fixture into `state` under `doc_id`.
    fn open_fixture(state: &AppState, doc_id: &str) {
        let src = crate::fixture_path();
                let entry = DocEntry::load(state.pdfium, &src.to_string_lossy(), None).expect("load pdf");
        state.insert_document(doc_id.to_string(), entry).expect("insert");
    }

    /// `sample.pdf` is a single 200x200 page containing the text "Test
    /// Fixture" in one run at 24pt, starting near the top-left of the page.
    /// This pins both the run-grouping logic and the coordinate conversion
    /// (PDF bottom-left origin -> top-left origin used by the UI).
    #[test]
    fn extract_page_text_returns_single_run_with_position() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let items = extract_page_text_impl(&state, "doc1".to_string(), 1).expect("extract");

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.text, "Test Fixture");
        assert_eq!(item.font_size, 24.0);
        assert!((item.x - 20.0).abs() < 0.5, "unexpected x: {}", item.x);
        assert!((item.y - 78.28).abs() < 0.5, "unexpected y: {}", item.y);
        assert!(item.width > 100.0, "unexpected width: {}", item.width);
        assert!(item.height > 0.0, "unexpected height: {}", item.height);
    }

    /// `page_text_in_document_order` (the `.all()` replacement introduced for
    /// issue #80) must return a page's text by walking characters in document
    /// order. On the simple fixture this equals the run text; the point of the
    /// helper is that it never routes through `FPDFText_GetBoundedText`, whose
    /// geometric reading-order reconstruction corrupts rotated / multi-column
    /// layouts. (The document that reproduces that corruption contains private
    /// data and cannot be checked in; this pins the helper's basic contract.)
    #[test]
    fn page_text_in_document_order_reads_fixture_text() {
        let pdfium = crate::test_pdfium();
        let src = crate::fixture_path();
        let entry = DocEntry::load(pdfium.get(), &src.to_string_lossy(), None).expect("load pdf");
        let page = entry.document.pages().get(0).expect("page 1");
        let text = page.text().expect("text");

        assert_eq!(page_text_in_document_order(&text), "Test Fixture");
    }

    #[test]
    fn extract_page_text_for_missing_page_is_error() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        match extract_page_text_impl(&state, "doc1".to_string(), 99) {
            Err(AppError::Pdfium { .. }) => {}
            Err(other) => panic!("expected AppError::Pdfium, got {other:?}"),
            Ok(_) => panic!("expected an error for an out-of-range page"),
        }
    }

    /// Searching for a word that appears on the page returns one rect with a
    /// sensible size, using the same coordinate conversion as
    /// `extract_page_text`.
    #[test]
    fn search_document_finds_known_word_with_nonempty_rect() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Test".to_string(),
            false,
            false,
            false,
        )
        .expect("search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page, 1);
        assert_eq!(results[0].matches.len(), 1);
        assert_eq!(results[0].matches[0].rects.len(), 1);

        let rect = &results[0].matches[0].rects[0];
        assert!(rect.width > 0.0, "unexpected width: {}", rect.width);
        assert!(rect.height > 0.0, "unexpected height: {}", rect.height);
        assert!(rect.x >= 0.0 && rect.y >= 0.0);
    }

    #[test]
    fn search_document_returns_empty_for_word_not_present() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Nonexistent".to_string(),
            false,
            false,
            false,
        )
        .expect("search");

        assert!(results.is_empty());
    }

    #[test]
    fn search_document_returns_empty_for_empty_query() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            String::new(),
            false,
            false,
            false,
        )
        .expect("search");

        assert!(results.is_empty());
    }

    /// The heart of issue #97: normally a native text layer wins and the OCR
    /// cache is only a fallback. On a force-re-OCR'd page that rule inverts —
    /// otherwise the junk layer the user rejected would keep being served and
    /// forcing would change nothing they can see.
    #[test]
    fn extract_page_text_prefers_ocr_words_over_native_text_when_forced() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        // Stand-in for a junk layer's replacement: the fixture's page has real
        // native text ("Test Fixture"), and OCR disagrees with it.
        state.set_ocr_words("doc1", 1, vec![ocr_word("Scanned")]);

        // Without an override the native layer still wins.
        let items = extract_page_text_impl(&state, "doc1".to_string(), 1).expect("extract");
        let text: String = items.iter().map(|i| i.text.as_str()).collect();
        assert!(text.contains("Test Fixture"), "native text should win: {text}");

        // Forced: the recognized words are served instead.
        state.set_ocr_override("doc1", 1);
        let items = extract_page_text_impl(&state, "doc1".to_string(), 1).expect("extract");
        let text: String = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(text, "Scanned", "forced page must serve OCR words");
        assert!(!text.contains("Test Fixture"), "junk layer leaked through");
    }

    /// Search must follow the same override, or a forced page would highlight
    /// hits from the very layer the user rejected.
    #[test]
    fn search_document_uses_ocr_words_over_native_text_when_forced() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");
        state.set_ocr_words("doc1", 1, vec![ocr_word("Scanned")]);

        let find = |query: &str| {
            search_document_impl(
                &state,
                "doc1".to_string(),
                query.to_string(),
                false, // match_case
                false, // whole_word
                false, // use_regex
            )
            .expect("search")
            .len()
        };

        // Unforced, the native layer is searched. (Cached words stay reachable
        // too — the per-page fallback fires whenever the native search misses,
        // which predates this feature and is harmless.)
        assert_eq!(find("Fixture"), 1);

        state.set_ocr_override("doc1", 1);

        // Forced: the native layer is out of the picture, and only the
        // recognized words answer.
        assert_eq!(find("Fixture"), 0, "junk layer still searchable after force");
        assert_eq!(find("Scanned"), 1, "OCR words not searchable after force");
    }

    fn ocr_word(text: &str) -> OcrWord {
        ocr_word_at(text, 10.0)
    }

    /// An OCR word at a given left edge, so tests can place two words side by
    /// side on one line. Rect is in PDF user space (origin bottom-left), as the
    /// cache stores it.
    fn ocr_word_at(text: &str, x: f32) -> OcrWord {
        ocr_word_xy(text, x, 150.0)
    }

    /// An OCR word at an explicit position, so a test can lay out more than one
    /// visual line. PDF user space, origin bottom-left: a larger `y` is higher.
    fn ocr_word_xy(text: &str, x: f32, y: f32) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: TextRect {
                x,
                y,
                width: 40.0,
                height: 12.0,
            },
        }
    }

    /// When a page has no native hits for the query, `search_document` falls
    /// back to the OCR cache. Searching the text-only fixture for a word that
    /// isn't in its text layer ("Banana") returns nothing natively, but a
    /// cached OCR word for that page makes it a hit.
    #[test]
    fn search_document_falls_back_to_ocr_cache() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");
        state.set_ocr_words("doc1", 1, vec![ocr_word("Banana")]);

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "banana".to_string(),
            false,
            false,
            false,
        )
        .expect("search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].page, 1);
        assert_eq!(results[0].matches.len(), 1);
        // y is flipped from the bottom-left cache rect into top-left space.
        let rect = &results[0].matches[0].rects[0];
        assert!((rect.y - (200.0 - (150.0 + 12.0))).abs() < 0.1, "y: {}", rect.y);
    }

    /// A blank page (no text layer) returns no native text, so
    /// `extract_page_text` falls back to the cached OCR words.
    #[test]
    fn extract_page_text_falls_back_to_ocr_cache_on_blank_page() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);

        let mut doc = pdfium.get().create_new_pdf().expect("create pdf");
        doc.pages_mut()
            .create_page_at_index(
                PdfPagePaperSize::new_custom(PdfPoints::new(200.0), PdfPoints::new(200.0)),
                0 as PdfPageIndex,
            )
            .expect("create blank page");
        state
            .insert_document(
                "blank".to_string(),
                DocEntry {
                    page_cache: Vec::new(),
                    document: doc,
                    file_path: "blank.pdf".to_string(),
                    // No backing file; these tests never touch the buffer.
                    buffer: Vec::new(),
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");

        // Without OCR, a blank page extracts to nothing.
        let before = extract_page_text_impl(&state, "blank".to_string(), 1).expect("extract");
        assert!(before.is_empty(), "blank page should have no native text");

        state.set_ocr_words("blank", 1, vec![ocr_word("Scanned")]);

        let after = extract_page_text_impl(&state, "blank".to_string(), 1).expect("extract");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].text, "Scanned");
        // Flipped from bottom-left (y=150,h=12) into top-left space.
        assert!((after[0].y - (200.0 - (150.0 + 12.0))).abs() < 0.1, "y: {}", after[0].y);
    }

    /// Minimal OCR engine for export tests: returns fixed pixel-space words.
    struct FakeOcrEngine {
        words: Vec<OcrWord>,
    }
    impl OcrEngine for FakeOcrEngine {
        fn recognize(&self, _rgba: &[u8], _w: u32, _h: u32) -> Result<Vec<OcrWord>, AppError> {
            Ok(self.words.clone())
        }
    }

    fn no_progress(_page: u32, _total: u32) {}

    /// Inserts an `n`-page blank document (no text layer) under `doc_id`.
    fn open_blank_doc(state: &AppState, doc_id: &str, pages: u32) {
                let mut doc = state.pdfium.create_new_pdf().expect("create pdf");
        for i in 0..pages {
            doc.pages_mut()
                .create_page_at_index(
                    PdfPagePaperSize::new_custom(PdfPoints::new(200.0), PdfPoints::new(200.0)),
                    i as PdfPageIndex,
                )
                .expect("create blank page");
        }
        state
            .insert_document(
                doc_id.to_string(),
                DocEntry {
                    page_cache: Vec::new(),
                    document: doc,
                    file_path: format!("{doc_id}.pdf"),
                    // No backing file; these tests never touch the buffer.
                    buffer: Vec::new(),
                    dirty: false,
                    protection: crate::state::Protection::Plaintext,
                    linearized: false,
                },
            )
            .expect("insert");
    }

    fn temp_txt(name: &str) -> String {
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn export_text_uses_native_text_without_ocr() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let entry = state.get_document("doc1").expect("get document");
        let dest = temp_txt("tumbler_export_native.txt");
        let cancel = Arc::new(AtomicBool::new(false));

        let result = export_text_impl(
            no_progress,
            entry,
            "doc1".to_string(),
            dest.clone(),
            false,
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            cancel,
        )
        .expect("export");

        assert_eq!(result.pages, 1);
        assert_eq!(result.ocr_pages, 0);
        assert!(!result.cancelled);

        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("--- Page 1 ---"), "missing page separator");
        assert!(content.contains("Test Fixture"), "missing native text");
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_text_ocr_fills_blank_page_and_populates_cache() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_blank_doc(&state, "blank", 1);

        let entry = state.get_document("blank").expect("get document");
        let cache = state.ocr_cache_handle();
        let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcrEngine {
            words: vec![ocr_word("Scanned")],
        });
        let dest = temp_txt("tumbler_export_ocr.txt");
        let cancel = Arc::new(AtomicBool::new(false));

        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            true,
            engine,
            cache.clone(),
            cancel,
        )
        .expect("export");

        assert_eq!(result.pages, 1);
        assert_eq!(result.ocr_pages, 1);

        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("Scanned"), "OCR text missing: {content}");
        assert!(
            !content.contains("[no extractable text]"),
            "should not show placeholder when OCR found text"
        );
        // Export also primed the cache so search/copy now work for this page.
        assert!(cache_get(&cache, "blank", 1).is_some(), "cache not populated");
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_text_without_ocr_keeps_placeholder_on_blank_page() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_blank_doc(&state, "blank", 1);

        let entry = state.get_document("blank").expect("get document");
        let dest = temp_txt("tumbler_export_no_ocr.txt");
        let cancel = Arc::new(AtomicBool::new(false));

        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            false,
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            cancel,
        )
        .expect("export");

        assert_eq!(result.ocr_pages, 0);
        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("[no extractable text]"), "missing placeholder");
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn export_text_cancellation_stops_before_writing() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_blank_doc(&state, "blank", 3);

        let entry = state.get_document("blank").expect("get document");
        let dest = temp_txt("tumbler_export_cancel.txt");
        std::fs::remove_file(&dest).ok();
        // Pre-set the cancel token so the very first page check fires.
        let cancel = Arc::new(AtomicBool::new(true));

        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            false,
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            cancel,
        )
        .expect("export");

        assert!(result.cancelled, "expected cancelled result");
        assert!(
            !std::path::Path::new(&dest).exists(),
            "cancelled export must not write a file"
        );
    }

    /// A blank page exported with `use_ocr=false` still uses cached OCR words
    /// (e.g. from a prior "Make Searchable") rather than re-OCRing or writing a
    /// placeholder. This is what lets the Export prompt be skipped afterward.
    #[test]
    fn export_text_uses_cached_ocr_even_without_use_ocr() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_blank_doc(&state, "blank", 1);
        // Simulate a prior Make Searchable having cached this page.
        state.set_ocr_words("blank", 1, vec![ocr_word("Scanned")]);

        let entry = state.get_document("blank").expect("get document");
        let dest = temp_txt("tumbler_export_cached.txt");
        let result = export_text_impl(
            no_progress,
            entry,
            "blank".to_string(),
            dest.clone(),
            false, // use_ocr = false: must still pick up the cache
            state.ocr_engine.clone(),
            state.ocr_cache_handle(),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("export");

        assert_eq!(result.ocr_pages, 1);
        let content = std::fs::read_to_string(&dest).expect("read output");
        assert!(content.contains("Scanned"), "cached OCR text missing: {content}");
        assert!(!content.contains("[no extractable text]"));
        std::fs::remove_file(&dest).ok();
    }

    // ── Search mode flag tests (issue #6) ──────────────────────────────────
    // These tests call `search_document_impl` with the new `match_case`,
    // `whole_word`, and `use_regex` parameters that will be added by the
    // implementation.  They will not compile until those parameters exist —
    // that is intentional (TDD red phase).

    /// Default (case-insensitive) search finds "Test Fixture" via lowercase query.
    #[test]
    fn test_search_case_insensitive_default() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "test fixture".to_string(),
            false, // match_case
            false, // whole_word
            false, // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "expected one page of results");
    }

    /// With match_case=true the lowercase query must not match "Test Fixture".
    #[test]
    fn test_search_match_case_rejects_wrong_case() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "test fixture".to_string(),
            true,  // match_case
            false, // whole_word
            false, // use_regex
        )
        .expect("search");

        assert!(
            results.is_empty(),
            "case-sensitive search should find no results for wrong case"
        );
    }

    /// With match_case=true the correctly-cased query must match.
    #[test]
    fn test_search_match_case_accepts_right_case() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Test Fixture".to_string(),
            true,  // match_case
            false, // whole_word
            false, // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "expected one page of results");
    }

    /// With whole_word=true a prefix substring of a word must not match.
    #[test]
    fn test_search_whole_word_rejects_substring() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "Te".to_string(),
            false, // match_case
            true,  // whole_word
            false, // use_regex
        )
        .expect("search");

        assert!(
            results.is_empty(),
            "whole-word search should not match a substring"
        );
    }

    /// A regex pattern matching the fixture text must return one result.
    #[test]
    fn test_search_regex_finds_pattern() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            r"Test\s+Fixture".to_string(),
            false, // match_case
            false, // whole_word
            true,  // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "regex should match the fixture text");
    }

    /// An invalid regex pattern must return Err rather than panic or silently
    /// returning empty results.
    #[test]
    fn test_search_invalid_regex_returns_err() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        let result = search_document_impl(
            &state,
            "doc1".to_string(),
            "[invalid".to_string(),
            false, // match_case
            false, // whole_word
            true,  // use_regex
        );

        assert!(result.is_err(), "invalid regex should return an error");
    }

    /// Each regex match is one result, and repeated text does not multiply.
    ///
    /// This used to pin a deduplication step: the old implementation re-searched
    /// the page for each matched *string*, which returned that string's
    /// page-wide occurrences once per match, so two 'e's produced four rects
    /// unless the strings were deduplicated first. Matches now come from their
    /// own character offsets, so there is nothing to deduplicate — but the
    /// count it asserts is still exactly right, and still worth holding.
    #[test]
    fn test_search_regex_one_result_per_match() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        // "e" (case-sensitive) matches twice in "Test Fixture" (positions 1 and 11).
        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            "e".to_string(),
            false, // match_case — regex itself is case-sensitive by default
            false, // whole_word
            true,  // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "should find matches on page 1");
        assert_eq!(
            results[0].matches.len(),
            2,
            "exactly 2 matches expected, one per 'e'; got {}",
            results[0].matches.len()
        );
    }

    /// OCR regex fallback reconstructs page text from word tokens so patterns
    /// spanning multiple words (e.g. `Hello\s+World`) match correctly.
    #[test]
    fn test_search_ocr_regex_matches_across_word_tokens() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");
        // "Hello" and "World" are not in the native fixture text, so pdfium
        // returns no hits and the OCR fallback runs.
        // Side by side on one line, so the merged rect must span both.
        state.set_ocr_words(
            "doc1",
            1,
            vec![ocr_word_at("Hello", 10.0), ocr_word_at("World", 60.0)],
        );

        let results = search_document_impl(
            &state,
            "doc1".to_string(),
            r"Hello\s+World".to_string(),
            false, // match_case
            false, // whole_word
            true,  // use_regex
        )
        .expect("search");

        assert_eq!(results.len(), 1, "regex should find the cross-word OCR match");
        assert_eq!(
            results[0].matches.len(),
            1,
            "one pattern occurrence is one match, not one per word token"
        );
        // Both word boxes ride along inside that single match, merged into one
        // box because they share a line.
        let rects = &results[0].matches[0].rects;
        assert_eq!(rects.len(), 1, "same-line word boxes should merge");
        assert!((rects[0].x - 10.0).abs() < 0.1, "x: {}", rects[0].x);
        assert!(
            (rects[0].width - 90.0).abs() < 0.1,
            "merged box must span both words, width: {}",
            rects[0].width
        );
    }

    /// OCR whole-word mode uses token equality (not split_whitespace) so a
    /// query does not accidentally match a longer token that merely starts
    /// with the same characters.
    #[test]
    fn test_search_ocr_whole_word_matches_exact_token() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "doc1");

        // Use words that are absent from the pdfium fixture ("Test Fixture")
        // so pdfium returns no hits and the OCR fallback runs.
        // "Bananasplit" is not in the fixture; querying "Banana" must NOT
        // match it under whole_word=true.
        state.set_ocr_words("doc1", 1, vec![ocr_word("Bananasplit")]);

        let no_match = search_document_impl(
            &state,
            "doc1".to_string(),
            "Banana".to_string(),
            false, // match_case
            true,  // whole_word
            false, // use_regex
        )
        .expect("search");
        assert!(
            no_match.is_empty(),
            "whole_word should not match a prefix of a longer token"
        );

        // Swap in the exact token — now it must match.
        state.set_ocr_words("doc1", 1, vec![ocr_word("Banana")]);

        let matched = search_document_impl(
            &state,
            "doc1".to_string(),
            "Banana".to_string(),
            false, // match_case
            true,  // whole_word
            false, // use_regex
        )
        .expect("search");
        assert_eq!(matched.len(), 1, "whole_word should match the exact token");
    }

    // ── Per-glyph text objects (the character-by-character search bug) ─────

    fn rect(x: f32, y: f32, width: f32, height: f32) -> TextRect {
        TextRect { x, y, width, height }
    }

    /// The measured shape of the bug: eight per-character boxes from one word,
    /// with the tight-box variation a real document shows — a capital sits
    /// higher and taller than the lowercase letters beside it. All eight are
    /// one line and must collapse to one box spanning them.
    #[test]
    fn merge_line_runs_collapses_per_character_boxes() {
        let chars = vec![
            rect(45.20, 30.81, 7.12, 8.19),
            rect(53.28, 32.78, 5.03, 6.22),
            rect(59.84, 30.33, 5.30, 8.78),
            rect(66.74, 32.78, 5.10, 6.32),
            rect(73.38, 32.78, 3.47, 6.22),
            rect(77.58, 32.78, 4.28, 6.32),
            rect(82.98, 32.78, 5.54, 6.32),
            rect(90.10, 32.78, 5.03, 6.22),
        ];

        let merged = merge_line_runs(chars);

        assert_eq!(merged.len(), 1, "one word on one line is one box");
        assert!((merged[0].x - 45.20).abs() < 0.01, "x: {}", merged[0].x);
        // Spans from the first glyph's left edge to the last glyph's right.
        assert!(
            (merged[0].width - (95.13 - 45.20)).abs() < 0.01,
            "width: {}",
            merged[0].width
        );
        // Tallest glyph's extent, top ('n' at 30.33) to bottom (39.11).
        assert!((merged[0].y - 30.33).abs() < 0.01, "y: {}", merged[0].y);
    }

    /// A match broken by a line break stays two boxes — it is one result, but
    /// it genuinely occupies two places on the page.
    #[test]
    fn merge_line_runs_keeps_one_box_per_line() {
        let merged = merge_line_runs(vec![
            rect(150.0, 100.0, 10.0, 12.0),
            rect(160.0, 100.0, 10.0, 12.0),
            rect(20.0, 130.0, 10.0, 12.0),
            rect(30.0, 130.0, 10.0, 12.0),
        ]);

        assert_eq!(merged.len(), 2);
        // Sorted into reading order: the earlier line first.
        assert!((merged[0].y - 100.0).abs() < 0.01);
        assert!((merged[0].x - 150.0).abs() < 0.01);
        assert!((merged[0].width - 20.0).abs() < 0.01);
        assert!((merged[1].y - 130.0).abs() < 0.01);
        assert!((merged[1].x - 20.0).abs() < 0.01);
    }

    #[test]
    fn merge_line_runs_on_empty_input_is_empty() {
        assert!(merge_line_runs(Vec::new()).is_empty());
    }

    /// Writes a one-page PDF whose glyphs are each their own show-text
    /// operator — `Td … Tj` per character inside one `BT … ET`, the shape
    /// emitted by the PDF generator that surfaced this bug. pdfium builds one
    /// text object per `Tj`, and `FPDFText_CountRects` starts a new rect at
    /// every text-object change, so a search for `word` yields one rect per
    /// character. `tests/fixtures/sample.pdf` cannot reproduce this: its text
    /// is a single object, which pdfium merges into one rect on its own.
    fn write_per_glyph_pdf(path: &str, word: &str) {
        use lopdf::{dictionary, Dictionary, Document, Object, Stream};

        // Advance by the previous glyph's real width in 12pt Helvetica.
        // Uniform spacing leaves a gap after a narrow letter, and pdfium reads
        // a wide enough gap as a word break — which would split the word in the
        // extracted text and defeat the point of the fixture.
        let advance = |ch: char| -> f32 {
            let per_1000 = match ch {
                'M' => 833.0,
                'r' => 333.0,
                's' => 500.0,
                _ => 556.0, // e, a, n, d
            };
            per_1000 * 12.0 / 1000.0
        };

        let mut content = String::from("BT\n/F1 12 Tf\n20 100 Td\n");
        let mut prev: Option<char> = None;
        for ch in word.chars() {
            // Each glyph advances from the previous one, exactly as the real
            // document does; only the first is placed absolutely.
            if let Some(prev) = prev {
                content.push_str(&format!("{:.4} 0 Td ", advance(prev)));
            }
            content.push_str(&format!("({ch}) Tj\n"));
            prev = Some(ch);
        }
        content.push_str("ET\n");

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let contents_id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents_id,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            },
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(300), Object::Integer(200),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).expect("write per-glyph pdf");
    }

    /// The regression this whole change exists for: on a per-glyph document,
    /// one word is **one** match with **one** highlight box — not eight of
    /// each, which is what made search step through a phrase letter by letter.
    #[test]
    fn search_on_per_glyph_document_is_one_match_with_one_rect() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);

        let path = temp_txt("tumbler_per_glyph.pdf");
        write_per_glyph_pdf(&path, "Meanders");
        let entry = DocEntry::load(state.pdfium, &path, None).expect("load");
        state.insert_document("glyphs".to_string(), entry).expect("insert");

        // Pin the premise: pdfium really does hand back one rect per character
        // here. If a future pdfium merges them itself this assertion moves,
        // and the reason for the merge below is on record.
        {
            let entry = state.get_document("glyphs").expect("get");
            let entry = lock_mutex(&entry).expect("lock");
            let page = entry.document.pages().get(0).expect("page");
            let text = page.text().expect("text");
            let search = text
                .search("Meanders", &PdfSearchOptions::new())
                .expect("search");
            let raw: Vec<usize> = search
                .iter(PdfSearchDirection::SearchForward)
                .map(|segments| segments.len())
                .collect();
            assert_eq!(
                page_text_in_document_order(&text),
                "Meanders",
                "glyph advances must not read as a word break"
            );
            assert_eq!(raw, vec![8], "expected pdfium to split per character");
        }

        let results = search_document_impl(
            &state,
            "glyphs".to_string(),
            "Meanders".to_string(),
            false,
            false,
            false,
        )
        .expect("search");

        assert_eq!(results.len(), 1, "one page of results");
        assert_eq!(results[0].matches.len(), 1, "one occurrence is one match");
        let rects = &results[0].matches[0].rects;
        assert_eq!(rects.len(), 1, "one line is one highlight box");
        // The box spans the whole word, not a single glyph: "Meanders" in
        // 12pt Helvetica is ~53pt wide.
        assert!(
            rects[0].width > 40.0,
            "highlight must cover the word, width: {}",
            rects[0].width
        );

        std::fs::remove_file(&path).ok();
    }

    // ── Regex line anchors (issue #113) ────────────────────────────────────

    /// Writes a one-page PDF with each string on its own visual line. pdfium
    /// extracts these separated by `\r\n`, which is the whole reason `$` needs
    /// CRLF mode.
    fn write_lines_pdf(path: &str, lines: &[&str]) {
        use lopdf::{dictionary, Dictionary, Document, Object, Stream};

        let mut content = String::from("BT\n/F1 12 Tf\n20 150 Td\n");
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                content.push_str("0 -20 Td ");
            }
            content.push_str(&format!("({line}) Tj\n"));
        }
        content.push_str("ET\n");

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let contents_id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents_id,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
            },
            "MediaBox" => vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(300), Object::Integer(200),
            ],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).expect("write lines pdf");
    }

    /// Opens a two-line page reading "Total 5" / "Subtotal Total" under
    /// `doc_id`, and returns its temp path for cleanup.
    fn open_two_line_doc(state: &AppState, doc_id: &str) -> String {
        let path = temp_txt(&format!("tumbler_lines_{doc_id}.pdf"));
        write_lines_pdf(&path, &["Total 5", "Subtotal Total"]);
        let entry = DocEntry::load(state.pdfium, &path, None).expect("load");
        state.insert_document(doc_id.to_string(), entry).expect("insert");
        path
    }

    fn regex_matches(state: &AppState, doc_id: &str, pattern: &str) -> Vec<SearchResult> {
        search_document_impl(
            state,
            doc_id.to_string(),
            pattern.to_string(),
            false, // match_case
            false, // whole_word
            true,  // use_regex
        )
        .expect("search")
    }

    fn match_count(results: &[SearchResult]) -> usize {
        results.iter().map(|r| r.matches.len()).sum()
    }

    /// `^` and `$` mean start and end of a *line*, with no flags typed. The
    /// user should not have to know that a page is matched as one long string.
    #[test]
    fn regex_anchors_are_line_anchors_by_default() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        let path = open_two_line_doc(&state, "lines");

        // Sanity: the page really is two CRLF-separated lines.
        {
            let entry = state.get_document("lines").expect("get");
            let entry = lock_mutex(&entry).expect("lock");
            let page = entry.document.pages().get(0).expect("page");
            assert_eq!(
                page_text_in_document_order(&page.text().expect("text")),
                "Total 5\r\nSubtotal Total"
            );
        }

        // Unanchored: both capital-T "Total"s ("Subtotal" is lowercase inside).
        assert_eq!(match_count(&regex_matches(&state, "lines", "Total")), 2);

        // Line start: only line 1 begins with it.
        assert_eq!(
            match_count(&regex_matches(&state, "lines", "^Total")),
            1,
            "^ should anchor to a line, and select only that occurrence"
        );

        // Line end: only line 2 ends with it. Without CRLF mode the `\r` would
        // sit between "Total" and the line break and this would find nothing.
        assert_eq!(
            match_count(&regex_matches(&state, "lines", "Total$")),
            1,
            "$ must see through the CRLF line ending"
        );

        // Whole line.
        assert_eq!(match_count(&regex_matches(&state, "lines", "^Total 5$")), 1);

        std::fs::remove_file(&path).ok();
    }

    /// The escape hatch: inline flags still beat the defaults, so anyone who
    /// wants the old whole-page anchoring can still ask for it.
    #[test]
    fn regex_inline_flag_restores_page_anchoring() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        let path = open_two_line_doc(&state, "lines");

        // Page-anchored: matches at the very start of the page text only.
        assert_eq!(match_count(&regex_matches(&state, "lines", "(?-m)^Total")), 1);
        // ...and nothing ends the *page* with "Total 5".
        assert!(regex_matches(&state, "lines", "(?-m)Total 5$").is_empty());

        std::fs::remove_file(&path).ok();
    }

    /// The regression for the heart of issue #113: an anchored pattern must
    /// select the occurrence it matched, not merely mark the page as having a
    /// hit. Before the fix `^Total` highlighted both "Total"s on this page,
    /// because the rectangles came from re-searching the page for the matched
    /// *string* rather than from the match's own offsets.
    #[test]
    fn regex_anchor_selects_only_the_matched_occurrence() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        let path = open_two_line_doc(&state, "lines");

        let results = regex_matches(&state, "lines", "^Total");
        assert_eq!(match_count(&results), 1);

        // It is line 1's occurrence, not line 2's. Line 1 sits higher on the
        // page, so in top-left coordinates it has the smaller y.
        let anchored = &results[0].matches[0].rects[0];
        let line2 = {
            let all = regex_matches(&state, "lines", "Total$");
            all[0].matches[0].rects[0].y
        };
        assert!(
            anchored.y < line2,
            "^Total selected the wrong line: y={} vs line 2 y={line2}",
            anchored.y
        );

        std::fs::remove_file(&path).ok();
    }

    /// Matches come back in reading order. The old implementation collected
    /// matched strings into a `HashSet`, so the order results arrived in was
    /// whatever the hasher produced — which the "next match" button walks.
    #[test]
    fn regex_matches_are_in_reading_order() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        let path = temp_txt("tumbler_order.pdf");
        write_lines_pdf(&path, &["alpha", "beta", "gamma", "delta"]);
        let entry = DocEntry::load(state.pdfium, &path, None).expect("load");
        state.insert_document("ord".to_string(), entry).expect("insert");

        let results = regex_matches(&state, "ord", "^[a-z]+$");
        assert_eq!(match_count(&results), 4);
        let ys: Vec<f32> = results[0].matches.iter().map(|m| m.rects[0].y).collect();
        assert!(
            ys.windows(2).all(|w| w[0] < w[1]),
            "matches out of reading order: {ys:?}"
        );

        std::fs::remove_file(&path).ok();
    }

    /// A page whose text drops a character pdfium cannot decode: the byte
    /// offsets the regex reports no longer line up with pdfium's character
    /// indices, and the highlight would land on the wrong glyphs.
    ///
    /// Authoring a PDF with an undecodable glyph is fiddly and fragile, so this
    /// pins the mapping directly. The layout below is what
    /// `page_text_with_char_index` produces for a page whose pdfium characters
    /// are `A`, <undecodable>, `B`, `C` — the text is "ABC", but `B` is pdfium
    /// character 2, not 1.
    #[test]
    fn page_text_char_index_survives_dropped_characters() {
        let page = PageText {
            text: "ABC".to_string(),
            offsets: vec![(0, 0), (1, 2), (2, 3)],
        };

        // "A" alone: pdfium character 0, one character.
        assert_eq!(page.pdfium_range(0, 1), Some((0, 1)));
        // "B" alone: character 2 — counting chars in "A" would have said 1.
        assert_eq!(page.pdfium_range(1, 2), Some((2, 1)));
        // "BC": characters 2..=3.
        assert_eq!(page.pdfium_range(1, 3), Some((2, 2)));
        // "ABC" spans 0..=3 — four characters, because the dropped one still
        // sits physically between A and B on the page.
        assert_eq!(page.pdfium_range(0, 3), Some((0, 4)));
        // An empty range selects nothing rather than panicking.
        assert_eq!(page.pdfium_range(1, 1), None);
    }

    /// Anchors mean the same thing on a scanned page as on a native one. The
    /// OCR fallback lays its words out in visual lines separated by `\n`; when
    /// it strung them together with spaces, a scan was one endless line and
    /// `^`/`$` silently degraded to page start/end.
    #[test]
    fn regex_anchors_work_on_ocr_pages() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_blank_doc(&state, "scan", 1);

        // Two visual lines: "Total 5" over "Subtotal Total".
        // Larger y is higher up the page, so line 1 is y=150.
        state.set_ocr_words(
            "scan",
            1,
            vec![
                ocr_word_xy("Total", 10.0, 150.0),
                ocr_word_xy("5", 60.0, 150.0),
                ocr_word_xy("Subtotal", 10.0, 120.0),
                ocr_word_xy("Total", 70.0, 120.0),
            ],
        );

        assert_eq!(match_count(&regex_matches(&state, "scan", "Total")), 2);
        assert_eq!(
            match_count(&regex_matches(&state, "scan", "^Total")),
            1,
            "^ must anchor to an OCR line, not to the whole page"
        );
        assert_eq!(
            match_count(&regex_matches(&state, "scan", "Total$")),
            1,
            "$ must anchor to an OCR line, not to the whole page"
        );
        // Cross-line patterns still work, since the lines are one string.
        assert_eq!(match_count(&regex_matches(&state, "scan", r"5\s+Subtotal")), 1);
    }

    #[test]
    fn count_pages_without_text_counts_only_uncovered_pages() {
        let pdfium = crate::test_pdfium();
        let state = AppState::new(pdfium.get(), None);
        open_fixture(&state, "text");
        open_blank_doc(&state, "blank", 1);

        let text_doc = state.get_document("text").expect("get text doc");
        let blank_doc = state.get_document("blank").expect("get blank doc");

        // Native text → 0; blank, uncached → 1.
        assert_eq!(
            count_pages_without_text_impl(text_doc, "text".to_string(), state.ocr_cache_handle())
                .expect("count"),
            0
        );
        assert_eq!(
            count_pages_without_text_impl(
                blank_doc.clone(),
                "blank".to_string(),
                state.ocr_cache_handle()
            )
            .expect("count"),
            1
        );

        // Once the blank page is cached (Make Searchable), it's no longer counted.
        state.set_ocr_words("blank", 1, vec![ocr_word("Scanned")]);
        assert_eq!(
            count_pages_without_text_impl(blank_doc, "blank".to_string(), state.ocr_cache_handle())
                .expect("count"),
            0
        );
    }
}

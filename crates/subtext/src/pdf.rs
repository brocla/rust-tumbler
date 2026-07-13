//! lopdf traversal primitives shared by the structural extractors.
//!
//! The decode discipline (spec §4-L): every extractor matches against the
//! *decoded* value of a PDF string, never raw file bytes — so octal/hex escapes
//! and UTF-16BE text strings are transparent. These helpers centralize that
//! decoding and the reference/tree walking so no extractor re-implements it.

use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};

/// Decodes PDF text-string bytes to a Rust `String` per the PDF text-string
/// convention: a UTF-16 byte-order mark selects UTF-16 (BE **or** LE); a UTF-8
/// BOM selects UTF-8; otherwise the bytes are **PDFDocEncoding** (whose
/// 0x80–0xA0 range is typographic punctuation — smart quotes, dashes, bullet —
/// *not* Latin-1 C1 controls, so a raw `b as char` cast silently mangles them
/// and a query for a name like `O’Brien` would miss). We delegate the BE/UTF-8/
/// PDFDocEncoding cases to lopdf's table-correct `decode_text_string` (wrapping
/// the bytes in a literal `Object`) and handle only the UTF-16LE BOM ourselves,
/// which lopdf does not. Used for both string objects and raw stream/content
/// bytes; `Object::as_str` has already un-escaped any literal.
pub fn decode_pdf_text(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        // UTF-16LE with BOM (lopdf's decoder only recognizes the BE BOM).
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        // UTF-16BE BOM, UTF-8 BOM, and PDFDocEncoding are all handled correctly
        // by lopdf's table-driven decoder.
        let obj = Object::String(bytes.to_vec(), StringFormat::Literal);
        lopdf::decode_text_string(&obj)
            .unwrap_or_else(|_| bytes.iter().map(|&b| b as char).collect())
    }
}

/// Decodes arbitrary **stream / embedded-file** bytes to text for scanning.
///
/// Unlike [`decode_pdf_text`] — which assumes PDFDocEncoding for non-BOM bytes,
/// correct for PDF *text-string objects* — a stream's contents are an arbitrary
/// file (an attachment, a JavaScript source, a rich-text value), most often
/// UTF-8. So we prefer UTF-16 (either BOM), then UTF-8, and only fall back to
/// the PDF text-string decode. This keeps a UTF-8 embedded file's non-ASCII
/// secret matchable (a PDFDocEncoding-only decode would mangle `Zürich` into
/// `ZÃ¼rich`) while still catching a UTF-16 stream.
pub fn decode_stream_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) || bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_pdf_text(bytes); // UTF-16 (BE via lopdf, LE ourselves)
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string(); // valid UTF-8 (covers ASCII and UTF-8 files)
    }
    decode_pdf_text(bytes) // last resort: PDF text-string / PDFDocEncoding
}

/// The decoded text of an object *if* it is a string; `None` otherwise. Routes
/// through [`decode_pdf_text`] so string objects get the same BOM-aware,
/// PDFDocEncoding-correct decode as raw bytes.
pub fn string_text(obj: &Object) -> Option<String> {
    obj.as_str().ok().map(decode_pdf_text)
}

/// Follows a chain of indirect references to the concrete object, with a small
/// cycle guard. Returns `obj` unchanged when it is already direct.
pub fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Object> {
    let mut current = obj;
    for _ in 0..32 {
        match current {
            Object::Reference(id) => current = doc.get_object(*id).ok()?,
            other => return Some(other),
        }
    }
    None
}

/// Resolves `dict[key]` to a concrete object (dereferencing a reference value).
pub fn get<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    resolve(doc, dict.get(key).ok()?)
}

/// Resolves `dict[key]` to a dictionary (the value may be a dict or a reference
/// to one).
pub fn get_dict<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
    get(doc, dict, key)?.as_dict().ok()
}

/// Resolves `dict[key]` to an array.
pub fn get_array<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Vec<Object>> {
    get(doc, dict, key)?.as_array().ok()
}

/// Resolves `dict[key]` to a decoded string.
pub fn get_string(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<String> {
    string_text(get(doc, dict, key)?)
}

/// The catalog dictionary, or `None` on a malformed document.
pub fn catalog(doc: &Document) -> Option<&Dictionary> {
    doc.catalog().ok()
}

/// The decompressed bytes of a stream object, best-effort (returns raw content
/// if the filters cannot be decoded). `None` when `id` is not a stream.
pub fn stream_bytes(doc: &Document, id: ObjectId) -> Option<Vec<u8>> {
    let stream = doc.get_object(id).ok()?.as_stream().ok()?;
    Some(
        stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone()),
    )
}

/// The decompressed bytes of an already-resolved stream object (handles a
/// direct inline stream that has no `ObjectId`). `None` when `obj` is not a
/// stream.
pub fn stream_object_bytes(obj: &Object) -> Option<Vec<u8>> {
    let stream = obj.as_stream().ok()?;
    Some(
        stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone()),
    )
}

/// Walks a PDF **name tree** (root has `/Names [k v k v …]` at leaves or
/// `/Kids [refs]` at branches), invoking `visit(key, value)` for every entry.
/// `key` is decoded; `value` is the concrete (dereferenced) object. Depth- and
/// visit-guarded against malformed/cyclic trees.
pub fn walk_name_tree<'a>(
    doc: &'a Document,
    root: &'a Dictionary,
    mut visit: impl FnMut(String, &'a Object),
) {
    fn recurse<'a>(
        doc: &'a Document,
        node: &'a Dictionary,
        depth: u32,
        budget: &mut u32,
        visit: &mut impl FnMut(String, &'a Object),
    ) {
        if depth > 64 || *budget == 0 {
            return;
        }
        if let Some(names) = get_array(doc, node, b"Names") {
            // Alternating key/value pairs.
            for pair in names.chunks(2) {
                if *budget == 0 {
                    return;
                }
                if let [k, v] = pair {
                    if let (Some(key), Some(val)) = (string_text(k), resolve(doc, v)) {
                        *budget -= 1;
                        visit(key, val);
                    }
                }
            }
        }
        if let Some(kids) = get_array(doc, node, b"Kids") {
            for kid in kids {
                if let Some(kid_dict) = resolve(doc, kid).and_then(|o| o.as_dict().ok()) {
                    recurse(doc, kid_dict, depth + 1, budget, visit);
                }
            }
        }
    }
    let mut budget = 100_000u32;
    recurse(doc, root, 0, &mut budget, &mut visit);
}

/// Walks a PDF **number tree** (`/Nums [int val int val …]` / `/Kids`),
/// invoking `visit(value)` for every value object. Used for `/PageLabels`.
pub fn walk_number_tree<'a>(
    doc: &'a Document,
    root: &'a Dictionary,
    mut visit: impl FnMut(&'a Object),
) {
    fn recurse<'a>(
        doc: &'a Document,
        node: &'a Dictionary,
        depth: u32,
        budget: &mut u32,
        visit: &mut impl FnMut(&'a Object),
    ) {
        if depth > 64 || *budget == 0 {
            return;
        }
        if let Some(nums) = get_array(doc, node, b"Nums") {
            for pair in nums.chunks(2) {
                if *budget == 0 {
                    return;
                }
                if let [_key, v] = pair {
                    if let Some(val) = resolve(doc, v) {
                        *budget -= 1;
                        visit(val);
                    }
                }
            }
        }
        if let Some(kids) = get_array(doc, node, b"Kids") {
            for kid in kids {
                if let Some(kid_dict) = resolve(doc, kid).and_then(|o| o.as_dict().ok()) {
                    recurse(doc, kid_dict, depth + 1, budget, visit);
                }
            }
        }
    }
    let mut budget = 100_000u32;
    recurse(doc, root, 0, &mut budget, &mut visit);
}

/// Iterates every object that carries a dictionary — plain dictionaries and
/// stream dictionaries alike — as `(id, &Dictionary)`. Centralizes the
/// object-graph dict coercion the whole-graph extractors (scripts, URIs,
/// optional content, signatures, metadata) would otherwise each re-derive, so
/// they can never disagree about which object kinds expose a scannable dict.
pub fn iter_dicts(doc: &Document) -> impl Iterator<Item = (ObjectId, &Dictionary)> {
    doc.objects.iter().filter_map(|(id, obj)| match obj {
        Object::Dictionary(d) => Some((*id, d)),
        Object::Stream(s) => Some((*id, &s.dict)),
        _ => None,
    })
}

/// True when `dict[key]` is a name equal to one of `names`. Keeps the fiddly
/// Option/Result-chained byte-name comparison (and its easy-to-flip
/// `unwrap_or(false)` default) in one place so a type filter can't silently
/// diverge across extractors.
pub fn name_is(dict: &Dictionary, key: &[u8], names: &[&[u8]]) -> bool {
    dict.get(key)
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| names.contains(&n))
        .unwrap_or(false)
}

/// The 1-based page numbers of every page, keyed by `ObjectId`, so page-level
/// extractors can label a finding "page N". Built from `Document::get_pages`.
pub fn page_numbers(doc: &Document) -> std::collections::HashMap<ObjectId, u32> {
    doc.get_pages()
        .into_iter()
        .map(|(num, id)| (id, num))
        .collect()
}

/// The decompressed bytes of a page's `/Contents` — a single stream, or an
/// array of streams each returned separately (the caller concatenates or scans
/// them). Shared by the marked-content and revision page scans.
pub fn page_content_streams(doc: &Document, page_id: ObjectId) -> Vec<Vec<u8>> {
    let Ok(page) = doc.get_dictionary(page_id) else { return Vec::new() };
    let Some(contents) = page.get(b"Contents").ok().and_then(|o| resolve(doc, o)) else {
        return Vec::new();
    };
    match contents {
        Object::Stream(_) => page
            .get(b"Contents")
            .ok()
            .and_then(|o| o.as_reference().ok())
            .and_then(|id| stream_bytes(doc, id))
            .into_iter()
            .collect(),
        Object::Array(parts) => parts
            .iter()
            .filter_map(|p| p.as_reference().ok())
            .filter_map(|id| stream_bytes(doc, id))
            .collect(),
        _ => Vec::new(),
    }
}

/// Concatenated show-operator text per page (1-based page number → text), from
/// decompressed content streams. A lopdf-only approximation of page text for
/// contexts where pdfium is not available (e.g. scanning a superseded
/// revision). Adjacent show strings within a page are joined, so a word split
/// across `Tj` operators still matches; it does NOT do pdfium's full geometric
/// reading-order reassembly, so it is a best-effort fallback, not a replacement
/// for the `PageText` extractor.
pub fn page_show_text(doc: &Document) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = Vec::new();
    for (page_id, page_num) in page_numbers(doc) {
        let mut text = String::new();
        for bytes in page_content_streams(doc, page_id) {
            for s in scan_content_strings(&bytes) {
                text.push_str(&s.value);
            }
        }
        if !text.is_empty() {
            out.push((page_num, text));
        }
    }
    out.sort_by_key(|(n, _)| *n);
    out
}

/// One string operand extracted from a content stream, with the `/Name` token
/// (if any) that immediately preceded it — enough to tell a marked-content
/// `/ActualText (…)` from an ordinary `(…) Tj` show operator.
pub struct ContentString {
    /// The `/Name` immediately before this string, lowercased of its bytes
    /// preserved as-is (e.g. `b"ActualText"`), or `None`.
    pub preceding_name: Option<Vec<u8>>,
    /// The decoded string value.
    pub value: String,
}

/// Extracts every literal `(…)` and hex `<…>` string operand from a decoded
/// content stream, in order, each tagged with the name token that preceded it.
/// This is a lightweight operand scanner, not a full content-stream parser —
/// it deliberately ignores operators and numbers; it only needs to surface the
/// text a redaction might have left in show operators, appearance streams, or
/// marked-content property lists (spec §4-A split-reassembly is pdfium's job;
/// this covers the *structural* streams pdfium doesn't extract).
pub fn scan_content_strings(content: &[u8]) -> Vec<ContentString> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut last_name: Option<Vec<u8>> = None;
    let n = content.len();
    while i < n {
        match content[i] {
            b'%' => {
                // Comment to end of line.
                while i < n && content[i] != b'\n' && content[i] != b'\r' {
                    i += 1;
                }
            }
            b'/' => {
                i += 1;
                let start = i;
                while i < n && !is_delimiter(content[i]) && !content[i].is_ascii_whitespace() {
                    i += 1;
                }
                last_name = Some(content[start..i].to_vec());
            }
            b'(' => {
                let (value, next) = read_literal_string(content, i);
                out.push(ContentString {
                    preceding_name: last_name.take(),
                    value: decode_pdf_text(&value),
                });
                i = next;
            }
            // Dictionary open `<<` — skip both so the second `<` is not read as
            // the start of a hex string.
            b'<' if i + 1 < n && content[i + 1] == b'<' => i += 2,
            b'<' if i + 1 < n && content[i + 1] != b'<' => {
                let (value, next) = read_hex_string(content, i);
                out.push(ContentString {
                    preceding_name: last_name.take(),
                    value: decode_pdf_text(&value),
                });
                i = next;
            }
            _ => i += 1,
        }
    }
    out
}

/// PDF delimiter characters that end a name token.
fn is_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// Reads a literal `(…)` string starting at `content[start] == b'('`, returning
/// its raw decoded bytes and the index just past the closing paren. Handles
/// nested parens, backslash escapes, and octal escapes.
fn read_literal_string(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut i = start + 1;
    let mut depth = 1i32;
    let n = content.len();
    while i < n {
        match content[i] {
            b'\\' => {
                i += 1;
                if i >= n {
                    break;
                }
                match content[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'(' => out.push(b'('),
                    b')' => out.push(b')'),
                    b'\\' => out.push(b'\\'),
                    d @ b'0'..=b'7' => {
                        // Up to three octal digits.
                        let mut val = (d - b'0') as u32;
                        for _ in 0..2 {
                            if i + 1 < n && (b'0'..=b'7').contains(&content[i + 1]) {
                                i += 1;
                                val = val * 8 + (content[i] - b'0') as u32;
                            } else {
                                break;
                            }
                        }
                        out.push(val as u8);
                    }
                    b'\n' => {} // line continuation
                    b'\r' => {
                        if i + 1 < n && content[i + 1] == b'\n' {
                            i += 1;
                        }
                    }
                    other => out.push(other),
                }
                i += 1;
            }
            b'(' => {
                depth += 1;
                out.push(b'(');
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    break;
                }
                out.push(b')');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    (out, i)
}

/// Reads a hex `<…>` string starting at `content[start] == b'<'`, returning its
/// decoded bytes and the index just past `>`.
fn read_hex_string(content: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut digits = Vec::new();
    let mut i = start + 1;
    let n = content.len();
    while i < n && content[i] != b'>' {
        if content[i].is_ascii_hexdigit() {
            digits.push(content[i]);
        }
        i += 1;
    }
    if i < n {
        i += 1; // consume '>'
    }
    if digits.len() % 2 == 1 {
        digits.push(b'0'); // odd final nibble is padded per spec
    }
    let bytes = digits
        .chunks_exact(2)
        .filter_map(|c| {
            let hi = (c[0] as char).to_digit(16)?;
            let lo = (c[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect();
    (bytes, i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn decode_latin1_and_utf16() {
        assert_eq!(decode_pdf_text(b"Zanzibar"), "Zanzibar");
        // UTF-16BE "Hi" with BOM.
        let utf16be = [0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        assert_eq!(decode_pdf_text(&utf16be), "Hi");
        // UTF-16LE "Hi" with BOM (lopdf doesn't handle LE; we do).
        let utf16le = [0xFF, 0xFE, 0x48, 0x00, 0x69, 0x00];
        assert_eq!(decode_pdf_text(&utf16le), "Hi");
    }

    #[test]
    fn decode_stream_text_prefers_utf8_then_utf16() {
        // A UTF-8 (no BOM) stream with a non-ASCII secret must round-trip — a
        // PDFDocEncoding-only decode would mangle it (regression guard).
        assert_eq!(decode_stream_text("Zürich".as_bytes()), "Zürich");
        assert_eq!(decode_stream_text(b"plain ascii"), "plain ascii");
        // UTF-16BE and LE (with BOM) still decode.
        assert_eq!(decode_stream_text(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]), "Hi");
        assert_eq!(decode_stream_text(&[0xFF, 0xFE, 0x48, 0x00, 0x69, 0x00]), "Hi");
        // Non-UTF-8, non-BOM bytes fall back to the PDF text-string decode
        // (PDFDocEncoding): 0x92 → trademark, not dropped or mis-cast.
        assert_eq!(decode_stream_text(&[0x92]), "\u{2122}");
    }

    #[test]
    fn decode_pdfdocencoding_punctuation() {
        // PDFDocEncoding 0x92 is the trademark sign and 0x90 the right single
        // quote — NOT Latin-1 C1 controls. A raw `b as char` cast would map
        // these to U+0092 / U+0090 and a query would miss the real glyphs.
        assert_eq!(decode_pdf_text(&[0x90]), "\u{2019}"); // right single quote
        assert_eq!(decode_pdf_text(&[0x92]), "\u{2122}"); // trademark
        // The apostrophe in a redacted name survives round-trip.
        let mut bytes = b"O".to_vec();
        bytes.push(0x90); // ’
        bytes.extend_from_slice(b"Brien");
        assert_eq!(decode_pdf_text(&bytes), "O\u{2019}Brien");
    }

    #[test]
    fn resolve_follows_references() {
        let mut doc = Document::with_version("1.5");
        let target = doc.add_object(Object::string_literal("deep"));
        let via_ref = Object::Reference(target);
        assert_eq!(string_text(resolve(&doc, &via_ref).unwrap()).unwrap(), "deep");
    }

    #[test]
    fn scan_content_strings_literal_hex_and_names() {
        let content = b"BT /F1 24 Tf (Zanzibar) Tj /Span <</ActualText (secret)>> BDC <5A616E> Tj";
        let strings = scan_content_strings(content);
        let values: Vec<&str> = strings.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&"Zanzibar"));
        assert!(values.contains(&"secret"));
        assert!(values.contains(&"Zan")); // <5A616E>
        // The /ActualText-preceded string is attributed to that name.
        let at = strings.iter().find(|s| s.value == "secret").unwrap();
        assert_eq!(at.preceding_name.as_deref(), Some(b"ActualText".as_ref()));
    }

    #[test]
    fn scan_content_strings_octal_escape() {
        // (\132\141\156) == "Zan"
        let strings = scan_content_strings(b"(\\132\\141\\156) Tj");
        assert_eq!(strings[0].value, "Zan");
    }

    #[test]
    fn walk_name_tree_collects_leaf_and_kids() {
        let mut doc = Document::with_version("1.5");
        let leaf = doc.add_object(dictionary! {
            "Names" => vec![
                Object::string_literal("k1"), Object::string_literal("v1"),
            ],
        });
        let root = dictionary! { "Kids" => vec![Object::Reference(leaf)] };
        let mut seen = Vec::new();
        walk_name_tree(&doc, &root, |k, v| {
            seen.push((k, string_text(v).unwrap_or_default()));
        });
        assert_eq!(seen, vec![("k1".to_string(), "v1".to_string())]);
    }
}

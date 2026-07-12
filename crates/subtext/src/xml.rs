//! XML text extraction for the XMP and XFA vectors (spec §7 note 3).
//!
//! XMP metadata and XFA packets are XML; a redaction can hide the secret in a
//! text node *or* an attribute value, and entities/CDATA must be decoded before
//! matching. `quick-xml` gives us that without a full DOM: we stream the events
//! and collect every text node, CDATA block, and attribute value into one
//! normalized string the query then matches against.

use quick_xml::events::Event;
use quick_xml::Reader;

/// All human-readable text carried by an XML document: text nodes, CDATA, and
/// attribute values, space-joined. Entities are decoded. Returns whatever was
/// parsed up to the first hard error (best-effort — a truncated/malformed XMP
/// packet still yields the text before the break).
pub fn visible_text(xml: &[u8]) -> String {
    let mut reader = Reader::from_reader(xml);
    let config = reader.config_mut();
    config.trim_text(true);
    let mut buf = Vec::new();
    let mut out: Vec<String> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Text(e)) => {
                if let Ok(t) = e.unescape() {
                    push_nonempty(&mut out, t.as_ref());
                }
            }
            Ok(Event::CData(e)) => {
                if let Ok(s) = std::str::from_utf8(e.as_ref()) {
                    push_nonempty(&mut out, s);
                }
            }
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                for attr in e.attributes().flatten() {
                    if let Ok(v) = attr.unescape_value() {
                        push_nonempty(&mut out, v.as_ref());
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out.join(" ")
}

fn push_nonempty(out: &mut Vec<String>, s: &str) {
    let trimmed = s.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_nodes_and_attributes() {
        let xml = br#"<rdf:Description xmlns:rdf="ns" dc:title="Zanzibar deal">
            <dc:creator>Alice</dc:creator>
        </rdf:Description>"#;
        let text = visible_text(xml);
        assert!(text.contains("Zanzibar deal"), "attr value missing: {text}");
        assert!(text.contains("Alice"), "text node missing: {text}");
        // Namespaces/tag names must not leak into the scanned text.
        assert!(!text.contains("rdf:Description"));
    }

    #[test]
    fn decodes_entities_and_cdata() {
        let xml = br#"<x><![CDATA[Zan & zibar]]><y>a &amp; b</y></x>"#;
        let text = visible_text(xml);
        assert!(text.contains("Zan & zibar"));
        assert!(text.contains("a & b"));
    }

    #[test]
    fn best_effort_on_malformed() {
        // Unclosed tag: we still get the text seen before the break.
        let xml = br#"<x>Zanzibar</x><broken"#;
        let text = visible_text(xml);
        assert!(text.contains("Zanzibar"));
    }
}

//! MCP server tests: transcripts of JSON-RPC lines in, lines out.

use super::cli::SharedWriter;
use super::mcp::{serve, PROTOCOL_VERSIONS};
use super::paths::tests::TempDir;
use super::tests::*;
use super::*;
use crate::commands::ocr::{OcrEngine, OcrWord};
use crate::commands::text::TextRect;
use crate::error::AppError;
use crate::state::AppState;
use base64::Engine as _;
use pdfium_render::prelude::Pdfium;
use serde_json::{json, Value};
use std::io::{BufReader, Read};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Runs a whole transcript: every line in, then end of input. Returns every
/// message the server wrote.
fn transcript(auto: Automation, lines: &[Value]) -> Vec<Value> {
    let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    serve(auto, BufReader::new(std::io::Cursor::new(input.into_bytes())), out.clone() as SharedWriter).expect("serve");
    messages(&out)
}

fn messages(out: &Arc<Mutex<Vec<u8>>>) -> Vec<Value> {
    let text = String::from_utf8(out.lock().unwrap().clone()).expect("utf-8");
    text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))).collect()
}

fn response(messages: &[Value], id: i64) -> Value {
    messages
        .iter()
        .find(|m| m["id"] == id && m.get("method").is_none())
        .cloned()
        .unwrap_or_else(|| panic!("no response to {id} in {messages:#?}"))
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(id: i64, tool: &str, args: Value) -> Value {
    request(id, "tools/call", json!({ "name": tool, "arguments": args }))
}

fn initialize(id: i64) -> Value {
    request(id, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }))
}

fn rooted_session(pdfium: &'static Pdfium, root: &TempDir) -> Automation {
    rooted(pdfium, root.path())
}

// ── Handshake and listing ────────────────────────────────────────────────────

#[test]
fn initialize_negotiates_the_protocol_version() {
    let pdfium = crate::test_pdfium();
    let out = transcript(
        session(pdfium.get()),
        &[
            initialize(1),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            request(2, "initialize", json!({ "protocolVersion": "2024-11-05" })),
            request(3, "initialize", json!({ "protocolVersion": "1999-01-01" })),
        ],
    );
    let r = response(&out, 1)["result"].clone();
    assert_eq!(r["protocolVersion"], "2025-06-18");
    assert_eq!(r["serverInfo"]["name"], "tumbler");
    assert_eq!(r["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(r["capabilities"]["tools"].is_object());
    assert!(r["instructions"].as_str().unwrap().contains("1-based"), "{r}");
    assert_eq!(response(&out, 2)["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(response(&out, 3)["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    // The notification got no reply.
    assert_eq!(out.len(), 3, "{out:#?}");
}

#[test]
fn tools_list_carries_schemas_and_hints() {
    let pdfium = crate::test_pdfium();
    let out = transcript(session(pdfium.get()), &[initialize(1), request(2, "tools/list", json!({}))]);
    let listed = response(&out, 2)["result"]["tools"].as_array().unwrap().clone();
    assert_eq!(listed.len(), tools().len());
    let render = listed.iter().find(|t| t["name"] == "page_render").unwrap();
    assert_eq!(render["inputSchema"]["type"], "object");
    assert_eq!(render["annotations"]["readOnlyHint"], true);
    let delete = listed.iter().find(|t| t["name"] == "page_delete").unwrap();
    assert_eq!(delete["annotations"]["destructiveHint"], true);
    assert_eq!(delete["annotations"]["readOnlyHint"], false);
}

#[test]
fn ping_is_answered() {
    let pdfium = crate::test_pdfium();
    let out = transcript(session(pdfium.get()), &[request(7, "ping", json!({}))]);
    assert_eq!(response(&out, 7)["result"], json!({}));
}

// ── Tool calls ───────────────────────────────────────────────────────────────

#[test]
fn a_successful_call_returns_text_and_structured_content() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("mcp");
    fixture_copy(&root, "a.pdf");
    let out = transcript(rooted_session(pdfium.get(), &root), &[initialize(1), call(2, "doc_open", json!({ "path": "a.pdf" }))]);
    let r = response(&out, 2)["result"].clone();
    assert_eq!(r["isError"], false);
    assert_eq!(r["structuredContent"]["doc"], 1);
    assert_eq!(r["content"][0]["type"], "text");
    let text: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["page_count"], 1);
}

/// A tool that fails is a result the agent can read, not a protocol error.
#[test]
fn a_failing_call_is_a_result_with_is_error() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("mcp");
    let outside = TempDir::new("outside");
    let elsewhere = fixture_copy(&outside, "x.pdf");
    let out = transcript(
        rooted_session(pdfium.get(), &root),
        &[
            call(2, "doc_open", json!({ "path": "missing.pdf" })),
            call(3, "doc_open", json!({ "path": s(&elsewhere) })),
            call(4, "doc_info", json!({ "doc": "one" })),
        ],
    );
    for (id, needle) in [(2, "missing.pdf"), (3, "outside"), (4, "`doc`")] {
        let r = response(&out, id)["result"].clone();
        assert_eq!(r["isError"], true, "{id}: {r}");
        assert!(r["content"][0]["text"].as_str().unwrap().contains(needle), "{id}: {r}");
    }
}

#[test]
fn page_render_returns_an_image_block() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("mcp");
    fixture_copy(&root, "a.pdf");
    let out = transcript(
        rooted_session(pdfium.get(), &root),
        &[call(1, "doc_open", json!({ "path": "a.pdf" })), call(2, "page_render", json!({ "doc": 1, "page": 1, "dpi": 72 }))],
    );
    let block = response(&out, 2)["result"]["content"][0].clone();
    assert_eq!((block["type"].clone(), block["mimeType"].clone()), (json!("image"), json!("image/png")));
    let png = base64::engine::general_purpose::STANDARD.decode(block["data"].as_str().unwrap()).unwrap();
    assert_eq!(png_size(&png), (200, 200));
}

// ── Protocol errors ──────────────────────────────────────────────────────────

#[test]
fn protocol_errors_use_json_rpc_codes() {
    let pdfium = crate::test_pdfium();
    let input = "this is not json\n\
                 {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"no/such/method\"}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":3}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"no_such_tool\"}}\n\
                 {\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{}}\n\
                 [1,2]\n\n";
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    serve(session(pdfium.get()), BufReader::new(input.as_bytes()), out.clone() as SharedWriter).unwrap();
    let out = messages(&out);
    let code = |m: &Value| m["error"]["code"].as_i64().unwrap();
    assert_eq!(code(&out[0]), -32700);
    assert_eq!(out[0]["id"], Value::Null);
    assert_eq!(code(&response(&out, 2)), -32601);
    assert_eq!(code(&response(&out, 3)), -32600);
    assert_eq!(code(&response(&out, 4)), -32602);
    assert_eq!(code(&response(&out, 5)), -32602);
    assert_eq!(code(out.last().unwrap()), -32600, "a batch array is not supported");
}

// ── Progress and cancellation ────────────────────────────────────────────────

/// Three blank pages, which OCR will visit one at a time.
fn three_blank_pages(dir: &TempDir) {
    use lopdf::{dictionary, Dictionary, Document, Object, Stream};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let kids: Vec<Object> = (0..3)
        .map(|_| {
            let content = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), Vec::new())));
            Object::Reference(doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => Object::Reference(pages_id),
                "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
                "Contents" => Object::Reference(content), "Resources" => Dictionary::new(),
            }))
        })
        .collect();
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 3 }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog));
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    dir.file("scan.pdf", &bytes);
}

#[test]
fn a_call_with_a_progress_token_reports_progress_before_its_result() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("mcp");
    three_blank_pages(&root);
    let out = transcript(
        rooted_session(pdfium.get(), &root),
        &[
            call(1, "doc_open", json!({ "path": "scan.pdf" })),
            request(2, "tools/call", json!({ "name": "ocr_recognize", "arguments": { "doc": 1 }, "_meta": { "progressToken": "tok" } })),
        ],
    );
    let progress: Vec<&Value> = out.iter().filter(|m| m["method"] == "notifications/progress").collect();
    assert_eq!(progress.len(), 3, "{out:#?}");
    assert_eq!(progress[0]["params"], json!({ "progressToken": "tok", "progress": 1, "total": 3 }));
    let last_progress = out.iter().rposition(|m| m["method"] == "notifications/progress").unwrap();
    let result_at = out.iter().position(|m| m["id"] == 2).unwrap();
    assert!(last_progress < result_at, "progress after the result");
    assert_eq!(response(&out, 2)["result"]["isError"], false);
}

/// Feeds the server one line at a time from the test, like a real client.
struct Pipe {
    lines: Receiver<String>,
    pending: Vec<u8>,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.lines.recv() {
                Ok(line) => self.pending = format!("{line}\n").into_bytes(),
                Err(_) => return Ok(0), // the test hung up: end of input
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

/// Signals each page it starts, then takes a while over it.
struct SlowOcr(Mutex<Sender<()>>);

impl OcrEngine for SlowOcr {
    fn recognize(&self, _: &[u8], _: u32, _: u32) -> Result<Vec<OcrWord>, AppError> {
        let _ = self.0.lock().unwrap().send(());
        std::thread::sleep(Duration::from_millis(300));
        Ok(vec![OcrWord { text: "x".into(), rect: TextRect { x: 10.0, y: 10.0, width: 10.0, height: 10.0 } }])
    }
}

fn wait_for(out: &Arc<Mutex<Vec<u8>>>, what: &str, found: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let now = messages(out);
        if found(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {now:#?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// While OCR runs, ping is still answered; notifications/cancelled stops the
/// run after its current page, nothing is applied, and the cancelled request
/// gets no response (as the MCP spec asks).
#[test]
fn cancellation_mid_ocr_stops_the_call_and_the_server_stays_responsive() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("mcp");
    three_blank_pages(&root);
    let (started_tx, started) = channel();
    let state = AppState::new(pdfium.get(), None).with_ocr_engine(Arc::new(SlowOcr(Mutex::new(started_tx))));
    let auto = Automation::new(state).with_root(root.path()).unwrap();

    let (send, lines) = channel::<String>();
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    let server_out = out.clone() as SharedWriter;
    let server = std::thread::spawn(move || serve(auto, BufReader::new(Pipe { lines, pending: Vec::new() }), server_out));

    send.send(call(1, "doc_open", json!({ "path": "scan.pdf" })).to_string()).unwrap();
    wait_for(&out, "doc_open", |m| m.iter().any(|m| m["id"] == 1));
    send.send(call(2, "ocr_recognize", json!({ "doc": 1 })).to_string()).unwrap();
    started.recv_timeout(Duration::from_secs(20)).expect("OCR started");

    send.send(request(3, "ping", json!({})).to_string()).unwrap();
    let now = wait_for(&out, "ping", |m| m.iter().any(|m| m["id"] == 3));
    assert!(!now.iter().any(|m| m["id"] == 2), "ping waited for the OCR call");

    send.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 2, "reason": "test" } }).to_string()).unwrap();
    send.send(call(4, "doc_info", json!({ "doc": 1 })).to_string()).unwrap();
    let now = wait_for(&out, "doc_info", |m| m.iter().any(|m| m["id"] == 4));
    drop(send);
    server.join().unwrap().unwrap();

    assert!(!now.iter().any(|m| m["id"] == 2), "a cancelled request gets no response: {now:#?}");
    let info = response(&messages(&out), 4)["result"]["structuredContent"].clone();
    assert_eq!(info["dirty"], false, "the cancelled edit was not applied");
    assert!(started.try_iter().count() < 2, "OCR kept going after the cancel");
}

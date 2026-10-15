//! A Model Context Protocol server over the tool table: `tumbler-cli mcp`.
//!
//! It runs only when an agent launches it, talks newline-delimited JSON-RPC
//! 2.0 over stdin/stdout, opens no port, and stops when stdin closes.
//! Implemented: `initialize`, `ping`, `tools/list`, `tools/call`, the
//! notifications a client sends (`notifications/initialized`,
//! `notifications/cancelled`), and outgoing `notifications/progress`.
//!
//! Tool calls run one at a time on a worker thread, since there is one pdfium
//! instance and the documents share it. The reading thread answers everything
//! else itself, so `ping` and cancellation still work while a call runs. A
//! tool that fails returns a result with `isError: true`, which the agent can
//! read; JSON-RPC error codes are kept for protocol errors.

use super::cli::SharedWriter;
use super::{tools, Automation, Content, Progress, ToolDef, ToolError};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

/// Protocol revisions spoken, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "Tumbler reads and edits PDF files. Open a file with doc_open to get a \
document id, which the other tools take. Edits stay in memory until doc_save writes them; there is no \
undo, so doc_close with discard_changes is how to abandon them. Pages are 1-based, and rectangles are in \
points from the top-left of the page as displayed. Every path must be inside the folder this server was \
started with (--root); relative paths are taken from it.";

/// A tools/call waiting for, or on, the worker.
struct Job {
    id: Value,
    tool: String,
    args: Value,
    progress_token: Option<Value>,
    cancel: Arc<AtomicBool>,
}

/// Cancel tokens of the calls not yet answered, by request id.
type InFlight = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;

/// Serves until `input` ends, then lets the call in progress finish.
pub fn serve(auto: Automation, input: impl BufRead, output: SharedWriter) -> std::io::Result<()> {
    let in_flight: InFlight = Arc::default();
    let (jobs, queue) = channel::<Job>();
    std::thread::scope(|scope| {
        let worker = {
            let (output, in_flight) = (output.clone(), in_flight.clone());
            scope.spawn(move || work(auto, queue, output, in_flight))
        };
        let mut result = Ok(());
        for line in input.lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(message) => handle(&message, &jobs, &in_flight),
                Err(e) => Some(error(Value::Null, PARSE_ERROR, &format!("parse error: {e}"))),
            };
            if let Some(reply) = reply {
                if let Err(e) = send(&output, &reply) {
                    result = Err(e);
                    break;
                }
            }
        }
        drop(jobs);
        let _ = worker.join();
        result
    })
}

/// One incoming message. Returns the reply due now, if any; a tools/call is
/// queued for the worker, which replies itself.
fn handle(message: &Value, jobs: &std::sync::mpsc::Sender<Job>, in_flight: &InFlight) -> Option<Value> {
    let Some(obj) = message.as_object() else {
        return Some(error(Value::Null, INVALID_REQUEST, "expected one JSON-RPC message object (batches are not supported)"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // Not a request: a response to something we sent (we send none), or
        // malformed.
        return id.map(|id| error(id, INVALID_REQUEST, "missing method"));
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        // A notification: nothing to answer.
        if method == "notifications/cancelled" {
            if let Some(request) = params.get("requestId") {
                if let Some(token) = in_flight.lock().ok()?.get(&request.to_string()) {
                    token.store(true, Ordering::Relaxed);
                }
            }
        }
        return None;
    };
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "tumbler", "title": "Tumbler", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools().iter().map(tool_json).collect::<Vec<_>>() })),
        "tools/call" => match params.get("name").and_then(Value::as_str) {
            None => Err((INVALID_PARAMS, "tools/call needs a tool name".to_string())),
            Some(name) if tools::find(name).is_none() => Err((INVALID_PARAMS, format!("unknown tool {name:?}"))),
            Some(name) => {
                let cancel = Arc::new(AtomicBool::new(false));
                if let Ok(mut map) = in_flight.lock() {
                    map.insert(id.to_string(), cancel.clone());
                }
                let progress_token = params.get("_meta").and_then(|m| m.get("progressToken")).cloned();
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                let job = Job { id: id.clone(), tool: name.to_string(), args, progress_token, cancel };
                if jobs.send(job).is_ok() {
                    return None;
                }
                Err((INVALID_REQUEST, "the server is shutting down".to_string()))
            }
        },
        other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => error(id, code, &message),
    })
}

/// The worker: runs queued calls in order and writes their responses.
fn work(mut auto: Automation, queue: Receiver<Job>, output: SharedWriter, in_flight: InFlight) {
    for job in queue {
        // Cancelled while still queued: never started, so nothing to undo.
        if job.cancel.load(Ordering::Relaxed) {
            if let Ok(mut map) = in_flight.lock() {
                map.remove(&job.id.to_string());
            }
            continue;
        }
        let progress = progress_notifier(job.progress_token.clone(), output.clone());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            auto.call_with(&job.tool, &job.args, progress, job.cancel.clone())
        }));
        if let Ok(mut map) = in_flight.lock() {
            map.remove(&job.id.to_string());
        }
        // The client cancelled it, so it expects no response.
        if job.cancel.load(Ordering::Relaxed) {
            continue;
        }
        let result = match outcome {
            Ok(Ok(content)) => call_result(content),
            Ok(Err(ToolError::UnknownTool(t))) => {
                let _ = send(&output, &error(job.id, INVALID_PARAMS, &format!("unknown tool {t:?}")));
                continue;
            }
            Ok(Err(e)) => tool_error(&e.to_string()),
            Err(_) => tool_error("internal error: the tool panicked; the document may be in an unknown state"),
        };
        let _ = send(&output, &json!({ "jsonrpc": "2.0", "id": job.id, "result": result }));
    }
}

/// `notifications/progress` for a call that asked for it with a token. The
/// protocol wants progress to increase, so a value that doesn't (a second
/// pass starting over) is dropped.
fn progress_notifier(token: Option<Value>, output: SharedWriter) -> Progress {
    let Some(token) = token else {
        return Arc::new(|_, _| {});
    };
    let last = Mutex::new(0u32);
    Arc::new(move |done, total| {
        let Ok(mut last) = last.lock() else { return };
        if done <= *last {
            return;
        }
        *last = done;
        let note = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": { "progressToken": token, "progress": done, "total": total },
        });
        let _ = send(&output, &note);
    })
}

/// Writes one message as one line; the lock keeps the reader's and the
/// worker's lines whole.
fn send(output: &SharedWriter, message: &Value) -> std::io::Result<()> {
    let mut out = output.lock().unwrap_or_else(|p| p.into_inner());
    writeln!(out, "{message}")?;
    out.flush()
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_json(t: &ToolDef) -> Value {
    json!({
        "name": t.name,
        "title": t.title,
        "description": t.description,
        "inputSchema": t.input_schema,
        "annotations": {
            "title": t.title,
            "readOnlyHint": t.read_only,
            "destructiveHint": t.destructive,
            "idempotentHint": t.read_only,
            "openWorldHint": false,
        },
    })
}

fn call_result(content: Vec<Content>) -> Value {
    let mut structured = None;
    let blocks: Vec<Value> = content
        .into_iter()
        .map(|c| match c {
            Content::Json(v) => {
                let text = serde_json::to_string_pretty(&v).unwrap_or_default();
                if v.is_object() && structured.is_none() {
                    structured = Some(v);
                }
                json!({ "type": "text", "text": text })
            }
            Content::Png { data, .. } => json!({
                "type": "image",
                "mimeType": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            }),
        })
        .collect();
    let mut result = json!({ "content": blocks, "isError": false });
    if let Some(structured) = structured {
        result["structuredContent"] = structured;
    }
    result
}

fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

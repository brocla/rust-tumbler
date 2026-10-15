//! In-process tests of the CLI front door: parsing, exit codes and stdout
//! shape. Tool behavior itself is covered through the table in `tests.rs`.

use super::cli::{main_with, parse_kv, parse_pages, CliEnv, Io, SharedWriter};
use super::paths::tests::TempDir;
use super::tests::*;
use super::*;
use crate::state::AppState;
use pdfium_render::prelude::Pdfium;
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// What one run of the CLI produced.
struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.stdout))
    }
}

struct Options {
    env: CliEnv,
    cancel: Arc<AtomicBool>,
    stderr_is_terminal: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { env: CliEnv::default(), cancel: Arc::default(), stderr_is_terminal: false }
    }
}

fn cli(pdfium: &'static Pdfium, args: &[&str]) -> Run {
    cli_with(pdfium, args, Options::default())
}

fn cli_with(pdfium: &'static Pdfium, args: &[&str], opts: Options) -> Run {
    cli_session(args, opts, || session(pdfium))
}

/// A run with its own session factory (another OCR engine, say).
fn cli_session(args: &[&str], opts: Options, make: impl FnOnce() -> Automation) -> Run {
    let stdout = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
    let io = Io {
        stdin: Box::new(std::io::empty()),
        stdout: stdout.clone() as SharedWriter,
        stderr: stderr.clone() as SharedWriter,
        stderr_is_terminal: opts.stderr_is_terminal,
    };
    let argv = std::iter::once("tumbler-cli").chain(args.iter().copied()).map(Into::into).collect();
    let code = main_with(argv, opts.env, io, opts.cancel, || Ok(make()));
    let text = |b: &Arc<Mutex<Vec<u8>>>| String::from_utf8(b.lock().unwrap().clone()).expect("utf-8");
    Run { code, stdout: text(&stdout), stderr: text(&stderr) }
}

fn ps(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Three blank pages of different sizes (lopdf, no pdfium).
fn three_pages(dir: &TempDir, name: &str) -> std::path::PathBuf {
    use lopdf::{dictionary, Dictionary, Document, Object, Stream};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let kids: Vec<Object> = [(200.0, 200.0), (300.0, 300.0), (400.0, 200.0)]
        .iter()
        .map(|&(w, h)| {
            let content = doc.add_object(Object::Stream(Stream::new(Dictionary::new(), Vec::new())));
            Object::Reference(doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => Object::Reference(pages_id),
                "MediaBox" => vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)],
                "Contents" => Object::Reference(content), "Resources" => Dictionary::new(),
            }))
        })
        .collect();
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 3 }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog));
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    dir.file(name, &out)
}

fn page_count(path: &Path) -> usize {
    lopdf::Document::load(path).expect("load").get_pages().len()
}

fn is_encrypted_file(path: &Path) -> bool {
    let bytes = std::fs::read(path).unwrap();
    bytes.windows(8).any(|w| w == b"/Encrypt")
}

// ── Parsing helpers ──────────────────────────────────────────────────────────

#[test]
fn key_value_pairs_parse_as_json_else_as_strings() {
    assert_eq!(parse_kv("pages=[1,3]").unwrap(), ("pages".into(), json!([1, 3])));
    assert_eq!(parse_kv("degrees=90").unwrap(), ("degrees".into(), json!(90)));
    assert_eq!(parse_kv("force=true").unwrap(), ("force".into(), json!(true)));
    assert_eq!(parse_kv(r"path=C:\a b.pdf").unwrap(), ("path".into(), json!(r"C:\a b.pdf")));
    assert_eq!(parse_kv("query=a=b").unwrap(), ("query".into(), json!("a=b")));
    assert!(parse_kv("noequals").is_err());
}

#[test]
fn page_lists_take_numbers_and_ranges() {
    assert_eq!(parse_pages("1,3,5-7").unwrap(), [1, 3, 5, 6, 7]);
    assert_eq!(parse_pages(" 2 ").unwrap(), [2]);
    for bad in ["", "0", "3-1", "a", "1,,2"] {
        assert!(parse_pages(bad).is_err(), "{bad:?}");
    }
}

// ── tools, --version, --help, usage errors ───────────────────────────────────

#[test]
fn version_and_help() {
    let pdfium = crate::test_pdfium();
    let r = cli(pdfium.get(), &["--version"]);
    assert_eq!((r.code, r.stdout.trim()), (0, concat!("tumbler-cli ", env!("CARGO_PKG_VERSION"))));
    let r = cli(pdfium.get(), &["--help"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("run") && r.stdout.contains("tools"), "{}", r.stdout);
}

#[test]
fn usage_errors_exit_2() {
    let pdfium = crate::test_pdfium();
    let r = cli(pdfium.get(), &["--bogus"]);
    assert_eq!(r.code, 2);
    assert!(!r.stderr.is_empty());
    assert_eq!(cli(pdfium.get(), &["render", "x.pdf", "--page", "1"]).code, 2, "render needs --out");
    assert_eq!(cli(pdfium.get(), &[]).code, 2);
}

#[test]
fn tools_lists_every_tool() {
    let pdfium = crate::test_pdfium();
    let r = cli(pdfium.get(), &["tools"]);
    assert_eq!(r.code, 0);
    for t in tools() {
        assert!(r.stdout.contains(t.name), "{} missing from:\n{}", t.name, r.stdout);
    }
    let r = cli(pdfium.get(), &["tools", "--json"]);
    let list = r.json();
    assert_eq!(list.as_array().unwrap().len(), tools().len());
    assert_eq!(list[0]["input_schema"]["type"], "object");
    assert!(list[0].get("read_only").is_some() && list[0].get("destructive").is_some());
}

// ── run ──────────────────────────────────────────────────────────────────────

#[test]
fn run_one_tool_prints_its_json() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let a = fixture_copy(&dir, "a.pdf");
    let r = cli(pdfium.get(), &["run", "doc_open", &format!("path={}", ps(&a))]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["doc"], 1);
}

#[test]
fn run_exit_codes_distinguish_usage_from_failure() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    assert_eq!(cli(pdfium.get(), &["run", "no_such_tool"]).code, 2);
    assert_eq!(cli(pdfium.get(), &["run", "doc_open", "path=x.pdf", "typo=1"]).code, 2);
    assert_eq!(cli(pdfium.get(), &["run", "doc_open", "noequals"]).code, 2);
    let r = cli(pdfium.get(), &["run", "doc_open", &format!("path={}", ps(&dir.0.join("missing.pdf")))]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("missing.pdf"), "{}", r.stderr);
}

/// Passwords never go on the command line, where shell history and the
/// process list would keep them; they come from a file or the environment.
#[test]
fn run_refuses_passwords_on_the_command_line_and_takes_them_from_the_environment() {
    let pdfium = crate::test_pdfium();
    let enc = ps(&crate::encrypted_fixture_path());
    let r = cli(pdfium.get(), &["run", "doc_open", &format!("path={enc}"), "password=open-sesame"]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("--password-file"), "{}", r.stderr);

    let env = CliEnv { password: Some(crate::ENCRYPTED_FIXTURE_PASSWORD.into()), ..Default::default() };
    let r = cli_with(pdfium.get(), &["run", "doc_open", &format!("path={enc}")], Options { env, ..Default::default() });
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["encrypted"], true);
}

#[test]
fn run_takes_the_password_from_a_file() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let pw = dir.file("pw.txt", b"open-sesame\r\n");
    let enc = ps(&crate::encrypted_fixture_path());
    let r = cli(pdfium.get(), &["run", "doc_open", &format!("path={enc}"), "--password-file", &ps(&pw)]);
    assert_eq!(r.code, 0, "{}", r.stderr);
}

#[test]
fn run_root_confines_paths() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("cli");
    let outside = TempDir::new("outside");
    let elsewhere = fixture_copy(&outside, "x.pdf");
    let r = cli(pdfium.get(), &["run", "doc_open", &format!("path={}", ps(&elsewhere)), "--root", &ps(root.path())]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("outside"), "{}", r.stderr);
}

#[test]
fn run_script_runs_steps_in_one_session() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("cli");
    three_pages(&root, "in.pdf");
    let script = root.file(
        "steps.json",
        json!([
            { "tool": "doc_open", "args": { "path": "in.pdf" } },
            { "tool": "page_rotate", "args": { "doc": 1, "pages": [1], "degrees": 90 } },
            { "tool": "page_render", "args": { "doc": 1, "page": 3, "dpi": 72 }, "out": "p3.png" },
            { "tool": "doc_save", "args": { "doc": 1, "path": "out.pdf" } }
        ])
        .to_string()
        .as_bytes(),
    );
    let r = cli(pdfium.get(), &["run", "--script", &ps(&script), "--root", &ps(root.path())]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let steps = r.json();
    assert_eq!(steps.as_array().unwrap().len(), 4);
    assert_eq!((steps[1]["step"].clone(), steps[1]["tool"].clone()), (json!(2), json!("page_rotate")));
    assert_eq!(steps[2]["result"]["image"], ps(&root.0.join("p3.png")));
    assert_eq!((steps[2]["result"]["width"].clone(), steps[2]["result"]["height"].clone()), (json!(400), json!(200)));
    assert!(root.0.join("p3.png").exists() && root.0.join("out.pdf").exists());
}

#[test]
fn run_script_stops_at_the_first_failing_step() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("cli");
    fixture_copy(&root, "a.pdf");
    let script = root.file(
        "steps.json",
        json!([
            { "tool": "doc_open", "args": { "path": "a.pdf" } },
            { "tool": "page_delete", "args": { "doc": 1, "pages": [1] } },
            { "tool": "doc_save", "args": { "doc": 1 } }
        ])
        .to_string()
        .as_bytes(),
    );
    let r = cli(pdfium.get(), &["run", "--script", &ps(&script), "--root", &ps(root.path())]);
    assert_eq!(r.code, 1);
    assert_eq!(r.json().as_array().unwrap().len(), 1, "completed steps are still printed");
    assert!(r.stderr.contains("step 2") && r.stderr.contains("page_delete"), "{}", r.stderr);
}

#[test]
fn image_results_without_out_are_summarized() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("cli");
    fixture_copy(&root, "a.pdf");
    let script = root.file(
        "s.json",
        br#"[{"tool":"doc_open","args":{"path":"a.pdf"}},{"tool":"page_render","args":{"doc":1,"page":1}}]"#,
    );
    let r = cli(pdfium.get(), &["run", "--script", &ps(&script), "--root", &ps(root.path())]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let summary = &r.json()[1]["result"];
    assert_eq!(summary["image"], "png");
    assert!(summary["hint"].as_str().unwrap().contains("out"), "{summary}");
}

// ── Friendly commands ────────────────────────────────────────────────────────

#[test]
fn info_prints_doc_info() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let r = cli(pdfium.get(), &["info", &ps(&fixture_copy(&dir, "a.pdf"))]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["page_count"], 1);
    assert!(r.json().get("metadata").is_some());
}

#[test]
fn text_prints_plain_text_with_form_feeds_between_pages() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let a = fixture_copy(&dir, "a.pdf");
    let r = cli(pdfium.get(), &["text", &ps(&a)]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("Test Fixture"), "{}", r.stdout);

    let three = three_pages(&dir, "three.pdf");
    let r = cli(pdfium.get(), &["text", &ps(&three)]);
    assert_eq!(r.stdout.matches('\x0c').count(), 2, "{:?}", r.stdout);
    let r = cli(pdfium.get(), &["text", &ps(&three), "--page", "2"]);
    assert_eq!(r.stdout.matches('\x0c').count(), 0);
}

#[test]
fn text_with_ocr_recognizes_scanned_pages() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let blank = dir.file("scan.pdf", &crate::geometry_page_bytes(300.0, 200.0, 0, None));
    assert!(!cli(pdfium.get(), &["text", &ps(&blank)]).stdout.contains("Scanned"));
    let r = cli(pdfium.get(), &["text", &ps(&blank), "--ocr"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("Scanned"), "{}", r.stdout);
}

#[test]
fn find_prints_matches() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let a = ps(&fixture_copy(&dir, "a.pdf"));
    assert_eq!(cli(pdfium.get(), &["find", &a, "fixture"]).json()["total"], 1);
    assert_eq!(cli(pdfium.get(), &["find", &a, "fixture", "--case"]).json()["total"], 0);
    assert_eq!(cli(pdfium.get(), &["find", &a, "Fix", "--word"]).json()["total"], 0);
    assert_eq!(cli(pdfium.get(), &["find", &a, "F.x", "--regex"]).json()["total"], 1);
}

#[test]
fn render_writes_a_png() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let a = ps(&fixture_copy(&dir, "a.pdf"));
    let out = dir.0.join("p.png");
    let r = cli(pdfium.get(), &["render", &a, "--page", "1", "--dpi", "72", "--out", &ps(&out)]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["image"], ps(&out));
    assert_eq!(png_size(&std::fs::read(&out).unwrap()), (200, 200));
}

#[test]
fn edit_applies_page_and_metadata_changes_and_saves() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let input = three_pages(&dir, "in.pdf");
    let out = dir.0.join("out.pdf");
    let r = cli(
        pdfium.get(),
        &["edit", &ps(&input), "--out", &ps(&out), "--rotate", "1,3:90", "--delete", "2", "--title", "Edited"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["path"], ps(&out));
    assert_eq!(page_count(&out), 2);
    assert_eq!(page_count(&input), 3, "the input is untouched");
    let info = cli(pdfium.get(), &["info", &ps(&out)]).json();
    assert_eq!(info["metadata"]["title"], "Edited");
    assert_eq!(info["pages"], json!([{ "width": 200.0, "height": 200.0 }, { "width": 200.0, "height": 400.0 }]));
}

#[test]
fn edit_rejects_nothing_to_do_and_ambiguous_flags() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let input = ps(&three_pages(&dir, "in.pdf"));
    let out = ps(&dir.0.join("out.pdf"));
    assert_eq!(cli(pdfium.get(), &["edit", &input, "--out", &out]).code, 2);
    assert_eq!(cli(pdfium.get(), &["edit", &input, "--out", &out, "--delete", "1", "--order", "2,1"]).code, 2);
    assert_eq!(cli(pdfium.get(), &["edit", &input, "--out", &out, "--rotate", "1:45"]).code, 2);
    assert_eq!(cli(pdfium.get(), &["edit", &input, "--out", &out, "--set-password", "--remove-password"]).code, 2);
}

#[test]
fn edit_sets_and_removes_passwords() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let input = ps(&fixture_copy(&dir, "a.pdf"));
    let locked = dir.0.join("locked.pdf");
    let env = CliEnv { new_password: Some("n3w".into()), ..Default::default() };
    let r = cli_with(pdfium.get(), &["edit", &input, "--out", &ps(&locked), "--set-password"], Options { env, ..Default::default() });
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(is_encrypted_file(&locked));

    // Without a new password to set, --set-password is a usage error.
    assert_eq!(cli(pdfium.get(), &["edit", &input, "--out", &ps(&dir.0.join("x.pdf")), "--set-password"]).code, 2);

    let plain = dir.0.join("plain.pdf");
    let env = CliEnv { password: Some("n3w".into()), ..Default::default() };
    let r = cli_with(pdfium.get(), &["edit", &ps(&locked), "--out", &ps(&plain), "--remove-password"], Options { env, ..Default::default() });
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(!is_encrypted_file(&plain));
}

#[test]
fn opening_a_protected_file_without_a_password_says_how_to_give_one() {
    let pdfium = crate::test_pdfium();
    let r = cli(pdfium.get(), &["info", &ps(&crate::encrypted_fixture_path())]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("--password-file") && r.stderr.contains("TUMBLER_PDF_PASSWORD"), "{}", r.stderr);
}

#[test]
fn combine_and_split() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let a = ps(&three_pages(&dir, "a.pdf"));
    let b = ps(&fixture_copy(&dir, "b.pdf"));
    let combined = dir.0.join("ab.pdf");
    let r = cli(pdfium.get(), &["combine", &a, &b, "--out", &ps(&combined)]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(page_count(&combined), 4);
    assert_eq!(cli(pdfium.get(), &["combine", &a, "--out", &ps(&combined)]).code, 2, "needs two inputs");

    std::fs::create_dir(dir.0.join("parts")).unwrap();
    let r = cli(pdfium.get(), &["split", &ps(&combined), "--out-dir", &ps(&dir.0.join("parts")), "--every", "2"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["files"].as_array().unwrap().len(), 2);
    let r = cli(pdfium.get(), &["split", &ps(&combined), "--out-dir", &ps(&dir.0.join("parts")), "--ranges", "1-1,2-4"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(page_count(&dir.0.join("parts").join("ab-2-4.pdf")), 3);
}

#[test]
fn ocr_compress_linearize_and_expand_margins_write_their_output() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let scan = ps(&dir.file("scan.pdf", &crate::geometry_page_bytes(300.0, 200.0, 0, None)));
    let a = ps(&fixture_copy(&dir, "a.pdf"));
    let out = |n: &str| ps(&dir.0.join(n));

    let r = cli(pdfium.get(), &["ocr", &scan, "--out", &out("ocr.pdf")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["pages_written"], 1);
    assert!(cli(pdfium.get(), &["text", &out("ocr.pdf")]).stdout.contains("Scanned"));

    let r = cli(pdfium.get(), &["compress", &a, "--out", &out("small.pdf"), "--steps", "recompress_streams,prune_unused"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["results"].as_array().unwrap().len(), 2);
    assert!(dir.0.join("small.pdf").exists());
    assert_eq!(cli(pdfium.get(), &["compress", &a, "--out", &out("x.pdf"), "--steps", "shrink"]).code, 2);

    let r = cli(pdfium.get(), &["linearize", &a, "--out", &out("web.pdf")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(dir.0.join("web.pdf").exists());

    let r = cli(pdfium.get(), &["expand-margins", &a, "--out", &out("big.pdf"), "--padding", "9"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.json()["scale"].as_f64().unwrap() > 1.0);
}

// ── Progress, cancellation, broken pipe ──────────────────────────────────────

#[test]
fn progress_goes_to_stderr_only_when_asked_or_on_a_terminal() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let three = ps(&three_pages(&dir, "scan.pdf"));
    let r = cli(pdfium.get(), &["text", &three, "--ocr"]);
    assert!(!r.stderr.contains("3/3"), "no progress when piped: {}", r.stderr);
    let r = cli(pdfium.get(), &["text", &three, "--ocr", "--progress"]);
    assert!(r.stderr.contains("1/3") && r.stderr.contains("3/3"), "{}", r.stderr);
    let r = cli_with(pdfium.get(), &["text", &three, "--ocr"], Options { stderr_is_terminal: true, ..Default::default() });
    assert!(r.stderr.contains("3/3"), "{}", r.stderr);
}

#[test]
fn cancellation_fails_the_command_and_writes_nothing() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let scan = ps(&dir.file("scan.pdf", &crate::geometry_page_bytes(300.0, 200.0, 0, None)));
    let out = dir.0.join("ocr.pdf");
    let opts = Options { cancel: Arc::new(AtomicBool::new(true)), ..Default::default() };
    let r = cli_with(pdfium.get(), &["ocr", &scan, "--out", &ps(&out)], opts);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("cancelled"), "{}", r.stderr);
    assert!(!out.exists());
}

/// `tumbler-cli tools | head` must not report an error when head closes the
/// pipe early.
#[test]
fn a_closed_stdout_pipe_exits_0() {
    struct ClosedPipe;
    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let pdfium = crate::test_pdfium();
    let io = Io {
        stdin: Box::new(std::io::empty()),
        stdout: Arc::new(Mutex::new(ClosedPipe)),
        stderr: Arc::new(Mutex::new(Vec::<u8>::new())),
        stderr_is_terminal: false,
    };
    let argv = vec!["tumbler-cli".into(), "tools".into()];
    let code = main_with(argv, CliEnv::default(), io, Arc::default(), || {
        Ok(Automation::new(AppState::new(pdfium.get(), None)))
    });
    assert_eq!(code, 0);
}

// ── mcp ──────────────────────────────────────────────────────────────────────

#[test]
fn mcp_requires_root() {
    let pdfium = crate::test_pdfium();
    let r = cli(pdfium.get(), &["mcp"]);
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("--root"), "{}", r.stderr);
}

#[test]
fn mcp_serves_stdin_until_it_closes() {
    let pdfium = crate::test_pdfium();
    let root = TempDir::new("cli");
    let stdout = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
    let input = format!("{}\n", json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }));
    let io = Io {
        stdin: Box::new(std::io::Cursor::new(input.into_bytes())),
        stdout: stdout.clone() as SharedWriter,
        stderr: stderr.clone() as SharedWriter,
        stderr_is_terminal: false,
    };
    let argv = vec!["tumbler-cli".into(), "mcp".into(), "--root".into(), root.0.clone().into_os_string()];
    let code = main_with(argv, CliEnv::default(), io, Arc::default(), || Ok(session(pdfium.get())));
    assert_eq!(code, 0);
    let out: Value = serde_json::from_slice(&stdout.lock().unwrap()).expect("one JSON line");
    assert_eq!((out["id"].clone(), out["result"].clone()), (json!(1), json!({})));
    let banner = String::from_utf8(stderr.lock().unwrap().clone()).unwrap();
    assert!(banner.contains("MCP server"), "{banner}");
}

// ── __path (installer support) ───────────────────────────────────────────────

#[test]
fn hidden_path_command_adds_and_removes_its_folder() {
    use super::user_path::{registry, tests::ScratchKey};
    let pdfium = crate::test_pdfium();
    let key = ScratchKey::new();
    registry::write(&key.0, r"%SystemRoot%\tools", registry::Kind::Expand).unwrap();
    let opts = || Options { env: CliEnv { path_key: Some(key.0.clone()), ..Default::default() }, ..Default::default() };
    let dir = r"C:\Users\someone\AppData\Local\Tumbler";

    let r = cli_with(pdfium.get(), &["__path", "add", dir], opts());
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["changed"], true);
    assert_eq!(registry::read(&key.0).unwrap().unwrap().0, format!(r"%SystemRoot%\tools;{dir}"));
    assert_eq!(cli_with(pdfium.get(), &["__path", "add", dir], opts()).json()["changed"], false);

    let r = cli_with(pdfium.get(), &["__path", "remove", dir], opts());
    assert_eq!(r.json()["changed"], true);
    assert_eq!(registry::read(&key.0).unwrap().unwrap().0, r"%SystemRoot%\tools");

    assert_eq!(cli_with(pdfium.get(), &["__path", "add", "relative"], opts()).code, 2);
    assert_eq!(cli_with(pdfium.get(), &["__path", "toggle", dir], opts()).code, 2);
    assert!(!cli(pdfium.get(), &["--help"]).stdout.contains("__path"), "installer plumbing stays out of --help");
}

/// At a shell, `split --out-dir parts` creates the folder; under --root it
/// must already exist, so nothing is created outside the root.
#[test]
fn split_creates_its_output_folder_only_without_root() {
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let input = ps(&three_pages(&dir, "in.pdf"));
    let parts = dir.0.join("new").join("parts");
    let r = cli(pdfium.get(), &["split", &input, "--out-dir", &ps(&parts)]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["files"].as_array().unwrap().len(), 3);

    let r = cli(pdfium.get(), &["split", "in.pdf", "--out-dir", "missing", "--root", &ps(dir.path())]);
    assert_eq!(r.code, 1);
    assert!(!dir.0.join("missing").exists());
}

// ── redact (v1.1) ────────────────────────────────────────────────────────────

#[test]
fn redact_writes_a_verified_copy() {
    use super::tests_v11::{session_with, NoWords};
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let input = ps(&fixture_copy(&dir, "a.pdf"));
    let out = dir.0.join("red.pdf");
    let args = ["redact", &input, "--term", "Fixture", "--rect", "1:10,150,20,20", "--dpi", "100", "--out", &ps(&out)];
    let r = cli_session(&args, Options::default(), || session_with(pdfium.get(), Arc::new(NoWords), &dir));
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.json()["verified"], true);
    assert!(out.exists());
    let r = cli_session(&["redact", &input, "--out", &ps(&out)], Options::default(), || session(pdfium.get()));
    assert_eq!(r.code, 2, "nothing to redact is a usage error");
    let r = cli_session(&["redact", &input, "--rect", "1:10,x", "--out", &ps(&out)], Options::default(), || session(pdfium.get()));
    assert_eq!(r.code, 2);
}

/// Exit code 3: the redaction ran but could not be verified, so nothing was
/// written. The result still goes to stdout, so a script can see the leaks.
#[test]
fn unverified_redaction_exits_3_and_writes_nothing() {
    use super::tests_v11::{session_with, SeesThroughBlackBoxes};
    let pdfium = crate::test_pdfium();
    let dir = TempDir::new("cli");
    let input = ps(&fixture_copy(&dir, "a.pdf"));
    let out = dir.0.join("leaky.pdf");
    let args = ["redact", &input, "--term", "Fixture", "--dpi", "100", "--out", &ps(&out)];
    let r = cli_session(&args, Options::default(), || session_with(pdfium.get(), Arc::new(SeesThroughBlackBoxes), &dir));
    assert_eq!(r.code, 3, "{}", r.stderr);
    assert_eq!(r.json()["verified"], false);
    assert!(r.stderr.contains("not verified"), "{}", r.stderr);
    assert!(!out.exists());

    let script = dir.file(
        "s.json",
        json!([
            { "tool": "doc_open", "args": { "path": input } },
            { "tool": "redact_apply", "args": { "doc": 1, "terms": ["Fixture"], "dpi": 100, "out": ps(&out) } },
            { "tool": "doc_close", "args": { "doc": 1 } }
        ])
        .to_string()
        .as_bytes(),
    );
    let r = cli_session(&["run", "--script", &ps(&script)], Options::default(), || {
        session_with(pdfium.get(), Arc::new(SeesThroughBlackBoxes), &dir)
    });
    assert_eq!(r.code, 3, "{}", r.stderr);
    assert_eq!(r.json().as_array().unwrap().len(), 2, "the script stops at the unverified step");
}

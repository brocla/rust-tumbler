//! `tumbler-cli`'s command line: `tools`, `run`, `run --script`, `mcp`, and
//! the friendly subcommands (`info`, `text`, `edit`, ...).
//!
//! Every friendly command is a short sequence of tool calls through the same
//! table as `run` and the MCP server, never separate logic, so it cannot drift
//! from them.
//!
//! stdout carries only the result: one JSON document, or the text for `text`.
//! Progress and errors go to stderr. Exit codes: 0 success; 1 the tool ran and
//! failed; 2 a usage or argument error. 3 is reserved for v1.1's redaction
//! ("finished but not verified").

use super::{tools, Automation, Content, Progress, ToolError};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub type SharedWriter = Arc<Mutex<dyn Write + Send>>;

/// The process's standard streams, injectable so tests run the CLI in-process.
pub struct Io {
    pub stdin: Box<dyn BufRead + Send>,
    pub stdout: SharedWriter,
    pub stderr: SharedWriter,
    /// Progress is shown on a terminal, or anywhere with --progress.
    pub stderr_is_terminal: bool,
}

/// What the CLI reads from its environment, injectable for the same reason.
#[derive(Default, Clone)]
pub struct CliEnv {
    /// `TUMBLER_PDF_PASSWORD`: opens password-protected inputs.
    pub password: Option<String>,
    /// `TUMBLER_PDF_NEW_PASSWORD`: the password `edit --set-password` sets.
    pub new_password: Option<String>,
    /// The registry key `__path` edits, in place of the real per-user
    /// environment, so tests never touch the real PATH.
    #[cfg(test)]
    pub path_key: Option<String>,
}

impl CliEnv {
    pub fn from_process() -> Self {
        Self {
            password: std::env::var("TUMBLER_PDF_PASSWORD").ok(),
            new_password: std::env::var("TUMBLER_PDF_NEW_PASSWORD").ok(),
            #[cfg(test)]
            path_key: None,
        }
    }
}

pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;

#[derive(Parser)]
#[command(
    name = "tumbler-cli",
    version,
    about = "Tumbler's PDF engine from the command line.",
    long_about = "Tumbler's PDF engine from the command line.\n\n\
        `run` calls one tool, or a script of tool calls, from the table `tools` lists. The \
        other commands are shortcuts built from those tools. Pages are 1-based. Passwords come \
        from --password-file or TUMBLER_PDF_PASSWORD, never the command line.\n\n\
        Exit codes: 0 success, 1 the operation failed, 2 a usage error."
)]
struct Cli {
    /// Print progress lines to stderr even when it is not a terminal.
    #[arg(long, global = true)]
    progress: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone, Default)]
struct Common {
    /// Confine every path read or written to this folder.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,
    /// Read the password for a protected input from this file.
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// List the tools, with their argument schemas under --json.
    Tools {
        /// Every tool's name, description, flags and JSON Schema, as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Call one tool (key=value arguments), or run a script of calls in one session.
    Run {
        /// The tool to call (see `tools`).
        tool: Option<String>,
        /// Arguments as key=value; values parse as JSON when they can, else as strings.
        args: Vec<String>,
        /// A JSON file: [{"tool": ..., "args": {...}, "out": "image.png"?}, ...].
        #[arg(long, value_name = "FILE", conflicts_with_all = ["tool", "args"])]
        script: Option<PathBuf>,
        /// Where to write an image result.
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Serve the tools to an agent over MCP (stdin/stdout) until stdin closes.
    #[command(long_about = "Serve the tools to an agent over the Model Context Protocol, on         stdin/stdout, until stdin closes. Opens no network port.

        --root is required: every path the agent reads or writes must be inside that folder.")]
    Mcp {
        /// The folder every path must be inside (required).
        #[arg(long, value_name = "DIR", required = true)]
        root: PathBuf,
    },
    /// Installer support (NSIS hooks): add a folder to, or remove it from, the
    /// per-user PATH, editing the registry value whole.
    #[command(name = "__path", hide = true)]
    UserPath {
        #[arg(value_parser = ["add", "remove"])]
        action: String,
        /// The folder, as an absolute path.
        dir: PathBuf,
    },
    /// Print a PDF's summary (doc_info) as JSON.
    Info {
        file: String,
        #[command(flatten)]
        common: Common,
    },
    /// Print a PDF's text, pages separated by form feeds.
    Text {
        file: String,
        /// Only this page (1-based).
        #[arg(long)]
        page: Option<u32>,
        /// Recognize pages without a text layer first (needs an OCR language pack).
        #[arg(long)]
        ocr: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Find text and print the matches as JSON.
    Find {
        file: String,
        query: String,
        /// Match case exactly.
        #[arg(long)]
        case: bool,
        /// Match whole words only.
        #[arg(long)]
        word: bool,
        /// Treat the query as a regular expression.
        #[arg(long)]
        regex: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Render one page to a PNG file.
    Render {
        file: String,
        /// The page (1-based).
        #[arg(long)]
        page: u32,
        /// Resolution in dots per inch (1-600).
        #[arg(long, default_value_t = 150.0)]
        dpi: f64,
        /// The PNG file to write.
        #[arg(long, value_name = "FILE")]
        out: String,
        #[command(flatten)]
        common: Common,
    },
    /// Edit pages, metadata or the password, and save to --out.
    #[command(long_about = "Edit pages, metadata or the password, and save to --out.\n\n\
        Page numbers in --rotate and --delete refer to the input. Rotation is applied before \
        deletion. --order cannot be combined with --delete.")]
    Edit {
        input: String,
        /// Where to save the result (may be the input itself).
        #[arg(long, value_name = "FILE")]
        out: String,
        /// PAGES:DEGREES, such as 1,3:90 or all:-90. Repeatable.
        #[arg(long, value_name = "PAGES:DEGREES")]
        rotate: Vec<String>,
        /// Pages to delete, such as 2,4-6.
        #[arg(long, value_name = "PAGES")]
        delete: Option<String>,
        /// The complete new page order, such as 3,1,2.
        #[arg(long, value_name = "PAGES", conflicts_with = "delete")]
        order: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        keywords: Option<String>,
        /// Protect the output with the password in --new-password-file or TUMBLER_PDF_NEW_PASSWORD.
        #[arg(long, conflicts_with = "remove_password")]
        set_password: bool,
        /// Read the password --set-password sets from this file.
        #[arg(long, value_name = "FILE", requires = "set_password")]
        new_password_file: Option<PathBuf>,
        /// Save the output without a password.
        #[arg(long)]
        remove_password: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Join PDFs, in order, into one file.
    Combine {
        /// Two or more PDF files.
        #[arg(num_args = 2.., required = true)]
        files: Vec<String>,
        #[arg(long, value_name = "FILE")]
        out: String,
        #[command(flatten)]
        common: Common,
    },
    /// Split a PDF into files of N pages, or of the given ranges.
    Split {
        file: String,
        /// The existing folder to write into.
        #[arg(long, value_name = "DIR")]
        out_dir: String,
        /// Pages per file (default 1).
        #[arg(long, conflicts_with = "ranges")]
        every: Option<u32>,
        /// Ranges, one file each, such as 1-3,4,5-9.
        #[arg(long)]
        ranges: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Make scanned pages searchable (OCR text layer) and save to --out.
    Ocr {
        file: String,
        #[arg(long, value_name = "FILE")]
        out: String,
        /// Also re-recognize pages that already have (junk) text.
        #[arg(long)]
        force: bool,
        /// Only these pages, such as 1,3-5.
        #[arg(long, value_name = "PAGES")]
        pages: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Reduce file size and save to --out.
    Compress {
        file: String,
        #[arg(long, value_name = "FILE")]
        out: String,
        /// Comma-separated steps (default: the four lossless ones). recompress_images is lossy.
        #[arg(long, value_name = "STEPS")]
        steps: Option<String>,
        /// recompress_images: target resolution (50-300).
        #[arg(long)]
        dpi: Option<f32>,
        /// recompress_images: JPEG quality (50-95).
        #[arg(long)]
        quality: Option<u8>,
        #[command(flatten)]
        common: Common,
    },
    /// Write a linearized (Fast Web View) copy to --out.
    Linearize {
        file: String,
        #[arg(long, value_name = "FILE")]
        out: String,
        #[command(flatten)]
        common: Common,
    },
    /// Scale content up to fill the page margins, and save to --out.
    ExpandMargins {
        file: String,
        #[arg(long, value_name = "FILE")]
        out: String,
        /// Space to leave at the page edge, in points (0-36; default 18).
        #[arg(long)]
        padding: Option<f32>,
        #[command(flatten)]
        common: Common,
    },
}

/// Why a command stopped.
enum Stop {
    Usage(String),
    Failed(String),
    /// stdout was closed (`| head`): not an error.
    ClosedPipe,
}

impl From<ToolError> for Stop {
    fn from(e: ToolError) -> Self {
        match e {
            ToolError::UnknownTool(_) | ToolError::InvalidArgs(_) => Stop::Usage(e.to_string()),
            ToolError::Failed(m) => Stop::Failed(m),
        }
    }
}

type Outcome = Result<(), Stop>;

/// Runs the CLI. `args` includes the program name. `session` builds the
/// automation session (binding pdfium) only for commands that need one.
pub fn main_with(
    args: Vec<OsString>,
    env: CliEnv,
    io: Io,
    cancel: Arc<AtomicBool>,
    session: impl FnOnce() -> Result<Automation, String>,
) -> u8 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // --help and --version are "errors" to clap, but successes here.
            let code = if e.use_stderr() { EXIT_USAGE } else { EXIT_OK };
            let target = if e.use_stderr() { &io.stderr } else { &io.stdout };
            let _ = write!(lock(target), "{}", e.render());
            return code;
        }
    };
    let show_progress = cli.progress || io.stderr_is_terminal;
    let mut ctx = Ctx { env, io, cancel, show_progress };
    let outcome = ctx.dispatch(cli.command, session);
    match outcome {
        Ok(()) | Err(Stop::ClosedPipe) => EXIT_OK,
        Err(Stop::Usage(m)) => {
            ctx.error(&m);
            EXIT_USAGE
        }
        Err(Stop::Failed(m)) => {
            ctx.error(&m);
            EXIT_FAILED
        }
    }
}

fn lock(w: &SharedWriter) -> std::sync::MutexGuard<'_, dyn Write + Send + 'static> {
    w.lock().unwrap_or_else(|p| p.into_inner())
}

struct Ctx {
    env: CliEnv,
    io: Io,
    cancel: Arc<AtomicBool>,
    show_progress: bool,
}

impl Ctx {
    fn dispatch(&mut self, command: Command, session: impl FnOnce() -> Result<Automation, String>) -> Outcome {
        if let Command::Tools { json } = command {
            return self.tools(json);
        }
        if let Command::Mcp { root } = command {
            return self.mcp(&root, session);
        }
        if let Command::UserPath { action, dir } = command {
            return self.user_path(&action, &dir);
        }
        let common = match &command {
            Command::Tools { .. } | Command::Mcp { .. } | Command::UserPath { .. } => unreachable!("handled above"),
            Command::Run { common, .. }
            | Command::Info { common, .. }
            | Command::Text { common, .. }
            | Command::Find { common, .. }
            | Command::Render { common, .. }
            | Command::Edit { common, .. }
            | Command::Combine { common, .. }
            | Command::Split { common, .. }
            | Command::Ocr { common, .. }
            | Command::Compress { common, .. }
            | Command::Linearize { common, .. }
            | Command::ExpandMargins { common, .. } => common.clone(),
        };
        let password = self.password(&common)?;
        let mut auto = session().map_err(Stop::Failed)?;
        if let Some(root) = &common.root {
            auto = auto.with_root(root).map_err(|e| Stop::Usage(format!("--root {}: {e}", root.display())))?;
        }
        let mut s = Session { ctx: self, auto, password };
        match command {
            Command::Tools { .. } | Command::Mcp { .. } | Command::UserPath { .. } => unreachable!("handled above"),
            Command::Run { tool, args, script, out, .. } => match script {
                Some(script) => s.run_script(&script),
                None => {
                    let tool = tool.ok_or_else(|| Stop::Usage("run: name a tool (see `tumbler-cli tools`), or pass --script".into()))?;
                    s.run_one(&tool, &args, out.as_deref())
                }
            },
            Command::Info { file, .. } => {
                let doc = s.open(&file)?;
                let info = s.json("doc_info", json!({ "doc": doc }))?;
                s.ctx.print_json(&info)
            }
            Command::Text { file, page, ocr, .. } => {
                let doc = s.open(&file)?;
                let pages = page.map(|p| json!([p]));
                if ocr {
                    let mut args = json!({ "doc": doc, "embed": false });
                    if let Some(pages) = &pages {
                        args["pages"] = pages.clone();
                    }
                    s.json("ocr_recognize", args)?;
                }
                let mut args = json!({ "doc": doc });
                if let Some(pages) = pages {
                    args["pages"] = pages;
                }
                let result = s.json("text_extract", args)?;
                let texts: Vec<&str> =
                    result["pages"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect();
                s.ctx.print_text(&format!("{}\n", texts.join("\x0c")))
            }
            Command::Find { file, query, case, word, regex, .. } => {
                let doc = s.open(&file)?;
                let found = s.json(
                    "text_find",
                    json!({ "doc": doc, "query": query, "match_case": case, "whole_word": word, "regex": regex }),
                )?;
                s.ctx.print_json(&found)
            }
            Command::Render { file, page, dpi, out, .. } => {
                let doc = s.open(&file)?;
                let content = s.call("page_render", json!({ "doc": doc, "page": page, "dpi": dpi }))?;
                let shown = s.show(content, Some(&out))?;
                s.ctx.print_json(&shown)
            }
            Command::Edit {
                input,
                out,
                rotate,
                delete,
                order,
                title,
                author,
                subject,
                keywords,
                set_password,
                new_password_file,
                remove_password,
                ..
            } => {
                let new_password = if set_password {
                    Some(self_new_password(&s.ctx.env, new_password_file.as_deref())?)
                } else {
                    None
                };
                let rotations = rotate.iter().map(|r| parse_rotation(r)).collect::<Result<Vec<_>, _>>()?;
                let delete = delete.as_deref().map(parse_pages).transpose().map_err(Stop::Usage)?;
                let order = order.as_deref().map(parse_pages).transpose().map_err(Stop::Usage)?;
                let mut info = Map::new();
                for (key, value) in [("title", title), ("author", author), ("subject", subject), ("keywords", keywords)] {
                    if let Some(v) = value {
                        info.insert(key.into(), json!(v));
                    }
                }
                if rotations.is_empty() && delete.is_none() && order.is_none() && info.is_empty() && !set_password && !remove_password {
                    return Err(Stop::Usage("edit: nothing to do (see `tumbler-cli edit --help`)".into()));
                }

                let doc = s.open(&input)?;
                let mut last = Value::Null;
                for (pages, degrees) in rotations {
                    let mut args = json!({ "doc": doc, "degrees": degrees });
                    if let Some(pages) = pages {
                        args["pages"] = json!(pages);
                    }
                    last = s.json("page_rotate", args)?;
                }
                if let Some(pages) = delete {
                    last = s.json("page_delete", json!({ "doc": doc, "pages": pages }))?;
                }
                if let Some(order) = order {
                    last = s.json("page_reorder", json!({ "doc": doc, "order": order }))?;
                }
                if !info.is_empty() {
                    info.insert("doc".into(), json!(doc));
                    last = s.json("doc_set_info", Value::Object(info))?;
                }
                if let Some(password) = new_password {
                    last = s.json("doc_protect", json!({ "doc": doc, "password": password }))?;
                }
                if remove_password {
                    last = s.json("doc_unprotect", json!({ "doc": doc }))?;
                }
                let saved = s.json("doc_save", json!({ "doc": doc, "path": out }))?;
                s.ctx.print_json(&merge(&last, &saved))
            }
            Command::Combine { files, out, .. } => {
                let doc = s.open(&files[0])?;
                let mut last = Value::Null;
                for file in &files[1..] {
                    last = s.json("page_insert_file", json!({ "doc": doc, "path": file }))?;
                }
                let saved = s.json("doc_save", json!({ "doc": doc, "path": out }))?;
                s.ctx.print_json(&merge(&last, &saved))
            }
            Command::Split { file, out_dir, every, ranges, .. } => {
                let doc = s.open(&file)?;
                let mut args = json!({ "doc": doc, "out_dir": out_dir });
                if let Some(every) = every {
                    args["every"] = json!(every);
                }
                if let Some(ranges) = ranges {
                    args["ranges"] = json!(parse_ranges(&ranges)?);
                }
                let result = s.json("doc_split", args)?;
                s.ctx.print_json(&result)
            }
            Command::Ocr { file, out, force, pages, .. } => {
                let doc = s.open(&file)?;
                let mut args = json!({ "doc": doc, "force": force });
                if let Some(pages) = pages {
                    args["pages"] = json!(parse_pages(&pages).map_err(Stop::Usage)?);
                }
                let result = s.json("ocr_recognize", args)?;
                let saved = s.json("doc_save", json!({ "doc": doc, "path": out }))?;
                s.ctx.print_json(&merge(&result, &saved))
            }
            Command::Compress { file, out, steps, dpi, quality, .. } => {
                let doc = s.open(&file)?;
                let mut args = json!({ "doc": doc });
                if let Some(steps) = steps {
                    let steps: Vec<&str> = steps.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
                    args["steps"] = json!(steps);
                }
                if let Some(dpi) = dpi {
                    args["dpi"] = json!(dpi);
                }
                if let Some(quality) = quality {
                    args["quality"] = json!(quality);
                }
                let result = s.json("doc_optimize", args)?;
                let saved = s.json("doc_save", json!({ "doc": doc, "path": out }))?;
                s.ctx.print_json(&merge(&result, &saved))
            }
            Command::Linearize { file, out, .. } => {
                let doc = s.open(&file)?;
                let result = s.json("doc_linearize", json!({ "doc": doc, "path": out }))?;
                s.ctx.print_json(&result)
            }
            Command::ExpandMargins { file, out, padding, .. } => {
                let doc = s.open(&file)?;
                let mut args = json!({ "doc": doc });
                if let Some(padding) = padding {
                    args["padding"] = json!(padding);
                }
                let result = s.json("margins_expand", args)?;
                let saved = s.json("doc_save", json!({ "doc": doc, "path": out }))?;
                s.ctx.print_json(&merge(&result, &saved))
            }
        }
    }

    fn tools(&mut self, as_json: bool) -> Outcome {
        if as_json {
            let list: Vec<Value> = tools()
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "title": t.title,
                        "description": t.description,
                        "read_only": t.read_only,
                        "destructive": t.destructive,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            return self.print_json(&Value::Array(list));
        }
        let width = tools().iter().map(|t| t.name.len()).max().unwrap_or(0);
        let mut text = String::new();
        for t in tools() {
            let flag = if t.read_only { "  (read-only)" } else if t.destructive { "  (destructive)" } else { "" };
            text.push_str(&format!("{:width$}  {}{flag}\n", t.name, t.title));
        }
        text.push_str("\n`tumbler-cli tools --json` gives each tool's description and argument schema.\n");
        self.print_text(&text)
    }

    fn mcp(&mut self, root: &std::path::Path, session: impl FnOnce() -> Result<Automation, String>) -> Outcome {
        let auto = session()
            .map_err(Stop::Failed)?
            .with_root(root)
            .map_err(|e| Stop::Usage(format!("--root {}: {e}", root.display())))?;
        let root = auto.root().map(|r| r.display().to_string()).unwrap_or_default();
        let _ = writeln!(
            lock(&self.io.stderr),
            "tumbler-cli: MCP server on stdin/stdout (protocol {}), paths confined to {root}; close stdin to stop",
            super::mcp::PROTOCOL_VERSIONS[0]
        );
        let stdin = std::mem::replace(&mut self.io.stdin, Box::new(std::io::empty()));
        super::mcp::serve(auto, stdin, self.io.stdout.clone()).map_err(|e| Stop::Failed(format!("MCP server: {e}")))
    }

    fn user_path(&mut self, action: &str, dir: &std::path::Path) -> Outcome {
        if !dir.is_absolute() {
            return Err(Stop::Usage(format!("__path: {} is not an absolute path", dir.display())));
        }
        #[cfg(test)]
        let key = self.env.path_key.clone().unwrap_or_else(|| super::user_path::USER_ENVIRONMENT.to_string());
        #[cfg(not(test))]
        let key = super::user_path::USER_ENVIRONMENT.to_string();
        let changed = match action {
            "add" => super::user_path::add(&key, dir),
            _ => super::user_path::remove(&key, dir),
        }
        .map_err(Stop::Failed)?;
        self.print_json(&json!({ "action": action, "dir": dir.to_string_lossy(), "changed": changed }))
    }

    /// The password for protected inputs: --password-file, else the
    /// environment.
    fn password(&self, common: &Common) -> Result<Option<String>, Stop> {
        match &common.password_file {
            Some(file) => read_secret(file).map(Some),
            None => Ok(self.env.password.clone()),
        }
    }

    fn print_json(&mut self, value: &Value) -> Outcome {
        let text = serde_json::to_string_pretty(value).map_err(|e| Stop::Failed(e.to_string()))?;
        self.print_text(&format!("{text}\n"))
    }

    fn print_text(&mut self, text: &str) -> Outcome {
        let mut out = lock(&self.io.stdout);
        match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(Stop::ClosedPipe),
            Err(e) => Err(Stop::Failed(format!("writing the result: {e}"))),
        }
    }

    fn error(&mut self, message: &str) {
        let _ = writeln!(lock(&self.io.stderr), "tumbler-cli: {message}");
    }

    fn progress(&self) -> Progress {
        if !self.show_progress {
            return Arc::new(|_, _| {});
        }
        let stderr = self.io.stderr.clone();
        Arc::new(move |done, total| {
            let _ = writeln!(lock(&stderr), "page {done}/{total}");
        })
    }
}

/// A command's open session.
struct Session<'a> {
    ctx: &'a mut Ctx,
    auto: Automation,
    password: Option<String>,
}

impl Session<'_> {
    fn call(&mut self, tool: &str, args: Value) -> Result<Vec<Content>, Stop> {
        let progress = self.ctx.progress();
        self.auto.call_with(tool, &args, progress, self.ctx.cancel.clone()).map_err(Stop::from)
    }

    /// A call whose result is one JSON document.
    fn json(&mut self, tool: &str, args: Value) -> Result<Value, Stop> {
        let content = self.call(tool, args)?;
        self.show(content, None)
    }

    /// Opens `file`, with the password if there is one, and returns its id.
    fn open(&mut self, file: &str) -> Result<u64, Stop> {
        let mut args = json!({ "path": file });
        if let Some(password) = &self.password {
            args["password"] = json!(password);
        }
        let opened = self.json("doc_open", args).map_err(|e| match e {
            Stop::Failed(m) if m.contains("password-protected") => Stop::Failed(format!(
                "{m}: give it with --password-file FILE or the TUMBLER_PDF_PASSWORD environment variable"
            )),
            other => other,
        })?;
        opened["doc"].as_u64().ok_or_else(|| Stop::Failed("doc_open returned no id".into()))
    }

    /// A tool result as JSON. An image is written to `out` when given (like a
    /// tool's own output: confined, atomic), or summarized.
    fn show(&mut self, content: Vec<Content>, out: Option<&str>) -> Result<Value, Stop> {
        let mut shown = Vec::with_capacity(content.len());
        for c in content {
            shown.push(match c {
                Content::Json(v) => v,
                Content::Png { data, width, height } => match out {
                    Some(path) => {
                        let written = self.auto.write_output(path, &data)?;
                        json!({ "image": written.to_string_lossy(), "width": width, "height": height, "bytes": data.len() })
                    }
                    None => json!({
                        "image": "png", "width": width, "height": height, "bytes": data.len(),
                        "hint": "pass --out file.png (or \"out\" in a script step) to save it",
                    }),
                },
            });
        }
        Ok(if shown.len() == 1 { shown.remove(0) } else { Value::Array(shown) })
    }

    fn run_one(&mut self, tool: &str, args: &[String], out: Option<&str>) -> Outcome {
        let mut obj = Map::new();
        for kv in args {
            let (key, value) = parse_kv(kv).map_err(Stop::Usage)?;
            if key == "password" {
                return Err(Stop::Usage(
                    "run: don't put passwords on the command line; use --password-file FILE or the \
                     TUMBLER_PDF_PASSWORD environment variable"
                        .into(),
                ));
            }
            obj.insert(key, value);
        }
        // Passwords come from a file or the environment: the one that opens a
        // file for doc_open, the one to set for doc_protect.
        let password = match tool {
            "doc_open" => self.password.clone(),
            "doc_protect" => self.ctx.env.new_password.clone(),
            _ => None,
        };
        if let Some(password) = password {
            obj.insert("password".into(), json!(password));
        }
        let content = self.call(tool, Value::Object(obj))?;
        let shown = self.show(content, out)?;
        self.ctx.print_json(&shown)
    }

    fn run_script(&mut self, script: &std::path::Path) -> Outcome {
        let text = std::fs::read_to_string(script).map_err(|e| Stop::Usage(format!("{}: {e}", script.display())))?;
        let steps: Vec<Value> =
            serde_json::from_str(&text).map_err(|e| Stop::Usage(format!("{}: not a JSON list of steps: {e}", script.display())))?;
        let mut done = Vec::with_capacity(steps.len());
        let mut stop = None;
        for (i, step) in steps.iter().enumerate() {
            let n = i + 1;
            let Some(tool) = step["tool"].as_str() else {
                stop = Some(Stop::Usage(format!("step {n}: missing \"tool\"")));
                break;
            };
            let result = self
                .call(tool, step.get("args").cloned().unwrap_or(Value::Null))
                .and_then(|content| self.show(content, step["out"].as_str()));
            match result {
                Ok(result) => done.push(json!({ "step": n, "tool": tool, "result": result })),
                Err(Stop::Usage(m)) => {
                    stop = Some(Stop::Usage(format!("step {n} ({tool}): {m}")));
                    break;
                }
                Err(Stop::Failed(m)) => {
                    stop = Some(Stop::Failed(format!("step {n} ({tool}): {m}")));
                    break;
                }
                Err(Stop::ClosedPipe) => return Err(Stop::ClosedPipe),
            }
        }
        // The steps that completed are printed either way, so a failure
        // midway still shows what was done.
        self.ctx.print_json(&Value::Array(done))?;
        stop.map_or(Ok(()), Err)
    }
}

fn self_new_password(env: &CliEnv, file: Option<&std::path::Path>) -> Result<String, Stop> {
    match file {
        Some(file) => read_secret(file),
        None => env.new_password.clone().ok_or_else(|| {
            Stop::Usage(
                "edit --set-password: give the new password with --new-password-file FILE or the \
                 TUMBLER_PDF_NEW_PASSWORD environment variable"
                    .into(),
            )
        }),
    }
}

/// A password file's contents, without the line ending an editor adds.
fn read_secret(file: &std::path::Path) -> Result<String, Stop> {
    let text = std::fs::read_to_string(file).map_err(|e| Stop::Usage(format!("{}: {e}", file.display())))?;
    let secret = text.trim_end_matches(['\r', '\n']).to_string();
    if secret.is_empty() {
        return Err(Stop::Usage(format!("{}: the password file is empty", file.display())));
    }
    Ok(secret)
}

/// `a`'s fields, then `b`'s on top.
fn merge(a: &Value, b: &Value) -> Value {
    let mut out = a.as_object().cloned().unwrap_or_default();
    if let Some(b) = b.as_object() {
        out.extend(b.clone());
    }
    Value::Object(out)
}

/// `key=value`, the value parsed as JSON when it can be, else a string.
pub(crate) fn parse_kv(kv: &str) -> Result<(String, Value), String> {
    let (key, value) = kv.split_once('=').ok_or_else(|| format!("run: expected key=value, got {kv:?}"))?;
    let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
    Ok((key.to_string(), value))
}

/// A page list such as `1,3,5-7`.
pub(crate) fn parse_pages(list: &str) -> Result<Vec<u32>, String> {
    let bad = || format!("{list:?} is not a page list such as 1,3,5-7");
    let mut pages = Vec::new();
    for part in list.split(',') {
        let (first, last) = parse_range(part.trim()).ok_or_else(bad)?;
        pages.extend(first..=last);
    }
    Ok(pages)
}

/// Ranges such as `1-3,4,5-9`, as doc_split's {first, last} objects.
fn parse_ranges(list: &str) -> Result<Vec<Value>, Stop> {
    list.split(',')
        .map(|part| {
            parse_range(part.trim())
                .map(|(first, last)| json!({ "first": first, "last": last }))
                .ok_or_else(|| Stop::Usage(format!("{list:?} is not a range list such as 1-3,4,5-9")))
        })
        .collect()
}

fn parse_range(part: &str) -> Option<(u32, u32)> {
    let number = |s: &str| s.trim().parse::<u32>().ok().filter(|n| *n >= 1);
    let (first, last) = match part.split_once('-') {
        Some((a, b)) => (number(a)?, number(b)?),
        None => (number(part)?, number(part)?),
    };
    (first <= last).then_some((first, last))
}

/// `PAGES:DEGREES` (`1,3:90`, `all:-90`): `None` pages means every page.
fn parse_rotation(spec: &str) -> Result<(Option<Vec<u32>>, i32), Stop> {
    let bad = |why: &str| Stop::Usage(format!("--rotate {spec:?}: {why} (use PAGES:DEGREES, such as 1,3:90 or all:-90)"));
    let (pages, degrees) = spec.rsplit_once(':').ok_or_else(|| bad("missing ':'"))?;
    let degrees: i32 = degrees.trim().parse().map_err(|_| bad("degrees is not a number"))?;
    if degrees % 90 != 0 {
        return Err(bad("degrees must be a multiple of 90"));
    }
    let pages = match pages.trim() {
        "all" => None,
        list => Some(parse_pages(list).map_err(Stop::Usage)?),
    };
    Ok((pages, degrees))
}

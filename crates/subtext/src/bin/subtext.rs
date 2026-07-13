//! `subtext` CLI — a thin wrapper over [`subtext::check_pdf`]: bind pdfium
//! once, parse args, run the checker per file, render (human by default,
//! `--json` for the machine report), and exit non-zero when a leak is found.

use clap::Parser;
use pdfium_render::prelude::Pdfium;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use subtext::report::{CheckStatus, Report, RiskTone, SignalKind};
use subtext::{CheckOptions, Query};

/// Subtext — A Redaction Checker. Report every place a term still appears
/// across a PDF's full leak-vector inventory.
#[derive(Parser, Debug)]
#[command(name = "subtext", version, about, long_about = None)]
struct Cli {
    /// A term to search for. Repeat for a list; mutually exclusive with --regex.
    #[arg(long = "term", value_name = "WORD")]
    terms: Vec<String>,

    /// A regular expression to search for (single pattern).
    #[arg(long = "regex", value_name = "PATTERN", conflicts_with = "terms")]
    regex: Option<String>,

    /// Case-sensitive matching (default: insensitive).
    #[arg(long)]
    case_sensitive: bool,

    /// Whole-word matching only.
    #[arg(long)]
    whole_word: bool,

    /// Password for encrypted input files (applied to every FILE given).
    #[arg(long, value_name = "PASSWORD")]
    password: Option<String>,

    /// Recurse into embedded PDFs (attachments), scanning each with the full
    /// vector set (depth-capped).
    #[arg(long)]
    recurse_embedded: bool,

    /// Run the rendered-image OCR pass (recovers image-of-text). Requires a
    /// build compiled with `--features ocr`; otherwise the pass is unavailable.
    #[arg(long)]
    ocr: bool,

    /// Emit the machine-readable JSON report instead of the human summary.
    #[arg(long)]
    json: bool,

    /// One or more PDF files to check (one report each).
    #[arg(value_name = "FILE", required = true)]
    files: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let query = match build_query(&cli) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("subtext: {e}");
            return ExitCode::from(2);
        }
    };

    // Bind pdfium once for the whole process (it can be bound only once).
    let pdfium = match bind_pdfium() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("subtext: {e}");
            return ExitCode::from(2);
        }
    };

    let options = CheckOptions {
        password: cli.password.clone(),
        recurse_embedded: cli.recurse_embedded,
        ocr: cli.ocr,
    };

    let mut any_error = false;
    let mut reports: Vec<Report> = Vec::with_capacity(cli.files.len());
    for file in &cli.files {
        let bytes = match std::fs::read(file) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("subtext: cannot read {}: {e}", file.display());
                any_error = true;
                continue;
            }
        };
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.display().to_string());
        reports.push(subtext::check_pdf(&pdfium, &bytes, &name, &query, &options));
    }

    if cli.json {
        // One report → a bare object (the §3.2 shape); multiple → a JSON array,
        // so a batch is still a single valid JSON document.
        let json = if reports.len() == 1 {
            serde_json::to_string_pretty(&reports[0])
        } else {
            serde_json::to_string_pretty(&reports)
        };
        match json {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("subtext: failed to serialize report: {e}");
                any_error = true;
            }
        }
    } else {
        for (i, report) in reports.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_human(report);
        }
        if reports.len() > 1 {
            print_batch_summary(&reports);
        }
    }

    // Exit code: 1 = a leak was found, 2 = an error, 0 = clean/warning only.
    let any_leak = reports.iter().any(|r| r.risk_tone == RiskTone::Leak);
    if any_error {
        ExitCode::from(2)
    } else if any_leak {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// A one-line tally across a multi-file run: how many files leaked, warned, or
/// came back clean.
fn print_batch_summary(reports: &[Report]) {
    let leaks = reports.iter().filter(|r| r.risk_tone == RiskTone::Leak).count();
    let warnings = reports.iter().filter(|r| r.risk_tone == RiskTone::Warning).count();
    let clean = reports.iter().filter(|r| r.risk_tone == RiskTone::Clean).count();
    println!(
        "\n{} files: {leaks} leak, {warnings} warning, {clean} clean",
        reports.len()
    );
}

fn build_query(cli: &Cli) -> Result<Query, String> {
    match &cli.regex {
        Some(pattern) => Query::regex(pattern.clone(), cli.case_sensitive, cli.whole_word),
        None if !cli.terms.is_empty() => {
            Query::literal(cli.terms.clone(), cli.case_sensitive, cli.whole_word)
        }
        None => Err("provide at least one --term or a --regex pattern".to_string()),
    }
}

/// Binds pdfium.dll. Reuses Tumbler's dev/bundled resolution order: check
/// `src-tauri/resources/pdfium.dll` (dev), then alongside the executable, then
/// let pdfium try the system library.
fn bind_pdfium() -> Result<Pdfium, String> {
    let candidates = pdfium_candidates();
    for path in &candidates {
        if let Ok(bindings) = Pdfium::bind_to_library(path) {
            return Ok(Pdfium::new(bindings));
        }
    }
    // Last resort: the system-installed library on PATH.
    Pdfium::bind_to_system_library()
        .map(Pdfium::new)
        .map_err(|e| {
            format!(
                "could not load pdfium (tried {}, then the system library): {e}",
                candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn pdfium_candidates() -> Vec<PathBuf> {
    let lib = Pdfium::pdfium_platform_library_name_at_path("./");
    let file = Path::new(&lib)
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&lib));
    let mut out = Vec::new();
    // Dev layout: the checked-out Tumbler tree.
    out.push(Path::new("src-tauri/resources").join(&file));
    // Alongside the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(&file));
            out.push(dir.join("resources").join(&file));
        }
    }
    // Current directory.
    out.push(PathBuf::from(&file));
    out
}

/// The default human report: the verdict, then the checks list ("all the ways
/// it was checked"), then the findings ("where the words were found").
fn print_human(report: &Report) {
    let tone = match report.risk_tone {
        RiskTone::Leak => "LEAK",
        RiskTone::Warning => "WARNING",
        RiskTone::Clean => "CLEAN",
    };
    println!("{} — {}", report.file_name, tone);
    println!("  {}", report.title);
    println!("  {}", report.description);
    println!("  risk: {:?} · pages: {} · size: {} bytes", report.risk_score, report.pages, report.file_size);

    let leaks = report.checks.iter().filter(|c| c.status == CheckStatus::Found).count();
    let clean = report.checks.iter().filter(|c| c.status == CheckStatus::CheckedClean).count();
    let skipped = report.checks.iter().filter(|c| c.status == CheckStatus::Skipped).count();
    println!(
        "\n  Checks ({} vectors — {clean} clean, {skipped} skipped, {leaks} with matches):",
        report.checks.len()
    );
    for c in &report.checks {
        let mark = match c.status {
            CheckStatus::Found => "LEAK",
            CheckStatus::CheckedClean => "ok  ",
            CheckStatus::Skipped => "skip",
        };
        println!("    [{mark}] {:<22} {}", c.label, c.detail);
    }

    if !report.findings.is_empty() {
        println!("\n  Findings:");
        for f in &report.findings {
            // Prefix the embedded-container path (--recurse-embedded) so a hit
            // inside an attachment names where it came from; revision-stamped
            // findings already carry the revision in their location text.
            let location = match &f.container {
                Some(container) => format!("{container} · {}", f.location),
                None => f.location.clone(),
            };
            println!("    • {} — \"{}\"", location, f.matched_text);
            if !f.context.is_empty() {
                println!("        {}", f.context);
            }
        }
    }

    if !report.signals.is_empty() {
        println!("\n  Signals:");
        for s in &report.signals {
            println!("    ! [{}] {} — {}", signal_label(s.kind), s.location, s.detail);
        }
    }
}

/// A short human label for a query-independent signal's kind.
fn signal_label(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::UnappliedRedactAnnotation => "unapplied redaction",
        SignalKind::RenderExtractMismatch => "render/extract mismatch",
        SignalKind::SubDocumentNotInspected => "sub-document not inspected",
    }
}

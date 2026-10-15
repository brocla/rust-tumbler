//! Runs the real `tumbler-cli.exe`, for what the in-process tests in
//! `automation::tests_cli` cannot reach: loading pdfium.dll, and the console.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn tumbler_cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tumbler-cli"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf")
}

/// An empty folder under the temp folder, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tumbler-cli-exe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn prints_its_version() {
    let out = tumbler_cli().arg("--version").output().expect("run tumbler-cli");
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), concat!("tumbler-cli ", env!("CARGO_PKG_VERSION")));
}

#[test]
fn a_usage_error_exits_2() {
    let out = tumbler_cli().arg("--no-such-flag").output().expect("run tumbler-cli");
    assert_eq!(out.status.code(), Some(2));
}

/// Loads pdfium relative to the executable, so it works from any folder.
#[test]
fn reads_a_pdf_from_another_working_directory() {
    let cwd = TempDir::new("cwd");
    let out = tumbler_cli().arg("info").arg(fixture()).current_dir(&cwd.0).output().expect("run tumbler-cli");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let info: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("JSON on stdout");
    assert_eq!(info["page_count"], 1);
}

/// A `resources\pdfium.dll` in the working directory is never loaded (DLL
/// planting). An empty file there would fail to load if it were tried.
#[test]
fn ignores_a_pdfium_dll_planted_in_the_working_directory() {
    let cwd = TempDir::new("plant");
    std::fs::create_dir(cwd.0.join("resources")).unwrap();
    std::fs::write(cwd.0.join("resources").join("pdfium.dll"), b"").unwrap();
    std::fs::write(cwd.0.join("pdfium.dll"), b"").unwrap();
    let out = tumbler_cli().arg("text").arg(fixture()).current_dir(&cwd.0).output().expect("run tumbler-cli");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("Test Fixture"));
}

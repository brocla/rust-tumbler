//! `tumbler-cli`: Tumbler's command line and MCP server (doc/cli-plan.html).
//! A console program: the GUI (`tumbler.exe`) is a windows-subsystem binary
//! with no stdin/stdout in release builds. Everything but process setup lives
//! in `tumbler_lib::automation::cli`.

use pdfium_render::prelude::Pdfium;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tumbler_lib::automation::{cli, Automation};

/// Set by Ctrl+C; the running tool checks it between pages.
static CANCEL: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn main() -> ExitCode {
    let cancel = CANCEL.get_or_init(Arc::default).clone();
    install_ctrl_c_handler();
    let io = cli::Io {
        stdin: Box::new(std::io::BufReader::new(std::io::stdin())),
        stdout: Arc::new(Mutex::new(std::io::stdout())),
        stderr: Arc::new(Mutex::new(std::io::stderr())),
        stderr_is_terminal: std::io::stderr().is_terminal(),
    };
    let code = cli::main_with(std::env::args_os().collect(), cli::CliEnv::from_process(), io, cancel, || {
        // pdfium is bound once and lives for the process, as in the GUI.
        let path = tumbler_lib::resolve_pdfium_path();
        let bindings =
            Pdfium::bind_to_library(&path).map_err(|e| format!("cannot load pdfium.dll from {path}: {e}"))?;
        Ok(Automation::with_pdfium(Box::leak(Box::new(Pdfium::new(bindings)))))
    });
    ExitCode::from(code)
}

/// The first Ctrl+C cancels the running tool, which stops after its current
/// page and reports "cancelled"; a second ends the process at once.
fn install_ctrl_c_handler() {
    use windows::core::BOOL;
    use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT};

    unsafe extern "system" fn handler(event: u32) -> BOOL {
        if event != CTRL_C_EVENT && event != CTRL_BREAK_EVENT {
            return BOOL(0);
        }
        match CANCEL.get() {
            Some(cancel) if !cancel.swap(true, Ordering::Relaxed) => BOOL(1),
            // Already cancelled once: let Windows end the process.
            _ => BOOL(0),
        }
    }
    // Without a console (output redirected by a parent, say) this fails, and
    // Ctrl+C has nothing to deliver anyway.
    let _ = unsafe { SetConsoleCtrlHandler(Some(handler), true) };
}

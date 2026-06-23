//! Registers the tumbler-thumbnailer.dll COM thumbnail provider in HKCU at
//! app startup so Explorer shows first-page PDF previews without any manual
//! regsvr32 step. Writes only when the current registration is absent or
//! points to a different path (e.g. after the app was moved or reinstalled).

use std::path::PathBuf;

#[cfg(target_os = "windows")]
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_QUERY_VALUE, REG_SZ, REG_VALUE_TYPE,
};
#[cfg(target_os = "windows")]
use windows::core::PCWSTR;

const CLSID: &str = "{0E3D8445-C38C-45EE-9FD3-A5EA0B089DEE}";
const THUMBNAIL_HANDLER_IID: &str = "{e357fccd-a995-4576-b01f-234630154e96}";

/// Called from `lib.rs::run()` before the Tauri window is built.
/// Registers the thumbnail handler only when Tumbler is the default PDF app,
/// so we don't clobber another app's handler when the user opens Tumbler
/// occasionally while something else (e.g. Acrobat, Foxit) is the default.
pub fn ensure_registered() {
    #[cfg(target_os = "windows")]
    {
        if !tumbler_is_default_pdf_handler() {
            return;
        }
        if let Some(dll_path) = find_thumbnailer_dll() {
            let dll_str = dll_path.to_string_lossy().into_owned();
            if !already_registered(&dll_str) {
                let _ = write_registration(&dll_str);
            }
        }
    }
}

/// Removes all HKCU entries written by `ensure_registered`. Called on uninstall
/// (via a Tauri plugin hook or the NSIS uninstaller).
#[allow(dead_code)]
pub fn unregister() {
    #[cfg(target_os = "windows")]
    {
        let clsid_key = format!("Software\\Classes\\CLSID\\{}", CLSID);
        let shlex_key = format!(
            "Software\\Classes\\.pdf\\ShellEx\\{}",
            THUMBNAIL_HANDLER_IID
        );
        let _ = reg_delete(HKEY_CURRENT_USER, &clsid_key);
        let _ = reg_delete(HKEY_CURRENT_USER, &shlex_key);
    }
}

/// Look for `tumbler-thumbnailer.dll` next to the running executable.
fn find_thumbnailer_dll() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent().map(PathBuf::from)?;
    let candidate = exe_dir.join("tumbler-thumbnailer.dll");
    candidate.exists().then_some(candidate)
}

/// Read a REG_SZ default value (or named value) from `root\subkey`.
/// Returns `None` if the key or value is absent or not a string type.
#[cfg(target_os = "windows")]
fn read_reg_sz(root: HKEY, subkey: &str, value_name: &str) -> Option<String> {
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let name_w: Vec<u16> = value_name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey = HKEY::default();
    unsafe {
        RegOpenKeyExW(root, PCWSTR(subkey_w.as_ptr()), None, KEY_QUERY_VALUE, &mut hkey)
    }
    .ok()
    .ok()?;
    let mut data = vec![0u8; 2048];
    let mut data_len = data.len() as u32;
    let mut kind = REG_VALUE_TYPE::default();
    let ok = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut kind as *mut REG_VALUE_TYPE),
            Some(data.as_mut_ptr()),
            Some(&mut data_len),
        )
    }
    .is_ok();
    let _ = unsafe { RegCloseKey(hkey) };
    if !ok || kind != REG_SZ {
        return None;
    }
    let chars = data_len as usize / 2;
    let words: Vec<u16> = data[..chars * 2]
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    Some(
        String::from_utf16_lossy(&words)
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// Returns true if Tumbler's executable is the registered handler for .pdf files.
///
/// Checks the Windows 10/11 UserChoice key first (the authoritative source set
/// by "Default Apps" settings), then falls back to the classic HKCU association.
/// Either way, resolves the ProgID's open command and compares it against our
/// own exe path so we don't accidentally match a ProgID that happens to contain
/// "Tumbler" in its name.
#[cfg(target_os = "windows")]
fn tumbler_is_default_pdf_handler() -> bool {
    fn check() -> Option<bool> {
        // Windows 10/11 UserChoice — authoritative default-app selection.
        let prog_id = read_reg_sz(
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\Shell\Associations\FileAssociations\.pdf\UserChoice",
            "ProgId",
        )
        // Older / pre-UserChoice path.
        .or_else(|| read_reg_sz(HKEY_CURRENT_USER, r"Software\Classes\.pdf", ""))?;

        // Resolve the open command for that ProgId.  Check HKCU first (per-user
        // installs like Tumbler), then HKCR (the merged HKCU+HKLM view) for
        // system-wide installs of other apps.
        let cmd_subkey = format!(r"Software\Classes\{}\shell\open\command", prog_id);
        let command = read_reg_sz(HKEY_CURRENT_USER, &cmd_subkey, "")
            .or_else(|| read_reg_sz(HKEY_CLASSES_ROOT, &cmd_subkey, ""))?;

        let exe = std::env::current_exe().ok()?;
        let exe_lower = exe.to_string_lossy().to_lowercase();
        Some(command.to_lowercase().contains(exe_lower.as_str()))
    }
    check().unwrap_or(false)
}

/// True if `InprocServer32` already points to `dll_path`.
#[cfg(target_os = "windows")]
fn already_registered(dll_path: &str) -> bool {
    let inproc = format!(
        "Software\\Classes\\CLSID\\{}\\InprocServer32",
        CLSID
    );
    let key_w: Vec<u16> = inproc.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey = HKEY::default();
    if unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut hkey,
        )
    }
    .is_err()
    {
        return false;
    }

    let name_w = [0u16]; // empty string = default value
    let mut data = vec![0u8; 1024];
    let mut data_len = data.len() as u32;
    let mut kind = REG_VALUE_TYPE::default();
    let ok = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut kind as *mut REG_VALUE_TYPE),
            Some(data.as_mut_ptr()),
            Some(&mut data_len),
        )
    }
    .is_ok();
    let _ = unsafe { RegCloseKey(hkey) };

    if !ok || kind != REG_SZ {
        return false;
    }
    let chars = data_len as usize / 2;
    let words: Vec<u16> = data[..chars * 2]
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let current = String::from_utf16_lossy(&words)
        .trim_end_matches('\0')
        .to_string();
    current.eq_ignore_ascii_case(dll_path)
}

#[cfg(target_os = "windows")]
fn write_registration(dll_path: &str) -> Result<(), windows::core::Error> {
    let clsid_key = format!("Software\\Classes\\CLSID\\{}", CLSID);
    reg_set(
        HKEY_CURRENT_USER,
        &clsid_key,
        "",
        "Tumbler PDF Thumbnail Provider",
    )?;
    let inproc_key = format!("{}\\InprocServer32", clsid_key);
    reg_set(HKEY_CURRENT_USER, &inproc_key, "", dll_path)?;
    reg_set(HKEY_CURRENT_USER, &inproc_key, "ThreadingModel", "Apartment")?;
    let shlex_key = format!(
        "Software\\Classes\\.pdf\\ShellEx\\{}",
        THUMBNAIL_HANDLER_IID
    );
    reg_set(HKEY_CURRENT_USER, &shlex_key, "", CLSID)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn reg_set(
    root: HKEY,
    subkey: &str,
    value_name: &str,
    data: &str,
) -> Result<(), windows::core::Error> {
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey = HKEY::default();
    // RegCreateKeyW: creates or opens the key; simpler than RegCreateKeyExW (no security param).
    unsafe { RegCreateKeyW(root, PCWSTR(subkey_w.as_ptr()), &mut hkey) }.ok()?;
    let name_w: Vec<u16> = value_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let data_w: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
    let data_bytes = unsafe {
        std::slice::from_raw_parts(data_w.as_ptr() as *const u8, data_w.len() * 2)
    };
    let result = unsafe {
        RegSetValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            REG_SZ,
            Some(data_bytes),
        )
    };
    unsafe { RegCloseKey(hkey) }.ok()?;
    result.ok()
}

#[cfg(target_os = "windows")]
fn reg_delete(root: HKEY, subkey: &str) -> Result<(), windows::core::Error> {
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegDeleteTreeW(root, PCWSTR(subkey_w.as_ptr())) }.ok()
}

//! tumbler-thumbnailer — Windows IThumbnailProvider COM in-process DLL.
//!
//! Explorer loads this DLL when generating thumbnail previews for .pdf files.
//! It renders page 1 via pdfium and returns a 32-bpp ARGB HBITMAP.
//!
//! Registration (HKCU, no admin required):
//!   regsvr32 tumbler-thumbnailer.dll          ← calls DllRegisterServer
//!   regsvr32 /u tumbler-thumbnailer.dll       ← calls DllUnregisterServer
//!
//! The main Tumbler.exe also writes the same HKCU keys at startup so the
//! user never needs to run regsvr32 manually.

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use pdfium_render::prelude::*;

use windows::core::{implement, IUnknown, Interface, GUID, HRESULT, PCWSTR};
use windows_core::BOOL;
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER, S_OK};
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, REG_SZ,
};
use windows::Win32::UI::Shell::{
    IThumbnailProvider, IThumbnailProvider_Impl, WTS_ALPHATYPE, WTSAT_ARGB,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithFile_Impl};

// ── CLSID ────────────────────────────────────────────────────────────────────

/// The stable COM class identifier for this thumbnail provider.
/// Must match the value registered in HKCU\Software\Classes\CLSID\{...}.
pub const CLSID_THUMBNAILER: GUID = GUID {
    data1: 0x0E3D8445,
    data2: 0xC38C,
    data3: 0x45EE,
    data4: [0x9F, 0xD3, 0xA5, 0xEA, 0x0B, 0x08, 0x9D, 0xEE],
};

/// IThumbnailProvider shell extension CLSID (Windows built-in interface).
const CLSID_THUMBNAIL_PROVIDER_HANDLER: &str =
    "{e357fccd-a995-4576-b01f-234630154e96}";

// ── DLL identity ─────────────────────────────────────────────────────────────

/// HMODULE of this DLL, stored in DllMain so we can resolve our own path.
static DLL_HMODULE: AtomicIsize = AtomicIsize::new(0);

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hinst: windows::Win32::Foundation::HMODULE,
    fdw_reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if fdw_reason == DLL_PROCESS_ATTACH {
        DLL_HMODULE.store(hinst.0 as isize, Ordering::Relaxed);
    }
    1 // TRUE
}

/// Absolute path to this DLL on disk.
fn dll_path() -> PathBuf {
    let hmod = windows::Win32::Foundation::HMODULE(
        DLL_HMODULE.load(Ordering::Relaxed) as *mut c_void,
    );
    let mut buf = [0u16; 32768];
    let len = unsafe { GetModuleFileNameW(Some(hmod), &mut buf) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
}

fn dll_dir() -> PathBuf {
    dll_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

// ── pdfium singleton ─────────────────────────────────────────────────────────

static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();

fn get_pdfium() -> Option<&'static Pdfium> {
    PDFIUM
        .get_or_init(|| {
            let dir = dll_dir();
            // Look for pdfium.dll next to this DLL, then in a resources/ subdir.
            let candidates = [dir.join("pdfium.dll"), dir.join("resources").join("pdfium.dll")];
            let path = candidates.iter().find(|p| p.exists())?;
            let bindings = Pdfium::bind_to_library(path).ok()?;
            Some(Pdfium::new(bindings))
        })
        .as_ref()
}

// ── COM object ───────────────────────────────────────────────────────────────

#[implement(IThumbnailProvider, IInitializeWithFile)]
struct TumblerThumbnailer {
    path: Mutex<Option<String>>,
}

impl TumblerThumbnailer {
    fn new() -> Self {
        Self {
            path: Mutex::new(None),
        }
    }
}

impl IInitializeWithFile_Impl for TumblerThumbnailer_Impl {
    fn Initialize(&self, pszfilepath: &PCWSTR, _grfmode: u32) -> windows::core::Result<()> {
        let path = unsafe { pszfilepath.to_string() }?;
        *self.path.lock().unwrap() = Some(path);
        Ok(())
    }
}

impl IThumbnailProvider_Impl for TumblerThumbnailer_Impl {
    fn GetThumbnail(
        &self,
        cx: u32,
        phbmp: *mut HBITMAP,
        pdwalphatype: *mut WTS_ALPHATYPE,
    ) -> windows::core::Result<()> {
        if phbmp.is_null() || pdwalphatype.is_null() {
            return Err(windows::core::Error::from(E_POINTER));
        }

        let path = self
            .path
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| windows::core::Error::from(E_FAIL))?;

        let pdfium = get_pdfium().ok_or_else(|| windows::core::Error::from(E_FAIL))?;

        let doc = pdfium
            .load_pdf_from_file(&path, None)
            .map_err(|_| windows::core::Error::from(E_FAIL))?;

        let page = doc
            .pages()
            .get(0)
            .map_err(|_| windows::core::Error::from(E_FAIL))?;

        let page_w = page.width().value;
        let page_h = page.height().value;

        // Scale to fit within cx×cx, preserving aspect ratio.
        let scale = (cx as f32) / page_w.max(page_h);
        let target_w = ((page_w * scale).round() as i32).max(1);
        let target_h = ((page_h * scale).round() as i32).max(1);

        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_width(target_w as Pixels)
                    .set_target_height(target_h as Pixels),
            )
            .map_err(|_| windows::core::Error::from(E_FAIL))?;

        let rgba = bitmap.as_rgba_bytes();
        let hbitmap = rgba_to_hbitmap(&rgba, target_w, target_h)?;

        unsafe {
            *phbmp = hbitmap;
            *pdwalphatype = WTSAT_ARGB;
        }
        Ok(())
    }
}

/// Convert a pdfium RGBA byte slice into a 32-bpp ARGB `HBITMAP`.
/// Windows DIBs store pixels bottom-up and in BGRA order; pdfium gives RGBA top-down.
fn rgba_to_hbitmap(
    rgba: &[u8],
    width: i32,
    height: i32,
) -> windows::core::Result<HBITMAP> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative height → top-down DIB (matches pdfium's row order).
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [Default::default()],
    };

    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbitmap = unsafe {
        CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
    }
    .map_err(|_| windows::core::Error::from(E_FAIL))?;

    // Copy pixels, swapping R↔B (pdfium RGBA → Windows BGRA).
    let pixel_count = (width * height) as usize;
    let dst = unsafe { std::slice::from_raw_parts_mut(bits as *mut u8, pixel_count * 4) };
    for i in 0..pixel_count {
        let s = i * 4;
        dst[s] = rgba[s + 2]; // B
        dst[s + 1] = rgba[s + 1]; // G
        dst[s + 2] = rgba[s]; // R
        dst[s + 3] = rgba[s + 3]; // A
    }

    Ok(hbitmap)
}

// ── IClassFactory ─────────────────────────────────────────────────────────────

#[implement(IClassFactory)]
struct TumblerThumbnailerFactory;

impl IClassFactory_Impl for TumblerThumbnailerFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: windows::core::Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if ppvobject.is_null() {
            return Err(windows::core::Error::from(E_POINTER));
        }
        unsafe { *ppvobject = std::ptr::null_mut() };
        if !punkouter.is_null() {
            return Err(windows::core::Error::from(CLASS_E_NOAGGREGATION));
        }
        let obj: IThumbnailProvider = TumblerThumbnailer::new().into();
        unsafe { obj.query(&*riid, ppvobject) }.ok()?;
        Ok(())
    }

    fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

// ── DLL exports ───────────────────────────────────────────────────────────────

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();

    if *rclsid != CLSID_THUMBNAILER {
        return windows::core::HRESULT(0x80040111u32 as i32); // CLASS_E_CLASSNOTAVAILABLE
    }

    let factory: IClassFactory = TumblerThumbnailerFactory.into();
    unsafe { factory.query(&*riid, ppv) }
}

/// Write HKCU registration entries so Explorer can find this DLL.
/// Uses HKCU so no administrator rights are needed (per-user install).
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    match register(true) {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    match register(false) {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

fn register(install: bool) -> windows::core::Result<()> {
    let dll = dll_path();
    let dll_str = dll.to_string_lossy();
    let guid_str = format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        CLSID_THUMBNAILER.data1,
        CLSID_THUMBNAILER.data2,
        CLSID_THUMBNAILER.data3,
        CLSID_THUMBNAILER.data4[0],
        CLSID_THUMBNAILER.data4[1],
        CLSID_THUMBNAILER.data4[2],
        CLSID_THUMBNAILER.data4[3],
        CLSID_THUMBNAILER.data4[4],
        CLSID_THUMBNAILER.data4[5],
        CLSID_THUMBNAILER.data4[6],
        CLSID_THUMBNAILER.data4[7],
    );

    if install {
        // HKCU\Software\Classes\CLSID\{guid}
        let clsid_path = format!("Software\\Classes\\CLSID\\{}", guid_str);
        reg_set(HKEY_CURRENT_USER, &clsid_path, "", "Tumbler PDF Thumbnail Provider")?;

        // HKCU\Software\Classes\CLSID\{guid}\InprocServer32
        let inproc_path = format!("{}\\InprocServer32", clsid_path);
        reg_set(HKEY_CURRENT_USER, &inproc_path, "", &dll_str)?;
        reg_set(HKEY_CURRENT_USER, &inproc_path, "ThreadingModel", "Apartment")?;

        // HKCU\Software\Classes\.pdf\ShellEx\{IThumbnailProvider}
        let shlex_path = format!(
            "Software\\Classes\\.pdf\\ShellEx\\{}",
            CLSID_THUMBNAIL_PROVIDER_HANDLER
        );
        reg_set(HKEY_CURRENT_USER, &shlex_path, "", &guid_str)?;
    } else {
        let clsid_path = format!("Software\\Classes\\CLSID\\{}", guid_str);
        let shlex_path = format!(
            "Software\\Classes\\.pdf\\ShellEx\\{}",
            CLSID_THUMBNAIL_PROVIDER_HANDLER
        );
        reg_delete(HKEY_CURRENT_USER, &clsid_path)?;
        reg_delete(HKEY_CURRENT_USER, &shlex_path)?;
    }

    Ok(())
}

fn reg_set(root: HKEY, subkey: &str, value_name: &str, data: &str) -> windows::core::Result<()> {
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey = HKEY::default();
    // RegCreateKeyW: simpler form; creates or opens the key with full access (no security param).
    unsafe { RegCreateKeyW(root, PCWSTR(subkey_w.as_ptr()), &mut hkey) }.ok()?;

    let name_w: Vec<u16> = value_name.encode_utf16().chain(std::iter::once(0)).collect();
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

fn reg_delete(root: HKEY, subkey: &str) -> windows::core::Result<()> {
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegDeleteTreeW(root, PCWSTR(subkey_w.as_ptr())) }.ok()
}

//! Adding Tumbler's install folder to the per-user `PATH`, and taking it off
//! again, for the NSIS installer (doc/cli-plan.html, change #9). The NSIS
//! hooks run `tumbler-cli __path add|remove <dir>`; the MSI uses Windows
//! Installer's own `Environment` element instead.
//!
//! Done in Rust rather than in NSIS because NSIS strings are limited to 1024
//! characters, and a longer `PATH` read through them comes back truncated,
//! silently dropping other programs' entries when written back. Here the
//! value is read and written whole, raw (`%VAR%` references unexpanded), and
//! in its original registry type, and only Tumbler's own entry is touched.

use std::path::Path;

/// Where a `PATH` value lives: `HKEY_CURRENT_USER\<subkey>`, value `Path`.
/// Production uses [`USER_ENVIRONMENT`]; tests use a scratch key.
pub(crate) const USER_ENVIRONMENT: &str = "Environment";

/// Adds `dir` to the end of the `PATH` under `subkey`, unless it is already
/// there. Returns whether anything changed.
pub(crate) fn add(subkey: &str, dir: &Path) -> Result<bool, String> {
    let dir = dir.to_string_lossy();
    let (current, kind) = registry::read(subkey)?.unwrap_or((String::new(), registry::Kind::Expand));
    match path_with(&current, &dir) {
        Some(updated) => registry::write(subkey, &updated, kind).map(|()| true),
        None => Ok(false),
    }
}

/// Removes every entry naming `dir` from the `PATH` under `subkey`, leaving
/// the rest exactly as it was. Returns whether anything changed.
pub(crate) fn remove(subkey: &str, dir: &Path) -> Result<bool, String> {
    let dir = dir.to_string_lossy();
    let Some((current, kind)) = registry::read(subkey)? else {
        return Ok(false);
    };
    match path_without(&current, &dir) {
        // Tumbler's entry was the only one: leave no empty value behind.
        Some(updated) if updated.is_empty() => registry::delete_value(subkey).map(|()| true),
        Some(updated) => registry::write(subkey, &updated, kind).map(|()| true),
        None => Ok(false),
    }
}

/// `path` with `dir` appended, or `None` if it is already an entry.
pub(crate) fn path_with(path: &str, dir: &str) -> Option<String> {
    if path.split(';').any(|entry| same_entry(entry, dir)) {
        return None;
    }
    Some(match path {
        "" => dir.to_string(),
        p if p.ends_with(';') => format!("{p}{dir}"),
        p => format!("{p};{dir}"),
    })
}

/// `path` without the entries naming `dir`, or `None` if it has none.
pub(crate) fn path_without(path: &str, dir: &str) -> Option<String> {
    let entries: Vec<&str> = path.split(';').collect();
    let kept: Vec<&str> = entries.iter().copied().filter(|entry| !same_entry(entry, dir)).collect();
    (kept.len() != entries.len()).then(|| kept.join(";"))
}

/// Whether two PATH entries name the same folder: case, surrounding quotes
/// and a trailing backslash don't matter.
fn same_entry(entry: &str, dir: &str) -> bool {
    let norm = |s: &str| s.trim().trim_matches('"').trim_end_matches('\\').to_lowercase();
    !entry.trim().is_empty() && norm(entry) == norm(dir)
}

/// The `Path` value under `HKEY_CURRENT_USER\<subkey>`, read and written raw.
pub(crate) mod registry {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    #[cfg(test)]
    use windows::Win32::System::Registry::RegDeleteTreeW;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    };

    /// The value's registry type, kept as found when it is rewritten.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(crate) enum Kind {
        /// `REG_EXPAND_SZ`: `%VAR%` references expand when read. Windows'
        /// own default for `Path`.
        Expand,
        /// `REG_SZ`.
        Plain,
    }

    const VALUE: &str = "Path";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The value and its type, or `None` when the key or value is absent.
    pub(crate) fn read(subkey: &str) -> Result<Option<(String, Kind)>, String> {
        let (subkey_w, name_w) = (wide(subkey), wide(VALUE));
        let mut hkey = HKEY::default();
        let opened =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(subkey_w.as_ptr()), None, KEY_QUERY_VALUE, &mut hkey) };
        if opened == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        opened.ok().map_err(|e| format!("cannot open HKCU\\{subkey}: {e}"))?;
        let result = read_open(hkey, &name_w, subkey);
        let _ = unsafe { RegCloseKey(hkey) };
        result
    }

    fn read_open(hkey: HKEY, name_w: &[u16], subkey: &str) -> Result<Option<(String, Kind)>, String> {
        let failed = |e: windows::core::Error| format!("cannot read HKCU\\{subkey}\\{VALUE}: {e}");
        // Ask for the size first: a PATH has no useful upper bound.
        let mut kind = REG_VALUE_TYPE::default();
        let mut len = 0u32;
        let sized = unsafe { RegQueryValueExW(hkey, PCWSTR(name_w.as_ptr()), None, Some(&mut kind), None, Some(&mut len)) };
        if sized == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        sized.ok().map_err(failed)?;
        let mut data = vec![0u8; len as usize];
        unsafe {
            RegQueryValueExW(hkey, PCWSTR(name_w.as_ptr()), None, Some(&mut kind), Some(data.as_mut_ptr()), Some(&mut len))
        }
        .ok()
        .map_err(failed)?;
        let kind = match kind {
            REG_EXPAND_SZ => Kind::Expand,
            REG_SZ => Kind::Plain,
            other => return Err(format!("HKCU\\{subkey}\\{VALUE} is not a string (registry type {})", other.0)),
        };
        let units: Vec<u16> = data[..len as usize].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let text = String::from_utf16(&units).map_err(|e| format!("HKCU\\{subkey}\\{VALUE}: {e}"))?;
        Ok(Some((text.trim_end_matches('\0').to_string(), kind)))
    }

    pub(crate) fn write(subkey: &str, value: &str, kind: Kind) -> Result<(), String> {
        let (subkey_w, name_w, data_w) = (wide(subkey), wide(VALUE), wide(value));
        let mut hkey = HKEY::default();
        unsafe { RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(subkey_w.as_ptr()), &mut hkey) }
            .ok()
            .map_err(|e| format!("cannot open HKCU\\{subkey}: {e}"))?;
        let bytes = unsafe { std::slice::from_raw_parts(data_w.as_ptr() as *const u8, data_w.len() * 2) };
        let ty = match kind {
            Kind::Expand => REG_EXPAND_SZ,
            Kind::Plain => REG_SZ,
        };
        let written = unsafe { RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), None, ty, Some(bytes)) };
        let _ = unsafe { RegCloseKey(hkey) };
        written.ok().map_err(|e| format!("cannot write HKCU\\{subkey}\\{VALUE}: {e}"))
    }

    pub(crate) fn delete_value(subkey: &str) -> Result<(), String> {
        let (subkey_w, name_w) = (wide(subkey), wide(VALUE));
        let mut hkey = HKEY::default();
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(subkey_w.as_ptr()), None, KEY_SET_VALUE, &mut hkey) }
            .ok()
            .map_err(|e| format!("cannot open HKCU\\{subkey}: {e}"))?;
        let deleted = unsafe { RegDeleteValueW(hkey, PCWSTR(name_w.as_ptr())) };
        let _ = unsafe { RegCloseKey(hkey) };
        if deleted == ERROR_SUCCESS || deleted == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("cannot delete HKCU\\{subkey}\\{VALUE}: {deleted:?}"))
        }
    }

    /// Test cleanup only: removes a whole scratch key.
    #[cfg(test)]
    pub(crate) fn delete_key(subkey: &str) -> Result<(), String> {
        let subkey_w = wide(subkey);
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey_w.as_ptr())) }.ok().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const DIR: &str = r"C:\Program Files\Tumbler";

    #[test]
    fn path_with_appends_once() {
        assert_eq!(path_with("", DIR).as_deref(), Some(DIR));
        assert_eq!(path_with(r"C:\a;C:\b", DIR).unwrap(), format!(r"C:\a;C:\b;{DIR}"));
        assert_eq!(path_with(r"C:\a;", DIR).unwrap(), format!(r"C:\a;{DIR}"));
        // Already there, however it is spelled.
        for present in [DIR, r"c:\program files\tumbler", r"C:\Program Files\Tumbler\", r#""C:\Program Files\Tumbler""#] {
            assert_eq!(path_with(&format!(r"C:\a;{present};C:\b"), DIR), None, "{present}");
        }
    }

    #[test]
    fn path_without_removes_only_its_own_entry() {
        let path = format!(r"%USERPROFILE%\bin;{DIR};C:\Program Files\Tumbler2;{DIR}\;D:\x");
        assert_eq!(path_without(&path, DIR).unwrap(), r"%USERPROFILE%\bin;C:\Program Files\Tumbler2;D:\x");
        assert_eq!(path_without(r"C:\a;C:\b", DIR), None);
        assert_eq!(path_without(DIR, DIR).as_deref(), Some(""));
    }

    /// The reason this is not done in NSIS: a PATH far over 1024 characters
    /// survives intact, other than the one entry.
    #[test]
    fn a_long_path_is_preserved_exactly() {
        let long: String = (0..400).map(|i| format!(r"C:\tools\program-{i:04}")).collect::<Vec<_>>().join(";");
        assert!(long.len() > 8000);
        let with = path_with(&long, DIR).unwrap();
        assert_eq!(with, format!("{long};{DIR}"));
        assert_eq!(path_without(&with, DIR).unwrap(), long);
    }

    // ── Against the registry, on a scratch key ───────────────────────────────

    /// `HKCU\Software\Tumbler-path-test-<uuid>`, deleted on drop.
    pub(crate) struct ScratchKey(pub(crate) String);

    impl ScratchKey {
        pub(crate) fn new() -> Self {
            ScratchKey(format!(r"Software\Tumbler-path-test-{}", uuid::Uuid::new_v4()))
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            let _ = registry::delete_key(&self.0);
        }
    }

    #[test]
    fn add_and_remove_round_trip_keeps_the_value_raw_and_its_type() {
        let key = ScratchKey::new();
        let long: String = (0..300).map(|i| format!(r"C:\tools\p{i:04}")).collect::<Vec<_>>().join(";");
        let original = format!(r"%USERPROFILE%\bin;{long}");
        registry::write(&key.0, &original, registry::Kind::Expand).unwrap();

        assert!(add(&key.0, Path::new(DIR)).unwrap());
        let (value, kind) = registry::read(&key.0).unwrap().unwrap();
        assert_eq!(value, format!("{original};{DIR}"), "%USERPROFILE% must stay unexpanded");
        assert_eq!(kind, registry::Kind::Expand);

        assert!(!add(&key.0, Path::new(DIR)).unwrap(), "adding twice changes nothing");
        assert!(remove(&key.0, Path::new(DIR)).unwrap());
        assert_eq!(registry::read(&key.0).unwrap().unwrap(), (original, registry::Kind::Expand));
        assert!(!remove(&key.0, Path::new(DIR)).unwrap(), "removing an absent entry changes nothing");
    }

    #[test]
    fn a_plain_string_path_stays_a_plain_string() {
        let key = ScratchKey::new();
        registry::write(&key.0, r"C:\a", registry::Kind::Plain).unwrap();
        add(&key.0, Path::new(DIR)).unwrap();
        assert_eq!(registry::read(&key.0).unwrap().unwrap().1, registry::Kind::Plain);
    }

    /// No per-user PATH yet (common): adding creates one; removing the only
    /// entry deletes the value again rather than leaving an empty one.
    #[test]
    fn a_missing_path_is_created_and_removed_cleanly() {
        let key = ScratchKey::new();
        assert_eq!(registry::read(&key.0).unwrap(), None);
        assert!(add(&key.0, Path::new(DIR)).unwrap());
        assert_eq!(registry::read(&key.0).unwrap().unwrap(), (DIR.to_string(), registry::Kind::Expand));
        assert!(remove(&key.0, Path::new(DIR)).unwrap());
        assert_eq!(registry::read(&key.0).unwrap(), None);
    }
}

//! App-level info commands.

/// The application version, baked in at build time from `Cargo.toml`
/// (kept in sync with package.json / tauri.conf.json by `scripts/sync-version.js`).
#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_non_empty_semver() {
        let v = get_app_version();
        // Baked in at build time; should look like "x.y.z".
        assert!(v.split('.').count() >= 2, "unexpected version: {v}");
    }
}

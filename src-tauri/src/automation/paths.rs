//! Path handling for tool arguments: `--root` confinement and atomic output
//! writes.
//!
//! With a root, every path a tool reads or writes must resolve inside it. A
//! relative path is taken relative to the root. Paths are canonicalized before
//! the containment check, so a symbolic link or junction inside the root that
//! points out of it is refused. Some forms are refused by name, before the
//! filesystem is asked anything: `..` components, NTFS alternate data streams
//! (`file.pdf:secret`), device names (`NUL`, `con.pdf`), UNC and other
//! non-drive prefixes (`\\server\share`, `\\?\`, `\\.\`), and drive- or
//! root-relative forms (`C:file.pdf`, `\file.pdf`), whose meaning depends on
//! process state.
//!
//! Without a root, paths are used as given: a person at a shell already chose
//! them. They are still canonicalized, because Tumbler compares stored paths
//! that way (the single-instance-per-file rule).

use super::{failed, ToolError};
use std::path::{Component, Path, PathBuf};

/// An existing file to read.
pub(crate) fn input_path(root: Option<&Path>, path: &str) -> Result<PathBuf, ToolError> {
    let joined = joined(root, path)?;
    let real = dunce::canonicalize(&joined).map_err(|e| failed(format!("{path}: {e}")))?;
    if real.is_dir() {
        return Err(failed(format!("{path} is a folder, not a file")));
    }
    within(root, &real, path)?;
    Ok(real)
}

/// A file to write. Its folder must exist; the file itself need not.
pub(crate) fn output_path(root: Option<&Path>, path: &str) -> Result<PathBuf, ToolError> {
    let joined = joined(root, path)?;
    let (Some(parent), Some(name)) = (joined.parent(), joined.file_name()) else {
        return Err(failed(format!("{path} does not name a file")));
    };
    let parent = dunce::canonicalize(parent)
        .map_err(|e| failed(format!("the folder for {path} cannot be used: {e}")))?;
    within(root, &parent, path)?;
    let target = parent.join(name);
    if target.is_dir() {
        return Err(failed(format!("{path} is a folder, not a file")));
    }
    // An existing file may itself be a link; what it points at must be inside
    // the root too.
    if let Ok(real) = dunce::canonicalize(&target) {
        within(root, &real, path)?;
    }
    Ok(target)
}

/// An existing folder to write files into.
pub(crate) fn output_dir(root: Option<&Path>, path: &str) -> Result<PathBuf, ToolError> {
    let joined = joined(root, path)?;
    let real = dunce::canonicalize(&joined).map_err(|e| failed(format!("{path}: {e}")))?;
    if !real.is_dir() {
        return Err(failed(format!("{path} is not a folder")));
    }
    within(root, &real, path)?;
    Ok(real)
}

/// Writes `bytes` to `path` through a temporary file and a rename, so a failed
/// write never leaves a truncated file behind.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ToolError> {
    crate::commands::save::atomic_write(&path.to_string_lossy(), bytes)
        .map_err(|e| failed(format!("{}: {e}", path.display())))
}

/// `path` as an absolute path, after the by-name checks. With a root, a
/// relative path is taken relative to it, and the result must lie inside it
/// before the filesystem is consulted, so nothing outside is even probed.
fn joined(root: Option<&Path>, path: &str) -> Result<PathBuf, ToolError> {
    if path.is_empty() {
        return Err(failed("the path is empty"));
    }
    let p = Path::new(path);
    let Some(root) = root else {
        return std::path::absolute(p).map_err(|e| failed(format!("{path}: {e}")));
    };
    let refuse = |why: &str| failed(format!("{path} is refused: {why}"));
    let mut has_prefix = false;
    for component in p.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(_) => has_prefix = true,
                _ => return Err(refuse(r"network, device and \\?\ paths are not allowed")),
            },
            Component::ParentDir => return Err(refuse("'..' is not allowed")),
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                if name.contains(':') {
                    return Err(refuse("alternate data streams are not allowed"));
                }
                if is_device_name(&name) {
                    return Err(refuse("device names are not allowed"));
                }
            }
            Component::RootDir | Component::CurDir => {}
        }
    }
    if !p.is_absolute() && (has_prefix || p.has_root()) {
        return Err(refuse("drive- or root-relative paths are not allowed"));
    }
    let joined = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    within(Some(root), &joined, path)?;
    Ok(joined)
}

/// Fails unless `real` lies inside `root` (when there is one).
fn within(root: Option<&Path>, real: &Path, path: &str) -> Result<(), ToolError> {
    match root {
        Some(root) if !real.starts_with(root) => Err(failed(format!(
            "{path} is outside the allowed folder {}",
            root.display()
        ))),
        _ => Ok(()),
    }
}

/// Windows device names, which open the device whatever folder or extension
/// they are given (`nul.txt` is `NUL`). Trailing dots and spaces don't count.
fn is_device_name(name: &str) -> bool {
    let trimmed = name.trim_end_matches(['.', ' ']);
    let stem = trimmed.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let mut chars = upper.chars();
    let head: String = chars.by_ref().take(3).collect();
    let digit: Vec<char> = chars.collect();
    (head == "COM" || head == "LPT")
        && matches!(digit.as_slice(), [d] if ('1'..='9').contains(d) || "\u{b9}\u{b2}\u{b3}".contains(*d))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh folder under the canonical temp folder, removed on drop.
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> Self {
            let base = dunce::canonicalize(std::env::temp_dir()).expect("canonical temp dir");
            let dir = base.join(format!("tumbler-cli-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            TempDir(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }

        pub(crate) fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).expect("write test file");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A directory junction at `link` pointing at `target`. Junctions, unlike
    /// symbolic links, need no privilege, so this works on any machine.
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J failed");
    }

    fn refused(r: Result<PathBuf, ToolError>) -> String {
        match r {
            Err(ToolError::Failed(m)) => m,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn relative_input_resolves_inside_root() {
        let root = TempDir::new("paths");
        let f = root.file("a.pdf", b"x");
        assert_eq!(input_path(Some(root.path()), "a.pdf").unwrap(), f);
    }

    #[test]
    fn absolute_input_inside_root_is_accepted() {
        let root = TempDir::new("paths");
        let f = root.file("a.pdf", b"x");
        assert_eq!(input_path(Some(root.path()), &f.to_string_lossy()).unwrap(), f);
    }

    #[test]
    fn paths_outside_root_are_refused() {
        let root = TempDir::new("paths");
        let outside = TempDir::new("outside");
        let f = outside.file("b.pdf", b"x");
        let msg = refused(input_path(Some(root.path()), &f.to_string_lossy()));
        assert!(msg.contains("outside"), "{msg}");
        refused(output_path(Some(root.path()), &outside.0.join("c.pdf").to_string_lossy()));
        refused(output_dir(Some(root.path()), &outside.0.to_string_lossy()));
    }

    /// `..` is refused by name, even when it would land back inside the root.
    #[test]
    fn parent_components_are_refused() {
        let root = TempDir::new("paths");
        std::fs::create_dir(root.0.join("sub")).unwrap();
        root.file("a.pdf", b"x");
        refused(input_path(Some(root.path()), r"sub\..\a.pdf"));
        refused(output_path(Some(root.path()), r"..\escape.pdf"));
    }

    #[test]
    fn junction_out_of_root_is_refused() {
        let root = TempDir::new("paths");
        let outside = TempDir::new("outside");
        outside.file("secret.pdf", b"x");
        junction(&root.0.join("link"), outside.path());
        refused(input_path(Some(root.path()), r"link\secret.pdf"));
        refused(output_path(Some(root.path()), r"link\new.pdf"));
        refused(output_dir(Some(root.path()), "link"));
    }

    #[test]
    fn alternate_data_streams_are_refused() {
        let root = TempDir::new("paths");
        root.file("a.pdf", b"x");
        refused(input_path(Some(root.path()), "a.pdf:secret"));
        refused(output_path(Some(root.path()), "a.pdf:secret:$DATA"));
    }

    #[test]
    fn device_names_are_refused() {
        let root = TempDir::new("paths");
        for name in ["NUL", "con.pdf", r"sub\aux", "nul.", "COM1.txt", "lpt9", "CON ."] {
            refused(output_path(Some(root.path()), name));
        }
        // Names that only start like a device are ordinary files.
        for name in ["console.pdf", "nullable.pdf", "com10.pdf"] {
            output_path(Some(root.path()), name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn unc_device_and_relative_drive_forms_are_refused() {
        let root = TempDir::new("paths");
        for p in [r"\\server\share\a.pdf", r"\\?\C:\a.pdf", r"\\.\pipe\a", r"C:a.pdf", r"\a.pdf"] {
            refused(output_path(Some(root.path()), p));
            refused(input_path(Some(root.path()), p));
        }
    }

    #[test]
    fn output_needs_an_existing_folder_and_must_not_be_one() {
        let root = TempDir::new("paths");
        std::fs::create_dir(root.0.join("sub")).unwrap();
        refused(output_path(Some(root.path()), r"missing\a.pdf"));
        refused(output_path(Some(root.path()), "sub"));
        assert_eq!(output_path(Some(root.path()), r"sub\new.pdf").unwrap(), root.0.join("sub").join("new.pdf"));
    }

    #[test]
    fn output_dir_must_exist_and_be_a_folder() {
        let root = TempDir::new("paths");
        root.file("a.pdf", b"x");
        std::fs::create_dir(root.0.join("out")).unwrap();
        assert_eq!(output_dir(Some(root.path()), "out").unwrap(), root.0.join("out"));
        refused(output_dir(Some(root.path()), "a.pdf"));
        refused(output_dir(Some(root.path()), "missing"));
    }

    #[test]
    fn missing_input_names_the_path() {
        let root = TempDir::new("paths");
        let msg = refused(input_path(Some(root.path()), "nope.pdf"));
        assert!(msg.contains("nope.pdf"), "{msg}");
        let msg = refused(input_path(None, &root.0.join("nope.pdf").to_string_lossy()));
        assert!(msg.contains("nope.pdf"), "{msg}");
    }

    #[test]
    fn empty_path_is_refused() {
        let root = TempDir::new("paths");
        refused(input_path(Some(root.path()), ""));
        refused(output_path(None, ""));
    }

    /// Without a root, any path works, and comes back canonical.
    #[test]
    fn without_root_paths_are_canonicalized_not_confined() {
        let dir = TempDir::new("free");
        let f = dir.file("a.pdf", b"x");
        let via_dots = format!(r"{}\.\a.pdf", dir.0.display());
        assert_eq!(input_path(None, &via_dots).unwrap(), f);
        assert_eq!(output_path(None, &dir.0.join("b.pdf").to_string_lossy()).unwrap(), dir.0.join("b.pdf"));
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_file() {
        let dir = TempDir::new("write");
        let target = dir.file("a.pdf", b"old");
        write_atomic(&target, b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["a.pdf"]);
    }
}

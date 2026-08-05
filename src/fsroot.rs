use std::path::{Component, Path, PathBuf};

/// Jail around the shared root directory. Every client-supplied path must go
/// through [`SecureRoot::resolve`] / [`SecureRoot::resolve_for_write`], which
/// reject traversal (`..`), absolute escapes and symlinks leading outside.
#[derive(Debug, Clone)]
pub struct SecureRoot {
    root: PathBuf,
}

#[derive(Debug)]
pub enum PathError {
    Denied(String),
    NotFound(String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Denied(p) => write!(f, "access denied: {p}"),
            PathError::NotFound(p) => write!(f, "not found: {p}"),
        }
    }
}
impl std::error::Error for PathError {}

impl SecureRoot {
    /// `root` must exist; it is canonicalized so later prefix checks are
    /// performed on a symlink-free absolute path.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self { root: root.canonicalize()? })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Normalize a client path (URL path, FTP argument, TFTP filename, ...)
    /// into a relative path with no `..`/`.`/absolute components.
    fn sanitize(rel: &str) -> Result<PathBuf, PathError> {
        let rel = rel.trim_start_matches('/');
        if rel.contains('\0') {
            return Err(PathError::Denied(rel.into()));
        }
        let mut clean = PathBuf::new();
        for comp in Path::new(rel).components() {
            match comp {
                Component::Normal(c) => clean.push(c),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(PathError::Denied(rel.into()));
                }
            }
        }
        Ok(clean)
    }

    /// Resolve for reading: the file must exist and, after resolving all
    /// symlinks, still live under the root.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, PathError> {
        let clean = Self::sanitize(rel)?;
        let joined = self.root.join(&clean);
        let canonical = joined
            .canonicalize()
            .map_err(|_| PathError::NotFound(rel.to_string()))?;
        if !canonical.starts_with(&self.root) {
            return Err(PathError::Denied(rel.to_string()));
        }
        Ok(canonical)
    }

    /// Resolve for writing: the file itself may not exist yet, but its parent
    /// directory must exist inside the root (symlink-checked). The final file
    /// name is validated separately.
    #[allow(dead_code)] // exercised in tests; upload flow uses begin_upload
    pub fn resolve_for_write(&self, rel: &str) -> Result<PathBuf, PathError> {
        let clean = Self::sanitize(rel)?;
        let file_name = clean
            .file_name()
            .ok_or_else(|| PathError::Denied(rel.to_string()))?
            .to_owned();
        validate_filename(&file_name.to_string_lossy()).map_err(PathError::Denied)?;
        let parent_rel = clean.parent().unwrap_or_else(|| Path::new(""));
        let parent = self.root.join(parent_rel);
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| PathError::NotFound(rel.to_string()))?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(PathError::Denied(rel.to_string()));
        }
        let target = canonical_parent.join(file_name);
        // If the target already exists as a symlink, refuse: writing through
        // it could escape the root.
        if let Ok(meta) = std::fs::symlink_metadata(&target) {
            if meta.file_type().is_symlink() {
                return Err(PathError::Denied(rel.to_string()));
            }
        }
        Ok(target)
    }

    /// Relative (URL-style, `/`-separated) path of an absolute path under root.
    pub fn relative(&self, abs: &Path) -> Option<String> {
        abs.strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }
}

/// Uploaded file names: no separators, no traversal, no control characters,
/// nothing hidden, reasonable length.
pub fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 {
        return Err(format!("invalid file name length: {name:?}"));
    }
    if name == "." || name == ".." {
        return Err("invalid file name".into());
    }
    if name.starts_with('.') {
        return Err(format!("hidden file names are not allowed: {name:?}"));
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c == '\0' || c.is_control()) {
        return Err(format!("file name contains forbidden characters: {name:?}"));
    }
    Ok(())
}

/// Free disk space in bytes for the filesystem containing `path`.
pub fn free_disk_space(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0 {
        Some(stat.f_bavail as u64 * stat.f_frsize as u64)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, SecureRoot) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.bin"), b"data").unwrap();
        std::fs::write(dir.path().join("sub/b.bin"), b"nested").unwrap();
        let root = SecureRoot::new(dir.path()).unwrap();
        (dir, root)
    }

    #[test]
    fn resolves_valid_paths() {
        let (_d, root) = setup();
        assert!(root.resolve("a.bin").is_ok());
        assert!(root.resolve("/a.bin").is_ok());
        assert!(root.resolve("sub/b.bin").is_ok());
        assert!(root.resolve("./sub/./b.bin").is_ok());
    }

    #[test]
    fn rejects_traversal() {
        let (_d, root) = setup();
        assert!(matches!(root.resolve("../etc/passwd"), Err(PathError::Denied(_))));
        assert!(matches!(root.resolve("sub/../../x"), Err(PathError::Denied(_))));
        assert!(matches!(root.resolve("..%2f..%2fx/.."), Err(PathError::Denied(_)) | Err(PathError::NotFound(_))));
        assert!(matches!(root.resolve_for_write("../up.bin"), Err(PathError::Denied(_))));
    }

    #[test]
    fn rejects_symlink_escape() {
        let (d, root) = setup();
        std::os::unix::fs::symlink("/etc/hosts", d.path().join("evil")).unwrap();
        assert!(matches!(root.resolve("evil"), Err(PathError::Denied(_))));
        std::os::unix::fs::symlink("/tmp", d.path().join("evildir")).unwrap();
        assert!(matches!(root.resolve("evildir/foo"), Err(PathError::Denied(_)) | Err(PathError::NotFound(_))));
    }

    #[test]
    fn symlink_inside_root_is_allowed() {
        let (d, root) = setup();
        std::os::unix::fs::symlink(d.path().join("a.bin"), d.path().join("alias.bin")).unwrap();
        assert!(root.resolve("alias.bin").is_ok());
    }

    #[test]
    fn write_resolution() {
        let (_d, root) = setup();
        assert!(root.resolve_for_write("new.bin").is_ok());
        assert!(root.resolve_for_write("sub/new.bin").is_ok());
        assert!(root.resolve_for_write("missingdir/new.bin").is_err());
    }

    #[test]
    fn filename_validation() {
        assert!(validate_filename("cat9k_iosxe.bin").is_ok());
        assert!(validate_filename("").is_err());
        assert!(validate_filename(".hidden").is_err());
        assert!(validate_filename("..").is_err());
        assert!(validate_filename("a/b").is_err());
        assert!(validate_filename("a\\b").is_err());
        assert!(validate_filename("a\0b").is_err());
        assert!(validate_filename(&"x".repeat(300)).is_err());
    }
}

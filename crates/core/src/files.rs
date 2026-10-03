use std::path::Path;
use std::time::SystemTime;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Modified,
}

impl SortBy {
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Size => "size",
            SortBy::Modified => "modified",
        }
    }
}

#[derive(Clone)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub ext: String,
}

pub struct FileBrowser {
    /// Path relative to root, "" = root.
    pub cwd: String,
    pub entries: Vec<FileEntry>,
    pub selected: usize,
    pub sort: SortBy,
    pub filter: String,
    pub error: Option<String>,
}

impl FileBrowser {
    pub fn new() -> Self {
        Self {
            cwd: String::new(),
            entries: Vec::new(),
            selected: 0,
            sort: SortBy::Name,
            filter: String::new(),
            error: None,
        }
    }

    pub fn visible(&self) -> Vec<&FileEntry> {
        let f = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name == ".." || f.is_empty() || e.name.to_lowercase().contains(&f))
            .collect()
    }

    pub fn up(&mut self) {
        self.cwd = self
            .cwd
            .rsplit_once('/')
            .map(|(p, _)| p.into())
            .unwrap_or_default();
        self.selected = 0;
        self.filter.clear();
    }
    pub fn enter(&mut self, name: &str) {
        if name == ".." {
            self.up();
        } else {
            self.cwd = if self.cwd.is_empty() {
                name.into()
            } else {
                format!("{}/{name}", self.cwd)
            };
            self.selected = 0;
            self.filter.clear();
        }
    }
    pub fn refresh(&mut self, root: &Path) {
        self.error = None;
        let jail = match crate::fsroot::SecureRoot::new(root) {
            Ok(jail) => jail,
            Err(e) => {
                self.error = Some(e.to_string());
                self.entries.clear();
                return;
            }
        };
        let dir = match jail.resolve(&self.cwd) {
            Ok(dir) => dir,
            Err(e) => {
                self.error = Some(e.to_string());
                self.entries.clear();
                return;
            }
        };
        let mut entries = Vec::new();
        match std::fs::read_dir(&dir) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    if jail.resolve(&format!("{}/{}", self.cwd, name)).is_err() {
                        continue;
                    }
                    let meta = match std::fs::metadata(e.path()) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let ext = std::path::Path::new(&name)
                        .extension()
                        .map(|x| x.to_string_lossy().to_string())
                        .unwrap_or_default();
                    entries.push(FileEntry {
                        is_dir: meta.is_dir(),
                        size: meta.len(),
                        modified: meta.modified().ok(),
                        name,
                        ext,
                    });
                }
            }
            Err(e) => self.error = Some(format!("cannot read directory: {e}")),
        }
        match self.sort {
            SortBy::Name => entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }),
            SortBy::Size => {
                entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| b.size.cmp(&a.size)))
            }
            SortBy::Modified => entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| b.modified.cmp(&a.modified))
            }),
        }
        if !self.cwd.is_empty() {
            entries.insert(
                0,
                FileEntry {
                    name: "..".into(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                    ext: String::new(),
                },
            );
        }
        self.entries = entries;
        if self.selected >= self.visible().len() {
            self.selected = self.visible().len().saturating_sub(1);
        }
    }
}

pub fn compute_hashes(path: &std::path::Path) -> std::io::Result<(String, String, String)> {
    use md5::Md5;
    use sha2::{Digest, Sha256, Sha512};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut md5 = Md5::new();
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        md5.update(&buf[..n]);
        sha256.update(&buf[..n]);
        sha512.update(&buf[..n]);
    }
    Ok((
        hex(&md5.finalize()),
        hex(&sha256.finalize()),
        hex(&sha512.finalize()),
    ))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl Default for FileBrowser {
    fn default() -> Self {
        Self::new()
    }
}
#[derive(Debug, Clone)]
pub struct FileHashes {
    pub md5: String,
    pub sha256: String,
    pub sha512: String,
}

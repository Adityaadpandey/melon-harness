use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub fn expanduser(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}

/// Lexically remove `.` and `..` (no filesystem access), like `os.path.normpath`.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

/// Resolve symlinks for the part of the path that exists and normalize the rest, like `Path.resolve()`.
pub fn resolve(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let abs = normalize(&abs);
    if let Ok(c) = fs::canonicalize(&abs) {
        return c;
    }
    // walk up to the deepest ancestor that exists, canonicalize it, and put the tail back
    let mut tail = Vec::new();
    let mut cur = abs.clone();
    while !cur.exists() {
        match cur.file_name() {
            Some(n) => tail.push(n.to_os_string()),
            None => return abs,
        }
        if !cur.pop() {
            return abs;
        }
    }
    let mut base = fs::canonicalize(&cur).unwrap_or(cur);
    for t in tail.into_iter().rev() {
        base.push(t);
    }
    base
}

/// A model-given path against the working directory. `~/x` expands, absolute paths resolve,
/// relative ones join the resolved base (same as the Python harness).
pub fn resolve_path(base: &Path, path: &str) -> PathBuf {
    let p = expanduser(path);
    if p.is_absolute() {
        return resolve(&p);
    }
    resolve(base).join(p)
}

pub fn ensure_parent_directory(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn is_binary_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = fs::File::open(path) else { return false };
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

/// Write via a temp file in the same folder and rename over the target, so a crash mid-write leaves the old file
/// intact. Keeps the original permissions and writes through a symlink to its target.
pub fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    let target = resolve(path);
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let mode = fs::metadata(&target).ok().map(|m| m.permissions());
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut tmp = tempfile::Builder::new().prefix(&format!(".{name}.")).suffix(".tmp").tempfile_in(&dir)?;
    tmp.write_all(text.as_bytes())?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    if let Some(m) = mode {
        fs::set_permissions(tmp.path(), m)?;
    }
    tmp.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

/// `path` relative to `base` when it sits under it, else unchanged (for messages).
pub fn rel_display(path: &Path, base: &Path) -> String {
    for (a, b) in [(path.to_path_buf(), base.to_path_buf()), (resolve(path), resolve(base))] {
        if let Ok(r) = a.strip_prefix(&b) {
            return r.to_string_lossy().to_string();
        }
    }
    path.to_string_lossy().to_string()
}

pub fn is_relative_to(path: &Path, base: &Path) -> bool {
    path == base || path.starts_with(base)
}

pub fn read_text_lossy(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).to_string()))
}

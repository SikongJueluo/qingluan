//! Path normalization and workspace/session association.

use std::path::{Component, Path, PathBuf};

/// Normalize a path lexically (like Node's `path.resolve`): make it absolute
/// against the process cwd and resolve `.`/`..` without touching the
/// filesystem or following symlinks.
pub fn lexical_absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Association key for a path: canonicalize existing paths (following
/// symlinks); keep unavailable paths representable via lexical normalization.
pub fn association_key(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(_) => lexical_absolute(path),
    }
}

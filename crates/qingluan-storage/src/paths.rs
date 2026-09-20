//! Storage root layout and directory durability helpers (no libc).

use std::path::{Path, PathBuf};

use crate::error::{io_error, StorageError};
use crate::identity::LogKey;

/// SQLite database file inside the storage root.
pub(crate) fn db_path(root: &Path) -> PathBuf {
    root.join("terminal.db")
}

/// One segment file inside the storage root.
pub(crate) fn segment_path(root: &Path, file_name: &str) -> PathBuf {
    root.join(file_name)
}

/// File name of a segment row id (ids are never reused; the AUTOINCREMENT
/// counter keeps quarantine artifacts from ever colliding).
pub(crate) fn segment_file_name(segment_id: i64) -> String {
    format!("seg-{segment_id:06}.log")
}

/// Durable-quarantine name of a segment file: both the whole-file rename
/// (orphan or irrecoverable segment) and the uncommitted-tail artifact use
/// it. The `quarantine-` prefix can never collide with a live segment
/// name, and the name is deterministic per segment so a re-run of recovery
/// converges onto the same artifact instead of accumulating copies.
pub(crate) fn quarantine_file_name(file_name: &str) -> String {
    format!("quarantine-{file_name}")
}

/// Lock file of one log's exclusive writer lease inside the storage root.
/// The composite key's components are opaque, attacker-influenced text
/// (they cannot go into a file name verbatim), so the name carries the
/// SHA-256 of the canonical key text: the same log always maps to the
/// same file, distinct logs map to distinct files except with
/// cryptographic improbability (a 32-bit checksum would collide between
/// real key sets and wrongly serialize two logs onto one lease), and the
/// `writer-*.lock` prefix can never collide with a segment or quarantine
/// name. The file is zero bytes and purely a lock anchor — its content is
/// never read.
///
/// Hash input disambiguation: every complete key component is hashed
/// behind a fixed-width (8-byte little-endian) length prefix and nothing
/// else. The resulting byte stream is uniquely parseable (each length
/// determines where the next component starts), so no two distinct key
/// tuples can ever hash to the same input — not even by shifting bytes
/// across a component boundary, which a bare separator byte (the retired
/// encoding) allowed whenever a component contained that byte.
pub(crate) fn writer_lock_path(root: &Path, key: &LogKey) -> PathBuf {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for component in [
        key.session_source.as_bytes(),
        key.external_session_id.as_bytes(),
        key.terminal_id.as_bytes(),
    ] {
        hasher.update((component.len() as u64).to_le_bytes());
        hasher.update(component);
    }
    root.join(format!("writer-{:x}.lock", hasher.finalize()))
}

/// fsync a directory so a newly created entry (segment file) is durable.
/// Opening the directory and `sync_all` is the libc-free equivalent of
/// `open(dir, O_DIRECTORY)` + `fsync(fd)`; it runs on the async runtime's
/// blocking pool (`tokio::fs`) so no async caller is stalled on the
/// executor thread.
pub(crate) async fn fsync_dir(dir: &Path) -> Result<(), StorageError> {
    let file = tokio::fs::File::open(dir).await.map_err(io_error(dir))?;
    file.sync_all().await.map_err(io_error(dir))?;
    Ok(())
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source: &str, external: &str, terminal: &str) -> LogKey {
        LogKey {
            session_source: source.to_owned(),
            external_session_id: external.to_owned(),
            terminal_id: terminal.to_owned(),
        }
    }

    /// The lease name is a stable, collision-resistant function of the
    /// complete canonical key: one lock per log, so two distinct logs can
    /// never be wrongly serialized onto one shared lease (which a 32-bit
    /// checksum of attacker-chosen components could not guarantee).
    #[test]
    fn writer_lock_names_are_stable_and_keyed_by_the_whole_identity() {
        let root = Path::new("/tmp");
        const TERMINAL: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
        const OTHER: &str = "8d0f7780-8536-51ef-055c-f180d2a01bf8";
        let base = writer_lock_path(root, &key("pi", "session-1", TERMINAL));
        // Deterministic: the same canonical key maps to the same name.
        assert_eq!(
            base,
            writer_lock_path(root, &key("pi", "session-1", TERMINAL))
        );
        // Every component participates, and near-miss keys (one byte of
        // one component) map to distinct names.
        for other in [
            key("pi", "session-1", OTHER),
            key("pi", "session-2", TERMINAL),
            key("pj", "session-1", TERMINAL),
            key("", "session-1", TERMINAL),
            key("pi", "", TERMINAL),
        ] {
            assert_ne!(
                base,
                writer_lock_path(root, &other),
                "distinct keys must never share one lease file"
            );
        }
        // The anchor stays on its own namespace: never a segment or
        // quarantine name, and it is a plain file name component.
        let name = base.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("writer-") && name.ends_with(".lock"));
        assert!(name.len() <= "writer-".len() + 64 + ".lock".len());
    }

    /// The retired encoding (component, then one `0x1f` separator byte):
    /// a component ending in `0x1f` and the next one starting with `0x1f`
    /// produced byte-identical hash inputs for *distinct* keys, wrongly
    /// serializing two logs onto one exclusive lease. Fixed-width length
    /// prefixes close the shift: the regression pins both halves — the
    /// retired digest of each pair is equal (the hazard was real), and the
    /// lease names of the same pair are distinct (the fix holds) — at
    /// every component boundary.
    #[test]
    fn writer_lock_names_disambiguate_shifted_component_boundaries() {
        fn retired_digest(source: &str, external: &str, terminal: &str) -> [u8; 32] {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            for component in [source.as_bytes(), external.as_bytes(), terminal.as_bytes()] {
                hasher.update(component);
                hasher.update([0x1f]);
            }
            hasher.finalize().into()
        }
        let root = Path::new("/tmp");
        const TERMINAL: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
        let shifted_terminal = format!("\u{1f}{TERMINAL}");
        // Boundary pairs: the separator byte shifted one position across
        // each of the two component boundaries (source|external and
        // external|terminal).
        let pairs = [
            (
                ("pi\u{1f}", "session-1", TERMINAL),
                ("pi", "\u{1f}session-1", TERMINAL),
            ),
            (
                ("pi", "session-1\u{1f}", TERMINAL),
                ("pi", "session-1", shifted_terminal.as_str()),
            ),
            // An embedded separator adjacent to one at a boundary: the
            // old stream collides across three components at once.
            (
                ("a\u{1f}", "\u{1f}b", TERMINAL),
                ("a", "\u{1f}\u{1f}b", TERMINAL),
            ),
        ];
        for ((a_source, a_external, a_terminal), (b_source, b_external, b_terminal)) in pairs {
            assert_eq!(
                retired_digest(a_source, a_external, a_terminal),
                retired_digest(b_source, b_external, b_terminal),
                "the retired separator encoding must collide for this pair \
                 (otherwise the regression no longer pins the hazard)"
            );
            assert_ne!(
                writer_lock_path(root, &key(a_source, a_external, a_terminal)),
                writer_lock_path(root, &key(b_source, b_external, b_terminal)),
                "distinct keys must never share one lease file"
            );
        }
    }
}

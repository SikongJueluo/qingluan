//! Conversion of opaque core identities into storage row keys and the
//! segment header's 16-byte identity fields.
//!
//! `TerminalId` and `LogEpoch` are qingluan-minted UUID text at this seam
//! (supervisor decision, S2): the exact text is kept for SQLite keys and
//! logs, while the parsed UUID bytes go into the on-disk header. Only the
//! canonical hyphenated lowercase spelling is accepted, so the text key
//! and the header bytes are strictly 1:1 — two spellings of one UUID can
//! never become two terminal rows sharing one on-disk identity. Identity
//! parsing happens before any file or database mutation so a malformed
//! identity can never leave partial state behind. `SessionSource` and
//! `ExternalSessionId` stay opaque text keys with no format constraint.

use qingluan_core::terminal::{LogIdentity, TerminalRef};
use uuid::Uuid;

use crate::error::StorageError;

/// Composite SQLite key of one terminal log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogKey {
    pub session_source: String,
    pub external_session_id: String,
    pub terminal_id: String,
}

impl LogKey {
    /// The key of one terminal, with no epoch: every query resolves the
    /// persisted epoch itself, so a caller only has to name the terminal.
    pub(crate) fn of(terminal: &TerminalRef) -> Self {
        Self {
            session_source: terminal.session.source.as_str().to_owned(),
            external_session_id: terminal.session.external_id.as_str().to_owned(),
            terminal_id: terminal.terminal_id.as_str().to_owned(),
        }
    }
}

/// 16-byte identities carried by every segment header of one log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeaderIdentity {
    pub terminal_uuid: [u8; 16],
    pub epoch: [u8; 16],
}

/// The parsed halves of one [`LogIdentity`].
#[derive(Debug, Clone)]
pub(crate) struct ResolvedIdentity {
    pub key: LogKey,
    pub header: HeaderIdentity,
    pub terminal_id: String,
    pub epoch: String,
}

fn parse_uuid16(field: &'static str, value: &str) -> Result<[u8; 16], StorageError> {
    let uuid = Uuid::parse_str(value).map_err(|_| StorageError::InvalidIdentity {
        field,
        value: value.to_owned(),
    })?;
    // Canonical spelling only (hyphenated lowercase, UUID Display form):
    // `Uuid::parse_str` also accepts braced/URN/simple spellings, and two
    // accepted spellings of one UUID would map to two different SQLite
    // text keys but one identical 16-byte header identity.
    if uuid.to_string() != value {
        return Err(StorageError::InvalidIdentity {
            field,
            value: value.to_owned(),
        });
    }
    Ok(*uuid.as_bytes())
}

impl ResolvedIdentity {
    /// Validate and split a [`LogIdentity`]. Fails before any mutation when
    /// the terminal id or log epoch is not UUID text.
    pub(crate) fn parse(log: &LogIdentity) -> Result<Self, StorageError> {
        let terminal_id = log.terminal.terminal_id.as_str();
        let epoch = log.log_epoch.as_str();
        Ok(Self {
            key: LogKey {
                session_source: log.terminal.session.source.as_str().to_owned(),
                external_session_id: log.terminal.session.external_id.as_str().to_owned(),
                terminal_id: terminal_id.to_owned(),
            },
            header: HeaderIdentity {
                terminal_uuid: parse_uuid16("terminal_id", terminal_id)?,
                epoch: parse_uuid16("log_epoch", epoch)?,
            },
            terminal_id: terminal_id.to_owned(),
            epoch: epoch.to_owned(),
        })
    }
}

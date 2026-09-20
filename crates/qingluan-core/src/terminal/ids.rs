//! Terminal and log identity types.
//!
//! Every identifier is an opaque newtype with private string storage:
//! distinct identities cannot be cross-assigned, and no wire
//! representation leaks into the core model. Constructing from (and
//! reading back) the host- or daemon-assigned string form is the
//! adapter-facing surface; protocol encodings belong to the daemon.

/// Host that owns agent sessions and mints their external IDs.
///
/// The name is assigned by that host and opaque to qingluan: comparison
/// is exact, with no normalization, case folding, or trimming.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionSource(String);

/// Host-minted session identifier, opaque to qingluan.
///
/// Unique for the current OS user within its [`SessionSource`]; cwd,
/// workspace, and connection identity deliberately do not participate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalSessionId(String);

/// Opaque identifier of one terminal inside a session.
///
/// Never reused, including after the terminal's record has been deleted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerminalId(String);

/// Generation of one terminal's persisted output log.
///
/// Changes only when a destructive rebuild makes old positions
/// uninterpretable; ordinary restarts and log rotation preserve it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogEpoch(String);

/// Opaque identifier of the mutable, not-yet-finalized tail line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TailId(String);

impl SessionSource {
    /// Wrap the host-assigned source name exactly as minted.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The host-assigned source name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ExternalSessionId {
    /// Wrap the host-assigned session identifier exactly as minted.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The host-assigned session identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TerminalId {
    /// Wrap the opaque terminal identifier exactly as minted.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The opaque terminal identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl LogEpoch {
    /// Wrap the persisted log generation exactly as minted.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The persisted log generation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TailId {
    /// Wrap the opaque tail-line identifier exactly as minted.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The opaque tail-line identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identity of an agent session as minted by a host (for example a Pi
/// workspace session).
///
/// Unique for the current OS user; cwd, workspace, and connection identity
/// deliberately do not participate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionRef {
    /// Host that owns the session and minted `external_id`.
    pub source: SessionSource,
    /// Host-specific session identifier, opaque to qingluan.
    pub external_id: ExternalSessionId,
}

/// Identity of one terminal inside a session.
///
/// `terminal_id` is opaque and never reused, including after the
/// terminal's record has been deleted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerminalRef {
    /// Owning session.
    pub session: SessionRef,
    /// Opaque terminal identifier; never reused.
    pub terminal_id: TerminalId,
}

/// Identity of the persisted output log of one terminal.
///
/// `log_epoch` changes only when a destructive rebuild makes old positions
/// uninterpretable; ordinary restarts and log rotation preserve it.
/// Positions and cursors are valid only for the exact epoch they were
/// minted against.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogIdentity {
    /// Terminal whose output log this is.
    pub terminal: TerminalRef,
    /// Persisted log generation, preserved across restart and rotation.
    pub log_epoch: LogEpoch,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(source: &str, external_id: &str) -> SessionRef {
        SessionRef {
            source: SessionSource::new(source),
            external_id: ExternalSessionId::new(external_id),
        }
    }

    fn terminal(session: SessionRef, terminal_id: &str) -> TerminalRef {
        TerminalRef {
            session,
            terminal_id: TerminalId::new(terminal_id),
        }
    }

    fn log(terminal: TerminalRef, log_epoch: &str) -> LogIdentity {
        LogIdentity {
            terminal,
            log_epoch: LogEpoch::new(log_epoch),
        }
    }

    #[test]
    fn identity_comparison_is_exact_and_opaque() {
        let base = log(terminal(session("pi", "s1"), "t1"), "epoch-1");

        // Identifiers and epochs are opaque: no normalization, case
        // folding, or trimming; comparison is exact equality.
        assert_eq!(base, log(terminal(session("pi", "s1"), "t1"), "epoch-1"));
        assert_ne!(base, log(terminal(session("pi", "s1"), "t1"), "epoch-2"));
        assert_ne!(base, log(terminal(session("pi", "s1"), "t2"), "epoch-1"));
        assert_ne!(base, log(terminal(session("other", "s1"), "t1"), "epoch-1"));
        assert_ne!(base, log(terminal(session("pi", "s2"), "t1"), "epoch-1"));
    }

    #[test]
    fn identifier_strings_round_trip_through_adapters() {
        // The adapter-facing surface is exactly `new` + `as_str`: the
        // minted string comes back unchanged and is not reformatted.
        let session = session("pi", "s1");
        assert_eq!(session.source.as_str(), "pi");
        assert_eq!(session.external_id.as_str(), "s1");
        assert_eq!(terminal(session, "t1").terminal_id.as_str(), "t1");
        assert_eq!(TailId::new("tail").as_str(), "tail");
        assert_eq!(LogEpoch::new(" epoch-1 ").as_str(), " epoch-1 ");
    }
}

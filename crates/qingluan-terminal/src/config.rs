//! Runtime construction parameters.

/// One runtime instance's construction parameters.
///
/// The limits are counts of *occupying* terminal slots (the two-level
/// activity quota). The cgroup tag names the manager-owned cgroup root
/// inside the current delegated subtree: it must be unique per runtime
/// instance so that startup reconciliation only ever sweeps that
/// instance's own leftover terminal cgroups.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Per-session activity limit.
    pub session_limit: usize,
    /// Global activity limit.
    pub global_limit: usize,
    /// Manager-owned cgroup root tag (unique per runtime instance).
    pub cgroup_tag: String,
}

impl RuntimeConfig {
    /// The confirmed initial limits (8 per session / 32 global) with the
    /// supplied manager tag.
    pub fn new(cgroup_tag: impl Into<String>) -> Self {
        Self {
            session_limit: 8,
            global_limit: 32,
            cgroup_tag: cgroup_tag.into(),
        }
    }

    /// Override the session and global activity limits (used by tests and
    /// by the daemon's validated configuration).
    pub fn with_limits(mut self, session_limit: usize, global_limit: usize) -> Self {
        self.session_limit = session_limit;
        self.global_limit = global_limit;
        self
    }
}

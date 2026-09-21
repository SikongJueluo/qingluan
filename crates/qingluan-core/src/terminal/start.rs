//! Explicit terminal start specification and environment snapshot.

use super::snapshot::TerminalSize;

/// Complete environment snapshot passed at start.
///
/// Explicit by construction: the caller supplies the whole environment for
/// the new process (the adapter forwards the agent process's actual
/// environment), so a missing snapshot is unrepresentable rather than
/// silently falling back to the qingluan service environment. An
/// explicitly empty snapshot is legal and means "no variables". Keys and
/// values are UTF-8 text — conversion from the host's raw environment is
/// the adapter's job, and no OS/environment-handle type appears here.
/// Values are used only to start the process; they never enter logs,
/// query results, or notifications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSnapshot {
    entries: Vec<(String, String)>,
}

impl EnvironmentSnapshot {
    /// Wrap an explicit entry list, preserving the caller's order.
    pub fn new(entries: Vec<(String, String)>) -> Self {
        Self { entries }
    }

    /// The explicitly empty environment.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Number of variables in the snapshot.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the snapshot sets no variable (a legal, explicit empty
    /// environment — distinct from "not provided", which is
    /// unrepresentable here).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over `(name, value)` pairs in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// Explicit program, arguments, working directory, environment, and
/// initial window size for one terminal start.
///
/// Never an implicit shell: a pipeline or redirection is expressed by
/// passing a shell program explicitly, and a persistent interactive shell
/// by starting that shell. All text is UTF-8; no OS path, handle, or
/// process type appears here. Whether the working directory exists, and
/// whether the program can actually be started, are runtime concerns of
/// the terminal slice, not shape rules of this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSpec {
    /// Program to execute (no shell wrapping).
    pub program: String,
    /// Arguments passed to the program, in order.
    pub args: Vec<String>,
    /// Working directory for the new process.
    pub cwd: String,
    /// Complete environment snapshot (explicit; may be empty).
    pub env: EnvironmentSnapshot,
    /// Initial terminal window size.
    pub size: TerminalSize,
}

impl StartSpec {
    /// Construct a start specification from its explicit parts.
    pub fn new(
        program: impl Into<String>,
        args: Vec<String>,
        cwd: impl Into<String>,
        env: EnvironmentSnapshot,
        size: TerminalSize,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            cwd: cwd.into(),
            env,
            size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> TerminalSize {
        TerminalSize {
            rows: 30,
            columns: 120,
        }
    }

    #[test]
    fn environment_snapshot_is_explicit_and_may_be_empty() {
        // An explicitly empty snapshot is a real value, not a missing one.
        let empty = EnvironmentSnapshot::empty();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.iter().count(), 0);

        // A provided snapshot round-trips its entries in order, including
        // non-ASCII UTF-8 and empty values.
        let snapshot = EnvironmentSnapshot::new(vec![
            ("PATH".to_owned(), "/bin".to_owned()),
            ("GREETING".to_owned(), "你好".to_owned()),
            ("EMPTY".to_owned(), String::new()),
        ]);
        assert_eq!(snapshot.len(), 3);
        assert!(!snapshot.is_empty());
        assert_eq!(
            snapshot.iter().collect::<Vec<_>>(),
            vec![("PATH", "/bin"), ("GREETING", "你好"), ("EMPTY", "")]
        );
        assert_ne!(snapshot, EnvironmentSnapshot::empty());
    }

    #[test]
    fn start_spec_carries_every_explicit_part() {
        let env = EnvironmentSnapshot::new(vec![("TERM".to_owned(), "xterm".to_owned())]);
        let spec = StartSpec::new(
            "/bin/sh",
            vec!["-c".to_owned(), "echo hi".to_owned()],
            "/tmp",
            env.clone(),
            size(),
        );
        assert_eq!(spec.program, "/bin/sh");
        assert_eq!(spec.args, vec!["-c".to_owned(), "echo hi".to_owned()]);
        assert_eq!(spec.cwd, "/tmp");
        assert_eq!(spec.env, env);
        assert_eq!(spec.size, size());

        // No implicit shell: an empty program/args is representable and
        // cannot be confused with "default to a shell".
        let bare = StartSpec::new("", Vec::new(), "", EnvironmentSnapshot::empty(), size());
        assert!(bare.program.is_empty());
        assert!(bare.args.is_empty());
        assert!(bare.env.is_empty());
    }
}

//! Qingluan configuration loading.
//!
//! Four layers, later layers win:
//!
//! 1. built-in defaults (this crate)
//! 2. global file: `~/.config/qingluan/config.toml` (XDG)
//! 3. project file: `qingluan.toml` in the current directory
//! 4. environment: `QINGLUAN_*`, nested keys via double underscore
//!    (e.g. `QINGLUAN_DAEMON__PORT=47200`)
//!
//! Missing files are fine and fall through to defaults. Malformed files,
//! unknown keys, or wrong types are hard errors: silently ignoring typos
//! causes confusing drift between what users write and what applies.

use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

/// Full Qingluan configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub workspace: WorkspaceConfig,
    pub daemon: DaemonConfig,
    pub terminal: TerminalConfig,
    pub sandbox: SandboxConfig,
    pub cube: CubeConfig,
}

/// Workspace placement for `qingluan workspace add`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Root directory for new workspaces:
    /// `<root>/<repo-name>/<workspace-name>`. A leading `~` is expanded
    /// against the user's home directory.
    pub root: PathBuf,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        let root = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Projects")
            .join(".workspace");
        Self { root }
    }
}

/// Daemon HTTP listen address. Consumed by `qingluan daemon start` and by
/// the CLI's default `--daemon-url`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub host: String,
    pub port: u16,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 47129,
        }
    }
}

/// Local terminal service settings consumed by `qingluan daemon start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    /// Unix-domain socket for the terminal gRPC service.
    pub socket_path: PathBuf,
    /// Durable terminal database and log root.
    pub storage_root: PathBuf,
    /// Maximum occupying terminals per session.
    pub session_limit: usize,
    /// Maximum occupying terminals daemon-wide.
    pub global_limit: usize,
    /// Manager-owned cgroup directory name inside the delegated unit.
    pub cgroup_tag: String,
    /// Server-side control lease lifetime. Clients normally renew every 10 s.
    pub lease_ttl_seconds: u64,
    /// Hard transport envelope for one encoded gRPC message.
    pub max_message_bytes: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        let runtime = dirs::runtime_dir()
            .or_else(dirs::state_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("qingluan");
        let storage = dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("qingluan")
            .join("terminal");
        Self {
            socket_path: runtime.join("daemon.sock"),
            storage_root: storage,
            session_limit: 8,
            global_limit: 32,
            cgroup_tag: "qingluan-terminal".into(),
            lease_ttl_seconds: 30,
            max_message_bytes: 1024 * 1024,
        }
    }
}

/// Sandbox defaults — NOT yet consumed (reserved for the task execution
/// engine). Schema-only so configs written today keep working later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    pub provider: String,
    pub template: Option<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            template: None,
        }
    }
}

/// Cube sandbox provider settings — NOT yet consumed (reserved for the
/// task execution engine).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CubeConfig {
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub template: Option<String>,
}

/// Path of the global configuration file (`$XDG_CONFIG_HOME` honored).
pub fn global_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("qingluan")
        .join("config.toml")
}

/// Load the configuration with all layers merged (see crate docs).
///
/// The error is boxed: `figment::Error` is 208 bytes and clippy rightly
/// complains about carrying that inline through `Result`.
pub fn load() -> Result<Config, Box<figment::Error>> {
    load_from(&global_config_path(), Path::new("qingluan.toml"))
}

/// Layered load with explicit file paths (testable core).
pub fn load_from(global: &Path, project: &Path) -> Result<Config, Box<figment::Error>> {
    let mut config: Config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(global))
        .merge(Toml::file(project))
        .merge(Env::prefixed("QINGLUAN_").split("__"))
        .extract()
        .map_err(Box::new)?;
    config.workspace.root = expand_tilde(&config.workspace.root);
    config.terminal.socket_path = expand_tilde(&config.terminal.socket_path);
    config.terminal.storage_root = expand_tilde(&config.terminal.storage_root);
    Ok(config)
}

/// Expand a leading `~` (exactly, or `~/…`) against the home directory.
/// `~user` forms are left untouched.
fn expand_tilde(path: &Path) -> PathBuf {
    if path == Path::new("~") {
        return dirs::home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    if let Ok(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn load_both(global: &str, project: &str) -> Result<Config, Box<figment::Error>> {
        let dir = TempDir::new().unwrap();
        let global_path = dir.path().join("global.toml");
        let project_path = dir.path().join("project.toml");
        fs::write(&global_path, global).unwrap();
        fs::write(&project_path, project).unwrap();
        load_from(&global_path, &project_path)
    }

    #[test]
    fn missing_files_fall_back_to_defaults() {
        let dir = TempDir::new().unwrap();
        let config = load_from(&dir.path().join("none.toml"), &dir.path().join("none.toml"))
            .expect("defaults load");
        assert_eq!(config.daemon.host, "127.0.0.1");
        assert_eq!(config.daemon.port, 47129);
        assert_eq!(config.terminal.session_limit, 8);
        assert_eq!(config.terminal.global_limit, 32);
        assert_eq!(config.terminal.lease_ttl_seconds, 30);
        assert_eq!(config.terminal.max_message_bytes, 1024 * 1024);
        assert_eq!(
            config.workspace.root,
            dirs::home_dir()
                .unwrap()
                .join("Projects")
                .join(".workspace")
        );
        // Schema-only sections have defaults but no consumer yet.
        assert_eq!(config.sandbox.provider, "local");
        assert_eq!(config.cube.endpoint, None);
    }

    #[test]
    fn project_layer_overrides_global_layer() {
        let config = load_both(
            r#"[daemon]
port = 47000

[workspace]
root = "/global/ws"

[terminal]
session_limit = 4
"#,
            r#"[daemon]
port = 48000

[terminal]
global_limit = 16
"#,
        )
        .expect("layers merge");
        assert_eq!(config.daemon.port, 48000);
        assert_eq!(config.daemon.host, "127.0.0.1");
        assert_eq!(config.terminal.session_limit, 4);
        assert_eq!(config.terminal.global_limit, 16);
        assert_eq!(config.workspace.root, PathBuf::from("/global/ws"));
    }

    #[test]
    fn tilde_paths_expand() {
        let config = load_both(
            r#"[workspace]
root = "~/Projects/.workspace"

[terminal]
socket_path = "~/.local/state/qingluan/daemon.sock"
storage_root = "~/.local/state/qingluan/terminal"
"#,
            "",
        )
        .expect("tilde config loads");
        let home = dirs::home_dir().unwrap();
        assert_eq!(config.workspace.root, home.join("Projects/.workspace"));
        assert_eq!(
            config.terminal.socket_path,
            home.join(".local/state/qingluan/daemon.sock")
        );
        assert_eq!(
            config.terminal.storage_root,
            home.join(".local/state/qingluan/terminal")
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = load_both(
            r#"[daemon]
prot = 47000
"#,
            "",
        )
        .expect_err("typo'd key must fail");
        assert!(err.to_string().contains("unknown field"), "got: {err}");
    }

    #[test]
    fn unknown_sections_are_rejected() {
        let err = load_both(
            r#"[sync]
exclude = ["target"]
"#,
            "",
        )
        .expect_err("unknown section must fail");
        assert!(err.to_string().contains("unknown field"), "got: {err}");
    }

    #[test]
    fn wrong_types_are_rejected() {
        let err = load_both(
            r#"[daemon]
port = "not-a-number"
"#,
            "",
        )
        .expect_err("type error must fail");
        assert!(!err.to_string().is_empty());
    }
}

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

/// Daemon listen address. Consumed by `qingluan-daemon` and by the CLI's
/// default `--daemon-url`.
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
"#,
            r#"[daemon]
port = 48000
"#,
        )
        .expect("layers merge");
        assert_eq!(config.daemon.port, 48000);
        assert_eq!(config.daemon.host, "127.0.0.1");
        assert_eq!(config.workspace.root, PathBuf::from("/global/ws"));
    }

    #[test]
    fn tilde_in_workspace_root_expands() {
        let config = load_both(
            r#"[workspace]
root = "~/Projects/.workspace"
"#,
            "",
        )
        .expect("tilde config loads");
        assert_eq!(
            config.workspace.root,
            dirs::home_dir()
                .unwrap()
                .join("Projects")
                .join(".workspace")
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

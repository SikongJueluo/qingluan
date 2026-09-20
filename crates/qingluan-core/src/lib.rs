/// Returns the current version of the qingluan-core crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Terminal domain types shared by the terminal, storage, and daemon crates.
pub mod terminal;

/// Workspace and Pi session discovery.
pub mod workspace;

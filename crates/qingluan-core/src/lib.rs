/// Returns the current version of the qingluan-core crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Workspace and Pi session discovery.
pub mod workspace;

//! Qingluan daemon library: shared state and review sessions.
//!
//! A lib target (besides the thin `main.rs` bin) lets integration tests
//! exercise the review extraction against a real jj subprocess.

pub mod review;

use review::ReviewStore;

/// Shared application state.
#[derive(Default)]
pub struct AppState {
    pub version: String,
    /// Live code-review sessions (daemon memory, ADR-0003).
    pub reviews: ReviewStore,
}

//! Embedded web UI: the `apps/web` build output compiled into the daemon
//! binary (ADR-0003) and served over axum.
//!
//! `include_dir!` requires `apps/web/dist` to exist at compile time — the
//! same constraint tauri's `generate_context!` already imposes. Asset
//! paths are content-hashed by the bundler, so no cache headers needed
//! for a localhost tool; every non-file path falls through to the SPA
//! entry (`index.html`) so client routes like `/review/{id}` survive
//! hard reloads.

use axum::{
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use include_dir::{Dir, include_dir};

static DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../apps/web/dist");

/// Resolve a request path to embedded file bytes plus a Content-Type.
///
/// Real files win; anything else (client-side routes such as
/// `/review/{id}`) gets the SPA entry.
pub fn resolve(path: &str) -> Option<(&'static [u8], &'static str)> {
    let clean = path.trim_start_matches('/');
    if !clean.is_empty()
        && let Some(file) = DIST.get_file(clean)
    {
        return Some((file.contents(), mime_for(clean)));
    }
    DIST.get_file("index.html")
        .map(|f| (f.contents(), "text/html; charset=utf-8"))
}

/// Content type from the file extension (bundler emits only these).
fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" | "map" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Fallback handler: embedded static file or the SPA entry.
pub async fn fallback(uri: Uri) -> Response {
    match resolve(uri.path()) {
        Some((bytes, content_type)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "web ui not embedded").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_real_assets_with_mime_type() {
        let (bytes, mime) = resolve("/index.html").expect("index.html embedded");
        assert!(bytes.starts_with(b"<!DOCTYPE html>") || bytes.starts_with(b"<!doctype html>"));
        assert!(mime.starts_with("text/html"));

        // Any bundled asset by name.
        let asset = DIST
            .get_dir("assets")
            .and_then(|d| d.files().next())
            .expect("bundled assets present");
        let (_, mime) = resolve(&format!("/{}", asset.path().display())).expect("asset resolves");
        assert!(mime.starts_with("text/") || mime.starts_with("application/"));
    }

    #[test]
    fn client_routes_fall_through_to_spa_entry() {
        let (bytes, mime) = resolve("/review/01a0dcac-cfa8-74c1-bcdd-f308b89948b7").unwrap();
        assert!(mime.starts_with("text/html"));
        assert!(bytes.starts_with(b"<!DOCTYPE html>") || bytes.starts_with(b"<!doctype html>"));
        // Root path serves the same entry.
        assert_eq!(resolve("/").unwrap().0, bytes);
    }

    #[test]
    fn unknown_paths_do_not_panic() {
        // Falls back to SPA entry as well; only a missing index.html
        // (impossible with include_dir) would yield None.
        assert!(resolve("/no/such/file.xyz").is_some());
    }
}

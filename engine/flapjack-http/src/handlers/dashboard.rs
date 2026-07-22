//! Axum handler that serves the embedded single-page dashboard application, with MIME detection and client-side routing fallback.
use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[cfg_attr(flapjack_dashboard_dist, folder = "../dashboard/dist/")]
#[cfg_attr(
    not(flapjack_dashboard_dist),
    folder = "src/handlers/dashboard_fallback/"
)]
struct DashboardAssets;

// Source of truth: files currently copied from engine/dashboard/public/ into the
// embedded dashboard root. Additions there must update this routing contract.
const ROOT_STATIC_FILES: &[&str] = &["favicon.ico", "flapjack-logo.png", "flapjack-logo.svg"];

fn is_public_dashboard_asset_path(path: &str) -> bool {
    path == "index.html" || path.starts_with("assets/") || ROOT_STATIC_FILES.contains(&path)
}

fn has_unsafe_path_syntax(path: &str) -> bool {
    path.contains('\\') || path.split('/').any(|segment| matches!(segment, "." | ".."))
}

#[cfg(test)]
pub(crate) fn dashboard_test_index_bytes() -> Vec<u8> {
    dashboard_test_asset_bytes("index.html").expect("dashboard must embed index.html")
}

#[cfg(test)]
pub(crate) fn dashboard_test_asset_bytes(path: &str) -> Option<Vec<u8>> {
    DashboardAssets::get(path).map(|file| file.data.into_owned())
}

/// Serve embedded dashboard assets with SPA-style client-side routing fallback.
///
/// Resolves the request URI against the embedded `DashboardAssets` (built from
/// `../dashboard/dist/`). Requests with no path default to `index.html`. Dots do
/// not identify assets because legal index names may contain them; Vite emits
/// bundled assets below `assets/`, and `public/` files are copied to the embed
/// root. Missing asset and known root-public file requests return 404, while all
/// other misses fall back to `index.html` for client-side routing.
pub async fn dashboard_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // rust-embed canonicalizes paths in debug builds. Reject dot-segment aliases
    // before the public-prefix check so `assets/../private.html` cannot escape
    // the assets namespace and expose another embedded artifact.
    if has_unsafe_path_syntax(path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Default to index.html for empty path
    let path = if path.is_empty() { "index.html" } else { path };

    if is_public_dashboard_asset_path(path) {
        match DashboardAssets::get(path) {
            Some(file) => {
                let mime = mime_guess::from_path(path).first_or_octet_stream();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime.as_ref())],
                    file.data,
                )
                    .into_response()
            }
            None => {
                // Missing Vite bundle asset or known root public file: genuine 404.
                StatusCode::NOT_FOUND.into_response()
            }
        }
    } else {
        // SPA route: serve index.html for client-side routing.
        match DashboardAssets::get("index.html") {
            Some(file) => Html(file.data).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

//! Serving the exported Next.js UI.
//!
//! The UI is a static export, so `fluxd` is its web server too — the appliance
//! has one listening port and no reverse proxy. Three things need handling:
//!
//! * **Directory indexes.** The export is configured with `trailingSlash: true`,
//!   so `/ports` is a directory containing `index.html`.
//! * **Unknown paths.** A deep link the export did not pre-render still has to
//!   reach the client-side router, so anything unmatched falls back to the root
//!   document rather than 404.
//! * **Caching.** Next fingerprints everything under `/_next/static`, so those
//!   are immutable; HTML must never be cached or an upgraded appliance keeps
//!   serving the previous build's markup against the new API.

use std::path::{Path, PathBuf};

use axum::http::{header, HeaderValue, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

/// Cache policy for content-hashed build output.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Cache policy for documents, which must always be revalidated.
const NO_CACHE: &str = "no-cache, no-store, must-revalidate";

/// Builds the fallback service that serves the UI.
///
/// Returns a handler rather than a `ServeDir` layer because the fallback chain
/// (file, then directory index, then root document) needs a decision the layer
/// cannot express on its own.
pub fn service(web_root: PathBuf) -> axum::routing::MethodRouter {
    axum::routing::any(move |req: Request<axum::body::Body>| {
        let web_root = web_root.clone();
        async move { serve(web_root, req).await }
    })
}

/// Resolves one request against the exported site.
async fn serve(web_root: PathBuf, req: Request<axum::body::Body>) -> Response {
    let index = web_root.join("index.html");

    // A missing export is the normal state during development, when `next dev`
    // serves the UI on its own port. Saying so beats a bare 404 that looks like a
    // routing bug.
    if !index.exists() {
        return missing_export(&web_root, req.uri());
    }

    let uri_path = req.uri().path().to_string();

    let serve_dir = ServeDir::new(&web_root)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(&index));

    match serve_dir.oneshot(req).await {
        Ok(response) => with_cache_headers(response.into_response(), &uri_path),
        Err(err) => {
            tracing::error!(%err, path = %uri_path, "failed to serve a static asset");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to read static assets").into_response()
        }
    }
}

/// Applies the cache policy for a path.
fn with_cache_headers(mut response: Response, path: &str) -> Response {
    let policy = if path.starts_with("/_next/static/") || path.starts_with("/fonts/") {
        IMMUTABLE
    } else {
        NO_CACHE
    };

    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    response
}

/// The page shown when no UI build is present.
fn missing_export(web_root: &Path, uri: &Uri) -> Response {
    tracing::warn!(
        web_root = %web_root.display(),
        path = %uri.path(),
        "no UI build found; serving the placeholder page"
    );

    let body = format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>Flux &mdash; UI not built</title>
<style>
  body {{ background:#0f1117; color:#d7d9de; font:15px/1.5 ui-sans-serif, system-ui, sans-serif;
          margin:0; display:grid; place-items:center; min-height:100vh; }}
  main {{ max-width:46rem; padding:2rem; }}
  h1 {{ color:#f2f3f5; font-size:1.5rem; letter-spacing:-0.015em; margin:0 0 .5rem; }}
  code {{ background:#1a1d26; border:1px solid #252830; border-radius:6px;
          padding:.15rem .4rem; font-family:ui-monospace, monospace; color:#00d992; }}
  p {{ color:#a2a6b0; }}
</style>
<main>
  <h1>The Flux API is running, but the UI has not been built.</h1>
  <p>Expected an exported Next.js build at <code>{}</code>.</p>
  <p>Run <code>make web-build</code> to build it, or <code>make dev</code> to serve the UI
     from the Next.js development server instead.</p>
  <p>The REST API is available at <code>/api/v1</code>.</p>
</main>"#,
        web_root.display()
    );

    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_is_cached_forever_and_documents_never_are() {
        let response = with_cache_headers(
            Response::new(axum::body::Body::empty()),
            "/_next/static/chunks/main-abc123.js",
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            IMMUTABLE,
            "content-hashed assets are safe to cache indefinitely"
        );

        for path in ["/", "/ports/", "/index.html"] {
            let response = with_cache_headers(Response::new(axum::body::Body::empty()), path);
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL).unwrap(),
                NO_CACHE,
                "{path} must be revalidated so an upgrade takes effect immediately"
            );
        }
    }

    #[test]
    fn a_stale_cache_header_from_the_file_service_is_replaced_not_appended() {
        let mut response = Response::new(axum::body::Body::empty());
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=5"));

        let response = with_cache_headers(response, "/");
        let values: Vec<_> = response.headers().get_all(header::CACHE_CONTROL).iter().collect();
        assert_eq!(values.len(), 1, "exactly one policy must survive");
        assert_eq!(values[0], NO_CACHE);
    }
}

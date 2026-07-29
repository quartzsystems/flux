//! Serving the exported Next.js UI.
//!
//! The UI is a static export, so `fluxd` is its web server too — the appliance
//! has one listening port and no reverse proxy. Three things need handling:
//!
//! * **Directory indexes.** The export is configured with `trailingSlash: true`,
//!   so `/ports` is a directory containing `index.html`.
//! * **Dynamic routes.** A run's id does not exist at build time, so the export
//!   emits one document per dynamic route under a placeholder segment
//!   (`/runs/__id__/`). This module maps `/runs/<anything>/` onto it, which is
//!   what keeps run URLs readable and linkable instead of degrading to a query
//!   parameter.
//! * **Unknown paths.** Anything still unmatched falls back to the root
//!   document, so a deep link reaches the client-side router rather than a 404.
//! * **Caching.** Next fingerprints everything under `/_next/static`, so those
//!   are immutable; HTML must never be cached or an upgraded appliance keeps
//!   serving the previous build's markup against the new API.

use std::path::{Path, PathBuf};

use axum::http::{header, HeaderValue, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

/// The placeholder segment the Next.js export uses for a dynamic route.
///
/// Must match `DYNAMIC_SEGMENT` in `web/app/runs/[id]/page.tsx`.
const DYNAMIC_SEGMENT: &str = "__id__";

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

    // A dynamic route's document lives under a placeholder segment. Rewriting
    // before the file service runs means `/runs/<uuid>/` finds it, while a real
    // file at that path still wins because the rewrite only happens when the
    // literal path does not exist.
    let req = match dynamic_route_target(&web_root, &uri_path) {
        Some(rewritten) => rewrite_path(req, &rewritten),
        None => req,
    };

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

/// Finds the exported document for a dynamic route, if the literal path misses.
///
/// Tries replacing each path segment with the placeholder, rightmost first, so
/// `/runs/<id>/report/` resolves before `/runs/<id>/` is considered. Returns
/// `None` when the literal path exists or nothing matches, in which case the
/// ordinary fallback applies.
fn dynamic_route_target(web_root: &Path, uri_path: &str) -> Option<String> {
    let segments: Vec<&str> = uri_path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }

    // A path that already resolves to a real document is never rewritten.
    if web_root.join(segments.join("/")).join("index.html").exists() {
        return None;
    }

    for position in (0..segments.len()).rev() {
        // A segment that is itself a directory in the export is a route name,
        // not an id — `/runs/` must not be rewritten to `/__id__/`.
        if web_root.join(segments[..=position].join("/")).is_dir() {
            continue;
        }

        let mut candidate: Vec<&str> = segments.clone();
        candidate[position] = DYNAMIC_SEGMENT;

        if web_root.join(candidate.join("/")).join("index.html").exists() {
            let rewritten = format!("/{}/", candidate.join("/"));
            tracing::trace!(from = %uri_path, to = %rewritten, "resolved a dynamic route");
            return Some(rewritten);
        }
    }

    None
}

/// Replaces a request's path, preserving its query string.
fn rewrite_path(req: Request<axum::body::Body>, path: &str) -> Request<axum::body::Body> {
    let (mut parts, body) = req.into_parts();

    let query = parts.uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    if let Ok(uri) = format!("{path}{query}").parse::<Uri>() {
        parts.uri = uri;
    }

    Request::from_parts(parts, body)
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

    /// Builds a fake export tree under a temporary directory.
    ///
    /// `paths` are directories to create; each gets an `index.html`, mirroring
    /// what `next build` emits with `trailingSlash: true`.
    fn export_tree(paths: &[&str]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "flux-spa-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        for path in paths {
            let dir = root.join(path);
            std::fs::create_dir_all(&dir).expect("creating the fake export");
            std::fs::write(dir.join("index.html"), "<!doctype html>").expect("writing a document");
        }
        root
    }

    #[test]
    fn a_run_id_resolves_to_the_placeholder_document() {
        let root = export_tree(&["runs", "runs/__id__"]);

        assert_eq!(
            dynamic_route_target(&root, "/runs/8f14e45f-ceea-467a-9ba5-000000000000/"),
            Some("/runs/__id__/".to_string())
        );
    }

    #[test]
    fn a_real_route_is_never_rewritten() {
        // `/runs/` is a page in its own right; rewriting it to `/__id__/` would
        // replace the history table with a run view of nothing.
        let root = export_tree(&["runs", "runs/__id__", "flows"]);

        assert_eq!(dynamic_route_target(&root, "/runs/"), None);
        assert_eq!(dynamic_route_target(&root, "/flows/"), None);
        assert_eq!(dynamic_route_target(&root, "/"), None);
    }

    #[test]
    fn a_nested_dynamic_route_resolves_before_its_parent() {
        // Milestone 3 adds /runs/<id>/report; it must not be answered with the
        // run view just because that also matches one segment earlier.
        let root = export_tree(&["runs", "runs/__id__", "runs/__id__/report"]);

        assert_eq!(
            dynamic_route_target(&root, "/runs/abc/report/"),
            Some("/runs/__id__/report/".to_string())
        );
    }

    #[test]
    fn a_path_with_no_placeholder_document_is_left_to_the_ordinary_fallback() {
        let root = export_tree(&["runs", "runs/__id__"]);
        assert_eq!(dynamic_route_target(&root, "/nonsense/deep/path/"), None);
    }

    #[test]
    fn an_asset_request_is_not_treated_as_a_dynamic_route() {
        // Assets are files, not directories with an index; the rewrite must not
        // fire for them or every chunk would 404.
        let root = export_tree(&["runs", "runs/__id__"]);
        assert_eq!(dynamic_route_target(&root, "/_next/static/chunks/main.js"), None);
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

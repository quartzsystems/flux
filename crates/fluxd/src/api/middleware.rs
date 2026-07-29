//! Request middleware.
//!
//! Authentication runs once per request and attaches an [`Identity`] to the
//! request extensions. It never rejects: deciding what an anonymous request may
//! do belongs to the extractors, which know what the handler requires. A
//! middleware that rejected here would have to know the role table too, and the
//! two would drift.

use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::extract::cookie::CookieJar;

use crate::auth::{self, Identity};
use crate::state::AppState;
use crate::store::sessions;

/// Resolves the session cookie into an identity, if it names a live session.
pub async fn authenticate(
    State(state): State<AppState>,
    jar: CookieJar,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if let Some(cookie) = jar.get(auth::SESSION_COOKIE) {
        let token_hash = auth::hash_token(cookie.value());

        match sessions::lookup(state.store.pool(), &token_hash).await {
            Ok(Some(session)) => {
                request.extensions_mut().insert(Identity {
                    session_id: session.id,
                    user_id: session.user_id,
                    username: session.username,
                    role: session.role,
                });
            }
            // An unknown or expired token is normal — a stale tab, a session that
            // timed out overnight — so this is not worth a log line per request.
            Ok(None) => {}
            // A database failure here must not be mistaken for "not logged in" by
            // anyone reading the logs later.
            Err(err) => {
                tracing::error!(%err, "could not resolve the session cookie");
            }
        }
    }

    next.run(request).await
}

/// Attaches the content security policy for the API and the UI shell.
///
/// Everything is locked to `'self'` — the appliance may be offline and loads no
/// third-party anything — and `frame-ancestors 'none'` is the clickjacking
/// defence that matters for a tool whose buttons start and stop line-rate
/// traffic.
///
/// `script-src` has to allow `'unsafe-inline'`: a Next.js static export bootstraps
/// itself from inline `<script>` tags, and with `output: 'export'` there is no
/// server to stamp a per-response nonce. Combined with `'self'`-only sources this
/// still blocks loading foreign script, which is the attack that matters here;
/// tightening it further needs the UI to move off static export.
pub fn security_headers() -> tower_http::set_header::SetResponseHeaderLayer<axum::http::HeaderValue>
{
    tower_http::set_header::SetResponseHeaderLayer::overriding(
        axum::http::header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static(
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; \
             style-src 'self' 'unsafe-inline'; \
             font-src 'self'; \
             connect-src 'self' ws: wss:; \
             object-src 'none'; \
             frame-ancestors 'none'; \
             base-uri 'self'; \
             form-action 'self'",
        ),
    )
}

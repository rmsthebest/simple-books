//! Security and caching headers applied to every response.

use axum::{
    extract::{Request, State},
    http::{HeaderName, HeaderValue, header},
    middleware::Next,
    response::Response,
};

use crate::server::AppState;

const STATIC_PREFIX: &str = "/pkg/";

pub async fn security_headers(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let is_static = request.uri().path().starts_with(STATIC_PREFIX);
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    let fixed = [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "same-origin"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
    ];
    for (name, value) in fixed {
        headers.insert(name, HeaderValue::from_static(value));
    }

    if state.config.public_url.starts_with("https://") {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }

    // Default to uncacheable, because a proxy may otherwise cache private data
    // by file extension. Static assets revalidate instead of being re-sent.
    if !headers.contains_key(header::CACHE_CONTROL) {
        let policy = if is_static {
            "no-cache"
        } else {
            "private, no-store, no-transform"
        };
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    }

    response
}

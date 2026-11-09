//! Optional protocol tracing (`KOBO_TRACE=true`), to compare against device
//! behaviour and collect regression fixtures. The token is scrubbed from
//! everything logged, and `Authorization` is never read.

use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, header},
    middleware::Next,
    response::Response,
};

use crate::server::http::trace::redact_path;

const MAX_LOGGED_BODY: usize = 1024 * 1024;
const LOGGED_REQUEST_HEADERS: [&str; 7] = [
    "user-agent",
    "host",
    "accept-encoding",
    "range",
    "content-type",
    "x-kobo-synctoken",
    "x-kobo-sync",
];

fn selected_headers(headers: &HeaderMap, names: &[&str]) -> String {
    names
        .iter()
        .filter_map(|name| {
            headers
                .get(*name)
                .and_then(|v| v.to_str().ok())
                .map(|v| format!("{name}={v:?}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn scrub(text: &[u8], token: &str) -> String {
    String::from_utf8_lossy(text).replace(token, "{token}")
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
}

pub async fn trace(request: Request, next: Next) -> Response {
    let path = super::original_path(&request);
    let token = path.split('/').nth(2).unwrap_or_default().to_owned();
    let redacted = redact_path(&path);
    let method = request.method().clone();
    let query = request.uri().query().unwrap_or_default().to_owned();

    tracing::info!(
        target: "kobo_trace",
        %method,
        path = redacted,
        query,
        headers = selected_headers(request.headers(), &LOGGED_REQUEST_HEADERS),
        "request"
    );

    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, MAX_LOGGED_BODY).await.unwrap_or_default();
    if !bytes.is_empty() {
        tracing::info!(target: "kobo_trace", path = redacted, body = scrub(&bytes, &token), "request body");
    }
    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;

    let status = response.status().as_u16();
    let summary = selected_headers(
        response.headers(),
        &[
            "content-type",
            "content-length",
            "transfer-encoding",
            "x-kobo-synctoken",
            "x-kobo-sync",
        ],
    );

    if !is_json(response.headers()) {
        tracing::info!(target: "kobo_trace", path = redacted, status, headers = summary, "response");
        return response;
    }

    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, MAX_LOGGED_BODY).await.unwrap_or_default();
    tracing::info!(
        target: "kobo_trace",
        path = redacted,
        status,
        headers = summary,
        body = scrub(&bytes, &token),
        "response"
    );
    Response::from_parts(parts, Body::from(bytes))
}

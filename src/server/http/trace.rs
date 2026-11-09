//! Request logging. Only the method, a redacted path, the status and the
//! duration are logged: never queries, `Authorization`, cookies or keys.

use std::time::Instant;

use axum::{extract::Request, middleware::Next, response::Response};

/// Path prefixes whose next segment is a secret.
const SECRET_PREFIXES: &[&str] = &["kobo", "setup"];

/// Replaces the secret segment of a path with `{token}`.
pub fn redact_path(path: &str) -> String {
    let mut segments = path.split('/');
    let mut out = Vec::new();
    while let Some(segment) = segments.next() {
        out.push(segment);
        if SECRET_PREFIXES.contains(&segment) && out.len() == 2 && segments.next().is_some() {
            out.push("{token}");
        }
    }
    out.join("/")
}

pub async fn log_requests(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = redact_path(request.uri().path());
    let start = Instant::now();

    let response = next.run(request).await;

    tracing::info!(
        target: "http",
        %method,
        path,
        status = response.status().as_u16(),
        ms = start.elapsed().as_millis() as u64,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::redact_path;

    #[test]
    fn redacts_secret_segments() {
        assert_eq!(
            redact_path("/kobo/s3cret/v1/library/sync"),
            "/kobo/{token}/v1/library/sync"
        );
        assert_eq!(redact_path("/setup/s3cret"), "/setup/{token}");
    }

    #[test]
    fn leaves_other_paths_alone() {
        assert_eq!(redact_path("/"), "/");
        assert_eq!(redact_path("/books/abc"), "/books/abc");
        assert_eq!(redact_path("/kobo"), "/kobo");
        // Only the first segment counts, so a book titled "kobo" isn't mangled.
        assert_eq!(redact_path("/books/kobo/x"), "/books/kobo/x");
    }
}

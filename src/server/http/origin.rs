//! Rejects state-changing requests that another site's page caused a browser to
//! send. `SameSite=Lax` already withholds the cookie on those; this is the second
//! lock. Kobo routes don't go through it: they have no cookie to abuse.

use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header::ORIGIN},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::server::AppState;

const SEC_FETCH_SITE: &str = "sec-fetch-site";

fn allowed(method: &Method, headers: &HeaderMap, public_url: &str) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    // Every current browser sends this one, and a page can't forge it.
    if let Some(site) = headers.get(SEC_FETCH_SITE) {
        return site.as_bytes() == b"same-origin";
    }
    // Older browsers send this on a POST. With neither, it isn't a browser at
    // all, and nothing on these routes is meant for anything else.
    headers
        .get(ORIGIN)
        .is_some_and(|origin| origin.as_bytes() == public_url.as_bytes())
}

pub async fn reject_cross_origin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if allowed(
        request.method(),
        request.headers(),
        &state.config.public_url,
    ) {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const URL: &str = "https://books.example.com";

    fn check(method: &Method, pairs: &[(&'static str, &'static str)]) -> bool {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, HeaderValue::from_static(value));
        }
        allowed(method, &headers, URL)
    }

    #[test]
    fn reads_are_always_allowed() {
        assert!(check(&Method::GET, &[("sec-fetch-site", "cross-site")]));
        assert!(check(&Method::HEAD, &[]));
    }

    #[test]
    fn writes_need_a_same_origin_browser_signal() {
        assert!(check(&Method::POST, &[("sec-fetch-site", "same-origin")]));
        assert!(check(&Method::POST, &[("origin", URL)]));
        assert!(check(&Method::DELETE, &[("sec-fetch-site", "same-origin")]));

        for site in ["same-site", "cross-site", "none"] {
            let headers = [("sec-fetch-site", site)];
            assert!(!check(&Method::POST, &headers), "{site}");
        }
        assert!(!check(&Method::POST, &[("origin", "https://evil.example")]));
        assert!(!check(
            &Method::POST,
            &[("origin", "https://books.example.com.evil.example")]
        ));
        assert!(!check(&Method::POST, &[]), "neither header");
    }

    #[test]
    fn the_stronger_signal_wins_when_they_disagree() {
        let headers = [("sec-fetch-site", "cross-site"), ("origin", URL)];
        assert!(!check(&Method::POST, &headers));
    }
}

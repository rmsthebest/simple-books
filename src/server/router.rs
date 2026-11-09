//! Assembles the application.

use axum::{Router, middleware, routing::get};
use leptos::prelude::provide_context;
use leptos_axum::{LeptosRoutes, file_and_error_handler, generate_route_list};
use tower_http::{catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer};

use super::{AppState, http, kobo, library, web};
use crate::app::{App, shell};

/// The default limit for request bodies. Upload routes raise it.
const DEFAULT_BODY_LIMIT: usize = 64 * 1024;

pub fn build(state: AppState) -> Router {
    let routes = generate_route_list(App);
    let options = state.leptos.clone();

    let pages = Router::new()
        .leptos_routes_with_context(
            &state,
            routes,
            {
                let state = state.clone();
                move || provide_context(state.clone())
            },
            {
                let options = options.clone();
                move || shell(options.clone())
            },
        )
        .fallback(file_and_error_handler::<AppState, _>(shell))
        .layer(RequestBodyLimitLayer::new(DEFAULT_BODY_LIMIT))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            web::gate::pages,
        ))
        // Outermost, so that it's the first thing to see a request.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            http::origin::reject_cross_origin,
        ));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(library::router(&state))
        .merge(web::router(&state))
        .nest("/kobo/{token}", kobo::router(state.clone()))
        // `nest` doesn't match a bare trailing slash, and the HTML fallback
        // would otherwise answer it, which makes Nickel abort the sync.
        .route_service(
            "/kobo/{token}/",
            kobo::router(state.clone()).with_state(state.clone()),
        )
        .merge(pages)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            http::headers::security_headers,
        ))
        .layer(middleware::from_fn(http::trace::log_requests))
        .layer(CatchPanicLayer::new())
        .with_state(state)
}

//! Endpoints Nickel calls but this server has no use for. Without a store
//! proxy they answer locally, with just enough for Nickel to carry on.

use axum::{
    Router,
    http::StatusCode,
    response::Response,
    routing::{any, get},
};
use rand::RngCore;
use serde_json::json;

use super::{empty_object, kobo_json};
use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/user/profile", get(profile))
        .route("/v1/deals", get(deals))
        .route("/v1/analytics/gettests", any(get_tests))
        .route("/v1/analytics/event", any(ok_empty))
        .route("/v1/user/loyalty/benefits", get(benefits))
        .route("/v1/user/wishlist", get(wishlist))
        .route("/v1/products/{id}/nextread", get(empty_array))
}

async fn profile() -> Response {
    kobo_json(
        StatusCode::OK,
        &json!({
            "AffiliateName": "simple-books",
            "IsOneStore": false,
            "IsChildAccount": false,
            "IsLibraryMigrated": true,
            "VipMembershipPurchased": true,
            "HasPurchased": true,
            "HasPurchasedBook": true,
            "HasPurchasedAudiobook": false,
            "SafeSearch": false,
        }),
    )
}

async fn deals() -> Response {
    kobo_json(StatusCode::OK, &json!({ "Deals": [] }))
}

async fn get_tests() -> Response {
    let mut bytes = [0u8; 6];
    rand::rng().fill_bytes(&mut bytes);
    kobo_json(
        StatusCode::OK,
        &json!({ "Result": "Success", "TestKey": hex::encode(bytes), "Tests": {} }),
    )
}

async fn benefits() -> Response {
    kobo_json(StatusCode::OK, &json!({ "Benefits": {} }))
}

async fn wishlist() -> Response {
    kobo_json(StatusCode::OK, &json!({ "Items": [], "TotalCount": 0 }))
}

async fn empty_array() -> Response {
    kobo_json(StatusCode::OK, &json!([]))
}

async fn ok_empty() -> Response {
    empty_object(StatusCode::OK)
}

/// Everything else. HTML or an error here makes Nickel abort the sync.
pub async fn fallback() -> Response {
    empty_object(StatusCode::OK)
}

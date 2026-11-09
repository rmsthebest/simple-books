//! `POST /v1/auth/device` and `/v1/auth/refresh`. The device is already
//! authenticated by its path token, so these tokens are placeholders that only
//! keep it happy. Nothing ever verifies them.

use axum::{body::Bytes, extract::rejection::BytesRejection, http::StatusCode, response::Response};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use serde_json::{Value, json};
use uuid::Uuid;

use super::kobo_json;

fn placeholder_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    STANDARD.encode(bytes)
}

pub async fn device(body: Result<Bytes, BytesRejection>) -> Response {
    let user_key = body
        .ok()
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .and_then(|v| v.get("UserKey").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();

    kobo_json(
        StatusCode::OK,
        &json!({
            "AccessToken": placeholder_token(),
            "RefreshToken": placeholder_token(),
            "TokenType": "Bearer",
            "TrackingId": Uuid::new_v4(),
            "UserKey": user_key,
        }),
    )
}

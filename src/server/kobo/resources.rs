//! `GET /v1/initialization`: tells the device where everything lives.

use axum::{
    Extension,
    extract::State,
    http::{HeaderValue, StatusCode},
    response::Response,
};
use serde_json::{Map, Value, json};

use super::{KoboContext, kobo_json};
use crate::server::AppState;

/// A snapshot of the dictionary Kobo's store returns, as known from community
/// implementations. To be refreshed from a device's `Kobo eReader.conf`.
const BASE: &str = include_str!("resources_base.json");
const STORE_ORIGIN: &str = "https://storeapi.kobo.com";

/// Sent on initialization and downloads. It's base64 for `{}`.
pub const API_TOKEN_HEADER: &str = "x-kobo-apitoken";
pub const API_TOKEN: &str = "e30=";

#[derive(Debug)]
pub struct Resources {
    base: Map<String, Value>,
}

impl Resources {
    pub fn load() -> Result<Self, serde_json::Error> {
        Ok(Self {
            base: serde_json::from_str(BASE)?,
        })
    }

    /// Builds the dictionary for a device. Every store URL is rewritten onto
    /// `base_url`, so nothing the store would have served goes to Kobo. Other
    /// Kobo hosts, such as reading services and sign-in, are left as they are.
    pub fn build(&self, base_url: &str) -> Value {
        let mut resources = self.base.clone();

        for value in resources.values_mut() {
            if let Value::String(text) = value
                && text.contains(STORE_ORIGIN)
            {
                *text = text.replace(STORE_ORIGIN, base_url);
            }
        }

        let overrides = [
            ("image_host", base_url.to_owned()),
            (
                "image_url_template",
                format!("{base_url}/covers/{{ImageId}}/{{Width}}/{{Height}}/false/image.jpg"),
            ),
            (
                "image_url_quality_template",
                format!(
                    "{base_url}/covers/{{ImageId}}/{{Width}}/{{Height}}/{{Quality}}/{{IsGreyscale}}/image.jpg"
                ),
            ),
            ("kobo_nativeborrow_enabled", "False".to_owned()),
        ];
        for (key, value) in overrides {
            resources.insert(key.to_owned(), Value::String(value));
        }

        json!({ "Resources": resources })
    }
}

pub async fn initialization(
    State(state): State<AppState>,
    Extension(ctx): Extension<KoboContext>,
) -> Response {
    let mut response = kobo_json(StatusCode::OK, &state.kobo_resources.build(&ctx.base_url));
    response
        .headers_mut()
        .insert(API_TOKEN_HEADER, HeaderValue::from_static(API_TOKEN));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_URL: &str = "https://books.example.com/kobo/tok";

    fn built() -> Value {
        Resources::load()
            .expect("embedded dictionary is valid")
            .build(BASE_URL)
    }

    #[test]
    fn required_keys_point_at_this_server() {
        let built = built();
        let resources = &built["Resources"];
        for key in [
            "library_sync",
            "library_metadata",
            "library_book",
            "content_access_book",
            "reading_state",
            "delete_entitlement",
            "user_profile",
            "deals",
            "get_tests_request",
            "post_analytics_event",
            "image_host",
            "image_url_template",
            "image_url_quality_template",
        ] {
            let value = resources[key]
                .as_str()
                .unwrap_or_else(|| panic!("{key} is missing"));
            assert!(value.starts_with(BASE_URL), "{key} is {value}");
        }
    }

    #[test]
    fn no_store_urls_remain() {
        assert!(!built().to_string().contains(STORE_ORIGIN));
    }

    #[test]
    fn native_borrow_is_off() {
        assert_eq!(built()["Resources"]["kobo_nativeborrow_enabled"], "False");
    }
}

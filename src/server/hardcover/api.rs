//! Talking to Hardcover: OAuth's sign-in with a code (RFC 8628), refreshing and
//! revoking tokens, and GraphQL. Nothing here logs a token or a response body.

use std::time::Duration;

use reqwest::{StatusCode, header};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Hardcover allows queries 30 seconds. Sign-in steps are quick.
const QUERY_TIMEOUT: Duration = Duration::from_secs(35);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Larger than anything asked for here could be.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Only what this needs: who you are, finding books, and your own shelves.
pub const SCOPES: &str = "read:me:content read:catalog:search read:library write:library";

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The token isn't accepted. Refresh, or connect again.
    #[error("not accepted")]
    Unauthorized,
    /// Try again after this long.
    #[error("rate limited")]
    RateLimited(Duration),
    /// Hardcover or the network had a problem that may pass.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Hardcover understood and refused. Trying again won't help.
    #[error("refused: {0}")]
    Refused(String),
}

impl From<reqwest::Error> for ApiError {
    fn from(error: reqwest::Error) -> Self {
        // reqwest's message can include the URL, which never holds a secret here.
        Self::Unavailable(error.without_url().to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: i64,
    #[serde(default = "default_interval")]
    pub interval: i64,
}

fn default_interval() -> i64 {
    5
}

#[derive(Clone, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum Poll {
    Waiting,
    SlowDown,
    Approved(Tokens),
    Denied,
    Expired,
}

#[derive(Debug, Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
    client_id: String,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

impl Api {
    pub fn new(base: &str, client_id: &str) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "simple-books/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/rmsthebest/simple-books)"
            ))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(QUERY_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base: base.to_owned(),
            client_id: client_id.to_owned(),
        })
    }

    async fn form(
        &self,
        path: &str,
        fields: &[(&str, &str)],
    ) -> Result<(StatusCode, Vec<u8>), ApiError> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .header(header::ACCEPT, "application/json")
            .form(fields)
            .send()
            .await?;
        let status = response.status();
        Ok((status, read_limited(response).await?))
    }

    /// Asks for a code for the user to enter on Hardcover.
    pub async fn start_sign_in(&self) -> Result<DeviceStart, ApiError> {
        let (status, body) = self
            .form(
                "/oauth2/device",
                &[("client_id", &self.client_id), ("scope", SCOPES)],
            )
            .await?;
        if !status.is_success() {
            return Err(status_error(status, None));
        }
        serde_json::from_slice(&body)
            .map_err(|_| ApiError::Unavailable("odd device response".to_owned()))
    }

    /// Whether the user has entered the code yet.
    pub async fn poll_sign_in(&self, device_code: &str) -> Result<Poll, ApiError> {
        let (status, body) = self
            .form(
                "/oauth2/token",
                &[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device_code),
                    ("client_id", &self.client_id),
                ],
            )
            .await?;
        if status.is_success() {
            return serde_json::from_slice(&body)
                .map(Poll::Approved)
                .map_err(|_| ApiError::Unavailable("odd token response".to_owned()));
        }
        match serde_json::from_slice::<OAuthError>(&body).map(|e| e.error) {
            Ok(error) => Ok(match error.as_str() {
                "authorization_pending" => Poll::Waiting,
                "slow_down" => Poll::SlowDown,
                "access_denied" => Poll::Denied,
                "expired_token" => Poll::Expired,
                other => return Err(ApiError::Refused(other.chars().take(64).collect())),
            }),
            Err(_) => Err(status_error(status, None)),
        }
    }

    /// New tokens. The refresh token is spent whether or not the answer arrives, so
    /// `Unavailable` here may mean the connection is gone.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, ApiError> {
        let (status, body) = self
            .form(
                "/oauth2/token",
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                    ("client_id", &self.client_id),
                ],
            )
            .await?;
        if status.is_success() {
            return serde_json::from_slice(&body)
                .map_err(|_| ApiError::Unavailable("odd token response".to_owned()));
        }
        match serde_json::from_slice::<OAuthError>(&body).map(|e| e.error) {
            Ok(error) if error == "invalid_grant" || error == "invalid_client" => {
                Err(ApiError::Unauthorized)
            }
            _ => Err(status_error(status, None)),
        }
    }

    /// Ends the connection on Hardcover's side too. Best effort: the tokens are
    /// forgotten here either way.
    pub async fn revoke(&self, refresh_token: &str) {
        let result = self
            .form(
                "/oauth2/revoke",
                &[
                    ("token", refresh_token),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", &self.client_id),
                ],
            )
            .await;
        if let Err(error) = result {
            tracing::warn!(%error, "couldn't revoke a Hardcover connection");
        }
    }

    /// Runs a query. `data` is returned as the type asked for; any GraphQL error, or
    /// an `error` a mutation reports, counts as a refusal.
    pub async fn graphql<T: DeserializeOwned>(
        &self,
        token: &str,
        query: &str,
        variables: Value,
    ) -> Result<T, ApiError> {
        let response = self
            .http
            .post(format!("{}/v1/graphql", self.base))
            .bearer_auth(token)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = read_limited(response).await?;
        if !status.is_success() {
            return Err(status_error(status, retry_after));
        }

        let mut parsed: Value = serde_json::from_slice(&body)
            .map_err(|_| ApiError::Unavailable("odd GraphQL response".to_owned()))?;
        if let Some(errors) = parsed.get("errors").and_then(Value::as_array) {
            let first = errors.first();
            let code = first
                .and_then(|e| e.pointer("/extensions/code"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(code, "invalid-jwt" | "invalid_token" | "access-denied") {
                return Err(ApiError::Unauthorized);
            }
            let message = first
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(ApiError::Refused(message.chars().take(200).collect()));
        }
        let data = parsed
            .get_mut("data")
            .map(Value::take)
            .ok_or_else(|| ApiError::Unavailable("no data".to_owned()))?;
        serde_json::from_value(data)
            .map_err(|error| ApiError::Unavailable(format!("unexpected answer: {error}")))
    }
}

fn status_error(status: StatusCode, retry_after: Option<u64>) -> ApiError {
    match status {
        StatusCode::UNAUTHORIZED => ApiError::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => ApiError::RateLimited(Duration::from_secs(
            retry_after.unwrap_or(60).clamp(1, 86_400),
        )),
        StatusCode::REQUEST_TIMEOUT => ApiError::Unavailable("timed out".to_owned()),
        status if status.is_server_error() => ApiError::Unavailable(format!("status {status}")),
        status => ApiError::Refused(format!("status {status}")),
    }
}

async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(ApiError::Unavailable("response too large".to_owned()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::Unavailable("response too large".to_owned()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

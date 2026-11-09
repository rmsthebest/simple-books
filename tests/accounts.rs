//! Setup links, logins, sessions and device credentials.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::{
    Json, Router,
    body::Body,
    http::{Method, Request, StatusCode, header::COOKIE},
    response::IntoResponse,
    routing::get,
};
use common::TestServer;
use serde_json::{Value, json};
use simple_books::server::{
    auth::{
        accounts::{self, AccountError},
        device::{generate_secret, hash_secret},
        session::{self, AdminUser, COOKIE_NAME, CurrentUser},
    },
    db::Db,
    now,
};
use tower::ServiceExt;

const QUOTA: i64 = 1024 * 1024;
const PASSWORD: &str = "correct horse battery";

fn db(server: &TestServer) -> &Db {
    &server.state.db
}

fn token_of(link: &str) -> &str {
    link.rsplit('/').next().unwrap()
}

/// A user who has finished setup, and the session that left them with.
async fn signed_up(server: &TestServer, name: &str, admin: bool) -> (i64, String) {
    let id = accounts::create_user(db(server), name, QUOTA, admin)
        .await
        .unwrap();
    let link = accounts::setup_link(db(server), "https://x", id)
        .await
        .unwrap();
    let session = accounts::complete_setup(db(server), token_of(&link), PASSWORD.to_owned())
        .await
        .unwrap();
    (id, session)
}

/// A small app with the real extractors, since the pages using them come later.
fn app(server: &TestServer) -> Router {
    Router::new()
        .route(
            "/me",
            get(|user: CurrentUser| async move {
                Json(json!({ "id": user.id, "name": user.username, "scope": user.scope.user_id() }))
            }),
        )
        .route(
            "/admin",
            get(|AdminUser(user): AdminUser| async move { user.username.into_response() }),
        )
        .with_state(server.state.clone())
}

async fn call(server: &TestServer, path: &str, session: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut request = Request::builder().method(Method::GET).uri(path);
    if let Some(token) = session {
        request = request.header(COOKIE, format!("{COOKIE_NAME}={token}"));
    }
    let response = app(server)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body.to_vec())
}

#[tokio::test]
async fn setup_link_sets_the_password_once() {
    let server = TestServer::start().await;
    let id = accounts::create_user(db(&server), "ann", QUOTA, false)
        .await
        .unwrap();

    // Nobody can log in before setup, not even with an empty password.
    for attempt in ["", PASSWORD] {
        let login = accounts::log_in(db(&server), "ann", attempt.to_owned());
        assert!(login.await.unwrap().is_none(), "{attempt:?}");
    }

    let link = accounts::setup_link(db(&server), "https://x", id)
        .await
        .unwrap();
    assert!(link.starts_with("https://x/setup/"), "{link}");
    let token = token_of(&link);
    assert_eq!(
        accounts::setup_link_user(db(&server), token).await.unwrap(),
        Some("ann".to_owned())
    );

    // A refused password doesn't use the link up.
    assert!(matches!(
        accounts::complete_setup(db(&server), token, "short".to_owned()).await,
        Err(AccountError::PasswordTooShort)
    ));

    let session = accounts::complete_setup(db(&server), token, PASSWORD.to_owned())
        .await
        .unwrap();
    assert_eq!(call(&server, "/me", Some(&session)).await.0, StatusCode::OK);

    assert!(matches!(
        accounts::complete_setup(db(&server), token, "another good password".to_owned()).await,
        Err(AccountError::InvalidSetupLink)
    ));
    assert_eq!(
        accounts::setup_link_user(db(&server), token).await.unwrap(),
        None
    );
    let login = accounts::log_in(db(&server), "ann", PASSWORD.to_owned());
    assert!(login.await.unwrap().is_some());
}

#[tokio::test]
async fn setup_link_fails_when_expired_or_replaced() {
    let server = TestServer::start().await;
    let id = accounts::create_user(db(&server), "ann", QUOTA, false)
        .await
        .unwrap();

    let expired = generate_secret();
    db(&server)
        .replace_setup_token(id, &hash_secret(&expired), now() - 1)
        .await
        .unwrap();
    assert!(matches!(
        accounts::complete_setup(db(&server), &expired, PASSWORD.to_owned()).await,
        Err(AccountError::InvalidSetupLink)
    ));

    let first = accounts::setup_link(db(&server), "https://x", id)
        .await
        .unwrap();
    let second = accounts::setup_link(db(&server), "https://x", id)
        .await
        .unwrap();
    assert!(matches!(
        accounts::complete_setup(db(&server), token_of(&first), PASSWORD.to_owned()).await,
        Err(AccountError::InvalidSetupLink)
    ));
    accounts::complete_setup(db(&server), token_of(&second), PASSWORD.to_owned())
        .await
        .unwrap();
}

#[tokio::test]
async fn usernames_clash_regardless_of_case() {
    let server = TestServer::start().await;
    accounts::create_user(db(&server), "Ann", QUOTA, false)
        .await
        .unwrap();
    assert!(matches!(
        accounts::create_user(db(&server), "ann", QUOTA, false).await,
        Err(AccountError::UsernameTaken)
    ));
    assert!(matches!(
        accounts::create_user(db(&server), " bob", QUOTA, false).await,
        Err(AccountError::InvalidUsername)
    ));
}

#[tokio::test]
async fn failed_logins_look_the_same() {
    let server = TestServer::start().await;
    signed_up(&server, "ann", false).await;
    accounts::create_user(db(&server), "unfinished", QUOTA, false)
        .await
        .unwrap();

    for (user, password) in [
        ("ann", "wrong password"),
        ("nobody", PASSWORD),
        ("unfinished", PASSWORD),
    ] {
        let login = accounts::log_in(db(&server), user, password.to_owned());
        assert!(login.await.unwrap().is_none(), "{user}");
    }
    let login = accounts::log_in(db(&server), "ANN", PASSWORD.to_owned());
    assert!(login.await.unwrap().is_some(), "any case signs in");
}

#[tokio::test]
async fn each_login_is_its_own_session() {
    let server = TestServer::start().await;
    let (_, first) = signed_up(&server, "ann", false).await;
    let second = accounts::log_in(db(&server), "ann", PASSWORD.to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first, second, "a new token at every login");

    // Signing out ends only this one.
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(COOKIE, format!("{COOKIE_NAME}={first}").parse().unwrap());
    session::end(db(&server), &headers).await.unwrap();
    assert_eq!(
        call(&server, "/me", Some(&first)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(call(&server, "/me", Some(&second)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn password_reset_ends_every_other_session() {
    let server = TestServer::start().await;
    let (id, old_session) = signed_up(&server, "ann", false).await;

    let link = accounts::setup_link(db(&server), "https://x", id)
        .await
        .unwrap();
    let new_session = accounts::complete_setup(
        db(&server),
        token_of(&link),
        "a brand new password".to_owned(),
    )
    .await
    .unwrap();

    assert_eq!(
        call(&server, "/me", Some(&old_session)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&server, "/me", Some(&new_session)).await.0,
        StatusCode::OK
    );
    let old = accounts::log_in(db(&server), "ann", PASSWORD.to_owned());
    assert!(old.await.unwrap().is_none());
}

#[tokio::test]
async fn session_extractor_gives_the_right_user_and_refuses_the_rest() {
    let server = TestServer::start().await;
    let (ann, ann_session) = signed_up(&server, "ann", false).await;
    let (_, bob_session) = signed_up(&server, "bob", true).await;

    assert_eq!(call(&server, "/me", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(&server, "/me", Some("forged")).await.0,
        StatusCode::UNAUTHORIZED
    );

    let (status, body) = call(&server, "/me", Some(&ann_session)).await;
    assert_eq!(status, StatusCode::OK);
    let me: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(me, json!({ "id": ann, "name": "ann", "scope": ann }));

    assert_eq!(
        call(&server, "/admin", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&server, "/admin", Some(&ann_session)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&server, "/admin", Some(&bob_session)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn expired_session_is_refused_and_cleared() {
    let server = TestServer::start().await;
    let (id, _) = signed_up(&server, "ann", false).await;
    let token = generate_secret();
    db(&server)
        .create_session(&hash_secret(&token), id, now() - 1)
        .await
        .unwrap();
    assert_eq!(
        call(&server, "/me", Some(&token)).await.0,
        StatusCode::UNAUTHORIZED
    );

    db(&server).delete_expired_credentials().await.unwrap();
    db(&server)
        .create_session(&hash_secret(&token), id, now() + 60)
        .await
        .expect("the expired row was removed, so its hash is free again");
}

#[tokio::test]
async fn disabling_a_user_stops_everything_until_re_enabled() {
    let server = TestServer::start().await;
    let (id, session) = signed_up(&server, "ann", false).await;
    let device_token = generate_secret();
    db(&server)
        .create_device(id, "kobo", &hash_secret(&device_token))
        .await
        .unwrap();
    let link = accounts::setup_link(db(&server), "https://x", id)
        .await
        .unwrap();

    let sync_path = format!("/kobo/{device_token}/v1/library/sync");
    assert_eq!(server.get(&sync_path).await.status, 200);

    assert!(db(&server).set_user_disabled(id, true).await.unwrap());

    assert_eq!(
        call(&server, "/me", Some(&session)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let login = accounts::log_in(db(&server), "ann", PASSWORD.to_owned());
    assert!(login.await.unwrap().is_none());
    assert_eq!(server.get(&sync_path).await.status, 401);
    assert!(matches!(
        accounts::complete_setup(db(&server), token_of(&link), PASSWORD.to_owned()).await,
        Err(AccountError::InvalidSetupLink)
    ));

    assert!(db(&server).set_user_disabled(id, false).await.unwrap());

    let login = accounts::log_in(db(&server), "ann", PASSWORD.to_owned());
    assert!(login.await.unwrap().is_some());
    assert_eq!(server.get(&sync_path).await.status, 200);
    assert_eq!(
        call(&server, "/me", Some(&session)).await.0,
        StatusCode::UNAUTHORIZED,
        "old sessions stay ended"
    );
}

/// Secrets exist only as hashes, so a stolen database can't be used to get in.
#[tokio::test]
async fn no_secret_is_stored_as_given() {
    let server = TestServer::start().await;
    let id = accounts::create_user(db(&server), "ann", QUOTA, false)
        .await
        .unwrap();
    let link = accounts::setup_link(db(&server), "https://x", id)
        .await
        .unwrap();
    let setup = token_of(&link).to_owned();
    let unused_link = accounts::setup_link(db(&server), "https://x", id)
        .await
        .unwrap();
    let session =
        accounts::complete_setup(db(&server), token_of(&unused_link), PASSWORD.to_owned())
            .await
            .unwrap();
    let device = generate_secret();
    db(&server)
        .create_device(id, "kobo", &hash_secret(&device))
        .await
        .unwrap();

    let mut stored = Vec::new();
    for entry in std::fs::read_dir(server.state.layout.root()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            stored.extend(std::fs::read(path).unwrap());
        }
    }
    assert!(
        stored.windows(4).any(|window| window == b"$arg"),
        "the check must be reading where the data lives"
    );
    for (name, secret) in [
        ("setup link", setup.as_str()),
        ("session", session.as_str()),
        ("device", device.as_str()),
        ("password", PASSWORD),
    ] {
        let found = stored
            .windows(secret.len())
            .any(|window| window == secret.as_bytes());
        assert!(!found, "{name} is in the database as plain text");
    }
}

fn sync_status(server: &TestServer, token: &str) -> impl Future<Output = StatusCode> {
    let path = format!("/kobo/{token}/v1/library/sync");
    async move { server.get(&path).await.status }
}

/// A patched config that's never applied must not break a working Kobo.
#[tokio::test]
async fn reconnecting_keeps_the_old_token_until_a_new_one_is_seen() {
    let server = TestServer::start().await;
    let (ann, _) = signed_up(&server, "ann", false).await;
    let old = generate_secret();
    let old_id = db(&server)
        .create_device(ann, "Clara", &hash_secret(&old))
        .await
        .unwrap();

    // Two patched files handed out; either may be the one that gets applied.
    let unused = generate_secret();
    let applied = generate_secret();
    for secret in [&unused, &applied] {
        db(&server)
            .begin_reconnect(ann, old_id, &hash_secret(secret))
            .await
            .unwrap()
            .unwrap();
    }

    assert_eq!(sync_status(&server, &old).await, StatusCode::OK);
    let devices = db(&server).list_devices(ann).await.unwrap();
    assert_eq!(
        devices.len(),
        1,
        "a reconnect in progress isn't another device"
    );
    assert_eq!(devices[0].id, old_id);

    assert_eq!(sync_status(&server, &applied).await, StatusCode::OK);

    assert_eq!(sync_status(&server, &old).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        sync_status(&server, &unused).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(sync_status(&server, &applied).await, StatusCode::OK);
    let devices = db(&server).list_devices(ann).await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_ne!(devices[0].id, old_id);
    assert_eq!(devices[0].name, "Clara", "it keeps its name");
    assert!(devices[0].last_seen_at.is_some());
}

/// The cursor belongs to the user, so a new token doesn't mean a full resend.
#[tokio::test]
async fn reconnecting_continues_from_the_same_place() {
    let server = TestServer::start().await;
    let (scope, token) = server.user("sam").await;
    server.add_book(&scope, "a book").await;
    let first = server.sync(&token, None).await;
    let cursor = first.header("x-kobo-synctoken").unwrap().to_owned();

    let user = scope.user_id();
    let device = db(&server).list_devices(user).await.unwrap()[0].id;
    let new_token = generate_secret();
    db(&server)
        .begin_reconnect(user, device, &hash_secret(&new_token))
        .await
        .unwrap()
        .unwrap();

    let reply = server.sync(&new_token, Some(&cursor)).await;
    assert_eq!(reply.json(), json!([]));
}

#[tokio::test]
async fn one_user_cannot_touch_anothers_device() {
    let server = TestServer::start().await;
    let (ann, _) = signed_up(&server, "ann", false).await;
    let (bob, _) = signed_up(&server, "bob", false).await;
    let token = generate_secret();
    let device = db(&server)
        .create_device(ann, "Clara", &hash_secret(&token))
        .await
        .unwrap();

    let stolen = generate_secret();
    assert_eq!(
        db(&server)
            .begin_reconnect(bob, device, &hash_secret(&stolen))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        sync_status(&server, &stolen).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(!db(&server).revoke_device(bob, device).await.unwrap());
    assert!(!db(&server).request_full_sync(bob, device).await.unwrap());
    assert!(db(&server).list_devices(bob).await.unwrap().is_empty());
    assert_eq!(sync_status(&server, &token).await, StatusCode::OK);

    assert!(db(&server).revoke_device(ann, device).await.unwrap());
    assert_eq!(sync_status(&server, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoking_a_device_also_cancels_a_reconnect_in_progress() {
    let server = TestServer::start().await;
    let (ann, _) = signed_up(&server, "ann", false).await;
    let device = db(&server)
        .create_device(ann, "Clara", &hash_secret(&generate_secret()))
        .await
        .unwrap();
    let pending = generate_secret();
    db(&server)
        .begin_reconnect(ann, device, &hash_secret(&pending))
        .await
        .unwrap()
        .unwrap();

    assert!(db(&server).revoke_device(ann, device).await.unwrap());
    assert_eq!(
        sync_status(&server, &pending).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn cross_site_posts_are_refused_but_kobo_routes_are_unaffected() {
    let server = TestServer::start().await;
    let (_, token) = server.user("sam").await;

    let cross = server
        .request(
            Method::POST,
            "/api/anything",
            &[("sec-fetch-site", "cross-site")],
            Vec::new(),
        )
        .await;
    assert_eq!(cross.status, 403);
    let bare = server
        .request(Method::POST, "/api/anything", &[], Vec::new())
        .await;
    assert_eq!(bare.status, 403);
    let same = server
        .request(
            Method::POST,
            "/api/anything",
            &[("sec-fetch-site", "same-origin")],
            Vec::new(),
        )
        .await;
    assert_ne!(same.status, 403);

    // Nickel sends neither header.
    let path = format!("/kobo/{token}/v1/auth/device");
    let kobo = server
        .request(Method::POST, &path, &[], b"{}".to_vec())
        .await;
    assert_eq!(kobo.status, 200);
}

#[tokio::test]
async fn there_is_no_way_to_sign_up() {
    let server = TestServer::start().await;
    for path in [
        "/signup",
        "/register",
        "/api/signup",
        "/api/register",
        "/users",
    ] {
        let reply = server
            .request(
                Method::POST,
                path,
                &[("sec-fetch-site", "same-origin")],
                b"username=x&password=y".to_vec(),
            )
            .await;
        // No route takes it: unknown addresses are 404, and pages refuse a POST.
        assert!(
            matches!(reply.status.as_u16(), 404 | 405),
            "{path}: {}",
            reply.status
        );
    }
    assert!(!db(&server).has_users().await.unwrap());
}

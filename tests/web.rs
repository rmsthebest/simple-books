//! The website, through the same routes a browser uses, with no JavaScript: every
//! request here is what a plain form sends.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;

use axum::http::Method;
use common::{Reply, TestServer};
use serde_json::json;
use simple_books::server::{
    auth::{accounts, session::COOKIE_NAME},
    db::{models::Visibility, scope::UserScope},
    testkit,
};
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

fn html(reply: &Reply) -> String {
    String::from_utf8(reply.body.clone()).unwrap()
}

fn location(reply: &Reply) -> &str {
    assert_eq!(reply.status, 303, "{}", html(reply));
    reply.header("location").unwrap()
}

fn encode(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut out = String::new();
    for (name, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(name);
        out.push('=');
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() {
                out.push(char::from(byte));
            } else {
                write!(out, "%{byte:02X}").unwrap();
            }
        }
    }
    out.into_bytes()
}

/// What a browser sends for a plain form on this site.
async fn post(server: &TestServer, path: &str, cookie: &str, pairs: &[(&str, &str)]) -> Reply {
    server
        .request(
            Method::POST,
            path,
            &[
                ("cookie", cookie),
                ("content-type", "application/x-www-form-urlencoded"),
                ("sec-fetch-site", "same-origin"),
            ],
            encode(pairs),
        )
        .await
}

async fn page(server: &TestServer, path: &str, cookie: &str) -> Reply {
    server
        .request(Method::GET, path, &[("cookie", cookie)], Vec::new())
        .await
}

/// A user with a password, signed in through the real form.
async fn sign_up(server: &TestServer, name: &str, admin: bool) -> (UserScope, String) {
    let db = &server.state.db;
    let id = accounts::create_user(db, name, 1024 * 1024 * 1024, admin)
        .await
        .unwrap();
    let link = accounts::setup_link(db, common::PUBLIC_URL, id)
        .await
        .unwrap();
    let path = link.strip_prefix(common::PUBLIC_URL).unwrap().to_owned();
    let reply = post(
        server,
        &path,
        "",
        &[("password", PASSWORD), ("confirm", PASSWORD)],
    )
    .await;
    assert_eq!(location(&reply), "/");
    let cookie = cookie_from(&reply);
    (testkit::scope(db, id), cookie)
}

fn cookie_from(reply: &Reply) -> String {
    let set = reply.header("set-cookie").unwrap();
    set.split(';').next().unwrap().to_owned()
}

fn id_in(body: &str, marker: &str) -> Option<String> {
    let at = body.find(marker)? + marker.len();
    Some(
        body[at..]
            .chars()
            .take_while(|c| *c != '"' && *c != '&')
            .collect(),
    )
}

mod signing_in {
    use super::*;

    #[tokio::test]
    async fn every_signed_in_page_sends_strangers_to_the_login_page() {
        let server = TestServer::start().await;
        for path in [
            "/",
            "/shared",
            "/device",
            "/admin/users",
            "/books/anything",
            "/nope",
        ] {
            assert_eq!(location(&server.get(path).await), "/login", "{path}");
        }
        assert_eq!(server.get("/login").await.status, 200);
    }

    #[tokio::test]
    async fn logging_in_and_out() {
        let server = TestServer::start().await;
        let (_, first) = sign_up(&server, "ann", false).await;

        let reply = post(
            &server,
            "/login",
            "",
            &[("username", "ann"), ("password", PASSWORD)],
        )
        .await;
        assert_eq!(location(&reply), "/");
        let set = reply.header("set-cookie").unwrap();
        for part in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
            assert!(set.contains(part), "{set}");
        }
        assert!(set.starts_with(COOKIE_NAME));
        let cookie = cookie_from(&reply);
        assert_ne!(cookie, first, "each login gets a new session");
        assert_eq!(page(&server, "/", &cookie).await.status, 200);

        let already = page(&server, "/login", &cookie).await;
        assert_eq!(location(&already), "/");

        let out = post(&server, "/logout", &cookie, &[]).await;
        assert_eq!(location(&out), "/login");
        assert!(out.header("set-cookie").unwrap().contains("Max-Age=0"));
        assert_eq!(location(&page(&server, "/", &cookie).await), "/login");
        assert_eq!(
            page(&server, "/", &first).await.status,
            200,
            "the other session stays"
        );
    }

    #[tokio::test]
    async fn every_failure_looks_the_same() {
        let server = TestServer::start().await;
        sign_up(&server, "ann", false).await;
        let db = &server.state.db;
        accounts::create_user(db, "no-password-yet", 1, false)
            .await
            .unwrap();

        for (username, password) in [
            ("ann", "wrong password"),
            ("nobody", PASSWORD),
            ("no-password-yet", PASSWORD),
            ("ann", &"x".repeat(5000)),
        ] {
            let reply = post(
                &server,
                "/login",
                "",
                &[("username", username), ("password", password)],
            )
            .await;
            assert_eq!(location(&reply), "/login?error=wrong", "{username}");
            assert!(reply.header("set-cookie").is_none());
        }
        let shown = html(&server.get("/login?error=wrong").await);
        assert!(
            shown.contains("don&#x27;t match") || shown.contains("don't match"),
            "{shown}"
        );
    }

    async fn wrong_login(server: &TestServer, username: &str, ip: &str) -> Reply {
        server
            .request(
                Method::POST,
                "/login",
                &[
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("sec-fetch-site", "same-origin"),
                    ("x-real-ip", ip),
                ],
                encode(&[("username", username), ("password", "not the password")]),
            )
            .await
    }

    #[tokio::test]
    async fn failed_logins_from_one_address_dont_lock_out_another() {
        let server = TestServer::start_with(&[("CLIENT_IP_HEADER", "X-Real-IP")]).await;
        sign_up(&server, "ann", false).await;

        for _ in 0..10 {
            let reply = wrong_login(&server, "ann", "1.1.1.1").await;
            assert_eq!(location(&reply), "/login?error=wrong");
        }
        let reply = wrong_login(&server, "ann", "1.1.1.1").await;
        assert_eq!(location(&reply), "/login?error=slow");

        // Someone else, with the right password, gets straight in.
        let reply = server
            .request(
                Method::POST,
                "/login",
                &[
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("sec-fetch-site", "same-origin"),
                    ("x-real-ip", "2.2.2.2"),
                ],
                encode(&[("username", "ann"), ("password", PASSWORD)]),
            )
            .await;
        assert_eq!(location(&reply), "/");
    }

    #[tokio::test]
    async fn a_spoofed_forwarding_header_doesnt_escape_the_limit() {
        let server = TestServer::start_with(&[("CLIENT_IP_HEADER", "X-Real-IP")]).await;
        sign_up(&server, "ann", false).await;

        for attempt in 0..11 {
            let forged = format!("9.9.9.{attempt}");
            let reply = server
                .request(
                    Method::POST,
                    "/login",
                    &[
                        ("content-type", "application/x-www-form-urlencoded"),
                        ("sec-fetch-site", "same-origin"),
                        ("x-real-ip", "1.1.1.1"),
                        ("x-forwarded-for", &forged),
                    ],
                    encode(&[("username", "ann"), ("password", "not the password")]),
                )
                .await;
            let expected = if attempt < 10 { "wrong" } else { "slow" };
            assert_eq!(location(&reply), format!("/login?error={expected}"));
        }
    }

    /// Spending the limit directly, since a hundred real guesses means a hundred Argon2 runs.
    #[tokio::test]
    async fn one_name_cant_be_guessed_from_many_addresses() {
        let server = TestServer::start().await;
        sign_up(&server, "ann", false).await;
        while server.state.limits.login_user.acquire("ann") {}
        for name in ["ann", "ANN"] {
            let reply = post(
                &server,
                "/login",
                "",
                &[("username", name), ("password", PASSWORD)],
            )
            .await;
            assert_eq!(location(&reply), "/login?error=slow", "{name}");
        }
    }

    #[tokio::test]
    async fn the_right_password_doesnt_count_against_the_name() {
        let server = TestServer::start().await;
        sign_up(&server, "ann", false).await;
        let limit = &server.state.limits.login_user;
        while limit.acquire("ann") {}
        limit.refund("ann");
        for _ in 0..3 {
            let reply = post(
                &server,
                "/login",
                "",
                &[("username", "ann"), ("password", PASSWORD)],
            )
            .await;
            assert_eq!(location(&reply), "/");
        }
    }

    #[tokio::test]
    async fn a_huge_password_is_just_a_wrong_one() {
        let server = TestServer::start().await;
        sign_up(&server, "ann", false).await;
        let huge = "x".repeat(100_000);
        let reply = post(
            &server,
            "/login",
            "",
            &[("username", "ann"), ("password", &huge)],
        )
        .await;
        assert_eq!(location(&reply), "/login?error=wrong");
    }

    #[tokio::test]
    async fn the_right_password_doesnt_use_up_attempts() {
        let server = TestServer::start_with(&[("CLIENT_IP_HEADER", "X-Real-IP")]).await;
        sign_up(&server, "ann", false).await;
        for _ in 0..15 {
            let reply = post(
                &server,
                "/login",
                "",
                &[("username", "ann"), ("password", PASSWORD)],
            )
            .await;
            assert_eq!(location(&reply), "/");
        }
    }

    #[tokio::test]
    async fn a_setup_link_works_once() {
        let server = TestServer::start().await;
        let db = &server.state.db;
        let id = accounts::create_user(db, "ann", 1000, false).await.unwrap();
        let link = accounts::setup_link(db, common::PUBLIC_URL, id)
            .await
            .unwrap();
        let path = link.strip_prefix(common::PUBLIC_URL).unwrap();

        let shown = html(&server.get(path).await);
        assert!(shown.contains("Hello ") && shown.contains("ann"), "{shown}");

        let reply = post(
            &server,
            path,
            "",
            &[("password", "too short"), ("confirm", "too short")],
        )
        .await;
        assert_eq!(location(&reply), format!("{path}?error=short"));

        let reply = post(
            &server,
            path,
            "",
            &[
                ("password", PASSWORD),
                ("confirm", "something else entirely"),
            ],
        )
        .await;
        assert_eq!(location(&reply), format!("{path}?error=mismatch"));

        let reply = post(
            &server,
            path,
            "",
            &[("password", PASSWORD), ("confirm", PASSWORD)],
        )
        .await;
        assert_eq!(location(&reply), "/");

        let again = post(
            &server,
            path,
            "",
            &[("password", PASSWORD), ("confirm", PASSWORD)],
        )
        .await;
        assert_eq!(location(&again), format!("{path}?error=expired"));
        assert!(html(&server.get(path).await).contains("expired"));
    }

    #[tokio::test]
    async fn there_is_no_signup() {
        let server = TestServer::start().await;
        for path in ["/signup", "/register", "/setup/made-up-token"] {
            let reply = post(
                &server,
                path,
                "",
                &[
                    ("username", "x"),
                    ("password", PASSWORD),
                    ("confirm", PASSWORD),
                ],
            )
            .await;
            assert!(reply.header("set-cookie").is_none(), "{path}");
        }
        assert!(
            !server
                .state
                .db
                .list_users()
                .await
                .unwrap()
                .iter()
                .any(|u| u.username == "x")
        );
    }
}

mod pages {
    use super::*;

    /// A plain `cargo build` and `cargo leptos build` must agree on this name: Leptos adds
    /// `_bg` unless told otherwise when it's compiled, and cargo-leptos doesn't write that
    /// file, so the page would load without its WASM and only the plain form would work.
    #[tokio::test]
    async fn pages_ask_for_the_wasm_file_the_build_writes() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        let body = html(&page(&server, "/", &cookie).await);
        assert!(body.contains("/pkg/simple-books.wasm"), "{body}");
        assert!(!body.contains("_bg.wasm"), "{body}");
    }

    #[tokio::test]
    async fn the_button_says_whose_book_goes_private() {
        let server = TestServer::start().await;
        let (ann, ann_cookie) = sign_up(&server, "ann", false).await;
        let (_, admin_cookie) = sign_up(&server, "root", true).await;
        let book = server.add_shared_book(&ann, "Ann Shared").await;
        let path = format!("/books/{book}");

        let own = html(&page(&server, &path, &ann_cookie).await);
        assert!(own.contains("Make it just for me"), "{own}");
        assert!(!own.contains("just for the uploader"), "{own}");

        let admin = html(&page(&server, &path, &admin_cookie).await);
        assert!(admin.contains("Make it just for the uploader"), "{admin}");
        assert!(!admin.contains("just for me"), "{admin}");
    }

    #[tokio::test]
    async fn each_book_is_one_stop_for_the_keyboard() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        server.add_book(&ann, "Ann Private").await;
        let body = html(&page(&server, "/", &cookie).await);
        assert!(
            body.contains(r#"tabindex="-1" aria-hidden="true""#),
            "{body}"
        );
        assert!(body.contains(r#"class="title""#), "{body}");
    }

    #[tokio::test]
    async fn the_library_shows_your_books_and_nobody_elses() {
        let server = TestServer::start().await;
        let (ann, ann_cookie) = sign_up(&server, "ann", false).await;
        let (bob, bob_cookie) = sign_up(&server, "bob", false).await;
        server.add_book(&ann, "Ann Private").await;
        server.add_book(&bob, "Bob Private").await;
        server.add_shared_book(&bob, "Bob Shared").await;

        let mine = html(&page(&server, "/", &ann_cookie).await);
        assert!(mine.contains("Ann Private"));
        assert!(!mine.contains("Bob Private"));
        assert!(!mine.contains("Bob Shared"), "not until it's added");

        let shared = html(&page(&server, "/shared", &ann_cookie).await);
        assert!(shared.contains("Bob Shared"));
        assert!(!shared.contains("Bob Private"));
        assert!(!shared.contains("Ann Private"));
        assert!(shared.contains("Add to my Kobo"));
        assert!(shared.contains("Shared by "));

        let own = html(&page(&server, "/shared", &bob_cookie).await);
        assert!(own.contains("You shared this"));
        assert!(!own.contains("Shared by "), "said once is enough");
        assert!(!own.contains("Add to my Kobo"), "already theirs");
    }

    #[tokio::test]
    async fn untrusted_metadata_is_only_ever_text() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let hostile = "</textarea></title><script>alert(1)</script><img src=x onerror=alert(2)>";
        let book = server.add_book(&ann, hostile).await;
        let edit = simple_books::server::db::models::MetadataEdit {
            title: hostile.to_owned(),
            authors: vec![hostile.to_owned()],
            details: simple_books::server::db::models::BookDetails {
                series: Some(hostile.to_owned()),
                description: Some(hostile.to_owned()),
                publisher: Some(r#"" onfocus="alert(3)"#.to_owned()),
                ..Default::default()
            },
        };
        ann.set_metadata(book, &edit).await.unwrap();

        for path in ["/".to_owned(), format!("/books/{book}")] {
            let body = html(&page(&server, &path, &cookie).await);
            assert!(!body.contains("<script>alert"), "{path}");
            assert!(!body.contains("<img src=x"), "{path}");
            assert!(!body.contains("</textarea></title>"), "{path}");
            assert!(!body.contains(r#"" onfocus="#), "{path}");
            assert!(body.contains("&lt;script&gt;alert(1)"), "{path}");
        }
    }

    #[tokio::test]
    async fn messages_come_only_from_the_fixed_list() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        let body = html(&page(&server, "/?done=%3Cb%3Epwned%3C/b%3E&error=pwned", &cookie).await);
        assert!(!body.contains("pwned"));
        let body = html(&page(&server, "/?done=uploaded", &cookie).await);
        assert!(body.contains("tap sync"));
    }

    #[tokio::test]
    async fn the_filter_finds_by_title_and_author() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        server.add_book(&ann, "Moby Dick").await;
        server.add_book(&ann, "Emma").await;

        let body = html(&page(&server, "/?q=moby", &cookie).await);
        assert!(body.contains("Moby Dick"));
        assert!(!body.contains(">Emma<"));
        let body = html(&page(&server, "/?q=ANN", &cookie).await);
        assert!(
            body.contains("Moby Dick") && body.contains(">Emma<"),
            "the test books' author"
        );
        let body = html(&page(&server, "/?q=nothing-like-this", &cookie).await);
        assert!(!body.contains("Moby Dick"));
    }

    #[tokio::test]
    async fn first_visit_offers_to_connect_a_kobo_and_then_stops() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        let before = html(&page(&server, "/", &cookie).await);
        assert!(before.contains(r#"action="/device/connect""#));

        let reply = post(&server, "/device/connect", &cookie, &[]).await;
        assert_eq!(reply.status, 200);
        let after = html(&page(&server, "/", &cookie).await);
        assert!(!after.contains(r#"action="/device/connect""#));
    }

    #[tokio::test]
    async fn pages_have_a_csp_matching_their_scripts_and_no_inline_styles() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", true).await;
        let book = server.add_book(&ann, "A Book").await;

        for path in [
            "/".to_owned(),
            "/shared".to_owned(),
            "/device".to_owned(),
            "/admin/users".to_owned(),
            format!("/books/{book}"),
        ] {
            let reply = page(&server, &path, &cookie).await;
            assert_eq!(reply.status, 200, "{path}");
            let csp = reply.header("content-security-policy").unwrap();
            let nonce = csp
                .split("'nonce-")
                .nth(1)
                .unwrap()
                .split('\'')
                .next()
                .unwrap();
            let body = html(&reply);
            for chunk in body.split("<script").skip(1) {
                let tag = chunk.split('>').next().unwrap();
                assert!(tag.contains(&format!("nonce=\"{nonce}\"")), "{path}: {tag}");
            }
            assert!(!body.contains(" style="), "{path}");
            assert!(!body.contains(" onclick="), "{path}");
            assert!(
                reply.header("cache-control").unwrap().contains("no-store"),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn the_upload_form_works_with_no_javascript() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;

        let body = html(&page(&server, "/", &cookie).await);
        assert!(body.contains(r#"action="/books/upload""#));
        assert!(body.contains(r#"enctype="multipart/form-data""#));
        assert!(body.contains(r#"value="private""#) && body.contains(" checked"));

        // A browser's form accepts HTML, not JSON, and gets sent back to the page.
        let bytes = b"this isn't an epub".to_vec();
        let reply = plain_upload(&server, &cookie, &bytes).await;
        assert_eq!(location(&reply), "/?error=not_a_book");

        assert_eq!(ann.used_bytes().await.unwrap(), 0);
    }

    async fn plain_upload(server: &TestServer, cookie: &str, file: &[u8]) -> Reply {
        let mut body = Vec::new();
        body.extend_from_slice(
            b"--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.epub\"\r\n\r\n",
        );
        body.extend_from_slice(file);
        body.extend_from_slice(b"\r\n--b--\r\n");
        server
            .request(
                Method::POST,
                "/books/upload",
                &[
                    ("cookie", cookie),
                    ("content-type", "multipart/form-data; boundary=b"),
                    ("accept", "text/html"),
                    ("sec-fetch-site", "same-origin"),
                ],
                body,
            )
            .await
    }

    #[tokio::test]
    async fn an_unknown_address_for_a_signed_in_user_is_a_404() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        for path in [
            "/no/such/page".to_owned(),
            "/books/not-a-uuid".to_owned(),
            format!("/books/{}", Uuid::new_v4()),
        ] {
            assert_eq!(page(&server, &path, &cookie).await.status, 404, "{path}");
        }
    }

    #[tokio::test]
    async fn no_spelling_of_the_users_page_shows_it_to_a_non_admin() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;
        let (_, ann) = sign_up(&server, "ann", false).await;
        assert_eq!(page(&server, "/admin/users", &ann).await.status, 403);
        for path in [
            "/./admin/users",
            "/%2e/admin/users",
            "/x/../admin/users",
            "/setup/../admin/users",
            "/login/../admin/users",
            "//admin/users",
            "/admin/./users",
            "/admin%2fusers",
        ] {
            for (who, cookie) in [("ann", &ann), ("root", &admin), ("nobody", &String::new())] {
                let reply = page(&server, path, cookie).await;
                assert_eq!(reply.status, 404, "{who} {path}");
                assert!(!html(&reply).contains("root"), "{who} {path}");
            }
        }
        // The plain spelling still works for an admin.
        assert!(html(&page(&server, "/admin/users", &admin).await).contains("ann"));
    }

    #[tokio::test]
    async fn pages_answer_nothing_but_get() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        for method in [
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::OPTIONS,
        ] {
            for path in ["/.gitkeep", "/shared/", "/", "/admin/users/", "/nope"] {
                for who in ["", cookie.as_str()] {
                    let reply = server
                        .request(
                            method.clone(),
                            path,
                            &[("cookie", who), ("sec-fetch-site", "same-origin")],
                            Vec::new(),
                        )
                        .await;
                    assert_eq!(reply.status, 405, "{method} {path}");
                    assert!(reply.body.is_empty(), "{method} {path}");
                }
            }
        }
    }
}

mod permissions {
    use super::*;

    struct World {
        server: TestServer,
        uploader: (UserScope, String),
        admin: String,
        other: (UserScope, String),
        private: Uuid,
        shared: Uuid,
    }

    async fn world() -> World {
        let server = TestServer::start().await;
        let uploader = sign_up(&server, "uploader", false).await;
        let (_, admin) = sign_up(&server, "admin", true).await;
        let other = sign_up(&server, "other", false).await;
        let private = server.add_book(&uploader.0, "Private Book").await;
        let shared = server.add_shared_book(&uploader.0, "Shared Book").await;
        World {
            server,
            uploader,
            admin,
            other,
            private,
            shared,
        }
    }

    const EDIT: [(&str, &str); 8] = [
        ("title", "New Title"),
        ("authors", "Someone"),
        ("series", ""),
        ("series_index", ""),
        ("publisher", ""),
        ("language", ""),
        ("published_on", ""),
        ("description", ""),
    ];

    /// The table in the plan, for each thing that changes a book.
    #[tokio::test]
    async fn who_may_change_which_book() {
        let w = world().await;
        let cases = [
            ("uploader", &w.uploader.1, w.private, 303),
            ("uploader", &w.uploader.1, w.shared, 303),
            ("admin", &w.admin, w.private, 404),
            ("admin", &w.admin, w.shared, 303),
            ("other", &w.other.1, w.private, 404),
            ("other", &w.other.1, w.shared, 403),
        ];
        for (who, cookie, book, expected) in cases {
            let edit = post(&w.server, &format!("/books/{book}/edit"), cookie, &EDIT).await;
            assert_eq!(edit.status, expected, "{who} editing {book}");

            let target = if book == w.shared {
                "shared"
            } else {
                "private"
            };
            let visibility = post(
                &w.server,
                &format!("/books/{book}/visibility"),
                cookie,
                &[("visibility", target)],
            )
            .await;
            assert_eq!(
                visibility.status, expected,
                "{who} changing visibility of {book}"
            );
        }
    }

    #[tokio::test]
    async fn who_may_see_which_book_and_its_controls() {
        let w = world().await;
        let cases = [
            ("uploader", &w.uploader.1, w.private, 200, true),
            ("uploader", &w.uploader.1, w.shared, 200, true),
            ("admin", &w.admin, w.private, 404, false),
            ("admin", &w.admin, w.shared, 200, true),
            ("other", &w.other.1, w.private, 404, false),
            ("other", &w.other.1, w.shared, 200, false),
        ];
        for (who, cookie, book, status, controls) in cases {
            let reply = page(&w.server, &format!("/books/{book}"), cookie).await;
            assert_eq!(reply.status, status, "{who} viewing {book}");
            let body = html(&reply);
            assert_eq!(
                body.contains("/edit\""),
                controls,
                "{who} offered controls for {book}"
            );
            if status == 404 {
                assert!(!body.contains("Private Book"), "{who}");
            }
        }
    }

    #[tokio::test]
    async fn deleting_follows_the_same_table() {
        let w = world().await;
        let path = |book: Uuid| format!("/books/{book}/delete");
        assert_eq!(
            post(&w.server, &path(w.private), &w.admin, &[])
                .await
                .status,
            404
        );
        assert_eq!(
            post(&w.server, &path(w.private), &w.other.1, &[])
                .await
                .status,
            404
        );
        assert_eq!(
            post(&w.server, &path(w.shared), &w.other.1, &[])
                .await
                .status,
            403
        );
        assert_eq!(
            location(&post(&w.server, &path(w.private), &w.uploader.1, &[]).await),
            "/?done=deleted"
        );
        assert_eq!(
            location(&post(&w.server, &path(w.shared), &w.admin, &[]).await),
            "/?done=deleted"
        );
    }

    #[tokio::test]
    async fn nobody_else_can_add_a_private_book() {
        let w = world().await;
        let path = format!("/books/{}/add", w.private);
        assert_eq!(post(&w.server, &path, &w.other.1, &[]).await.status, 404);
        assert_eq!(post(&w.server, &path, &w.admin, &[]).await.status, 404);
    }

    /// Every route that changes something, for someone with no session, and from
    /// another site with a valid one.
    #[tokio::test]
    async fn every_change_needs_a_session_and_this_site() {
        let w = world().await;
        let book = w.shared;
        let device = 1;
        let routes = [
            format!("/books/{book}/edit"),
            format!("/books/{book}/visibility"),
            format!("/books/{book}/add"),
            format!("/books/{book}/remove"),
            format!("/books/{book}/delete"),
            format!("/books/{book}/cover"),
            "/books/upload".to_owned(),
            "/device/connect".to_owned(),
            format!("/device/{device}/reconnect"),
            format!("/device/{device}/resync"),
            format!("/device/{device}/remove"),
            "/admin/users".to_owned(),
            "/admin/users/1/setup-link".to_owned(),
            "/admin/users/1/disable".to_owned(),
            "/admin/users/1/enable".to_owned(),
            "/admin/users/1/quota".to_owned(),
        ];
        for route in &routes {
            let stranger = post(&w.server, route, "", &[]).await;
            assert_eq!(stranger.status, 401, "{route} with no session");

            let cross = w
                .server
                .request(
                    Method::POST,
                    route,
                    &[("cookie", &w.admin), ("sec-fetch-site", "cross-site")],
                    Vec::new(),
                )
                .await;
            assert_eq!(cross.status, 403, "{route} from another site");
        }
        for route in ["/login", "/logout", "/setup/abc"] {
            let cross = w
                .server
                .request(
                    Method::POST,
                    route,
                    &[("sec-fetch-site", "cross-site")],
                    Vec::new(),
                )
                .await;
            assert_eq!(cross.status, 403, "{route} from another site");
        }
    }

    #[tokio::test]
    async fn only_admins_reach_the_admin_routes() {
        let w = world().await;
        let cookie = &w.other.1;
        assert_eq!(page(&w.server, "/admin/users", cookie).await.status, 403);
        let target = w.uploader.0.user_id();
        for route in [
            "/admin/users".to_owned(),
            format!("/admin/users/{target}/setup-link"),
            format!("/admin/users/{target}/disable"),
            format!("/admin/users/{target}/enable"),
            format!("/admin/users/{target}/quota"),
        ] {
            let reply = post(
                &w.server,
                &route,
                cookie,
                &[("username", "made-by-other"), ("quota_mb", "1")],
            )
            .await;
            assert_eq!(reply.status, 403, "{route}");
        }
        let users = w.server.state.db.list_users().await.unwrap();
        assert!(!users.iter().any(|u| u.username == "made-by-other"));
    }

    #[tokio::test]
    async fn someone_elses_device_is_out_of_reach() {
        let w = world().await;
        let (_, token) =
            testkit::create_kobo_device(&w.server.state.db, w.uploader.0.user_id(), "k")
                .await
                .unwrap();
        let devices = w
            .server
            .state
            .db
            .list_devices(w.uploader.0.user_id())
            .await
            .unwrap();
        let id = devices[0].id;
        for action in ["reconnect", "resync", "remove"] {
            let reply = post(
                &w.server,
                &format!("/device/{id}/{action}"),
                &w.other.1,
                &[],
            )
            .await;
            assert_eq!(reply.status, 404, "{action}");
        }
        assert_eq!(
            w.server.sync(&token, None).await.status,
            200,
            "still working"
        );
    }
}

mod books {
    use super::*;

    const fn edit_form<'a>(title: &'a str, index: &'a str) -> [(&'a str, &'a str); 8] {
        [
            ("title", title),
            ("authors", "First Author\nSecond, Author\n\n"),
            ("series", "The Series"),
            ("series_index", index),
            ("publisher", "Pub"),
            ("language", "en"),
            ("published_on", "2001"),
            ("description", "Line one\nLine two"),
        ]
    }

    #[tokio::test]
    async fn editing_reaches_the_kobo_without_touching_the_file() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let (_, token) = testkit::create_kobo_device(&server.state.db, ann.user_id(), "k")
            .await
            .unwrap();
        let book = server.add_book(&ann, "Old Title").await;
        let synced = server.sync(&token, None).await;
        let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();
        let file = std::fs::read(server.state.layout.original(book)).unwrap();

        let reply = post(
            &server,
            &format!("/books/{book}/edit"),
            &cookie,
            &edit_form("New Title", "2.5"),
        )
        .await;
        assert_eq!(location(&reply), format!("/books/{book}?done=saved"));

        let row = ann.library_book(book).await.unwrap().unwrap();
        assert_eq!(row.title, "New Title");
        assert_eq!(row.authors(), ["First Author", "Second, Author"]);
        assert_eq!(row.series.as_deref(), Some("The Series"));
        assert_eq!(row.series_index, Some(2.5));
        assert_eq!(row.description.as_deref(), Some("Line one\nLine two"));

        let entries = server.sync(&token, Some(&cursor)).await.json();
        assert_eq!(
            entries[0]["ChangedProductMetadata"]["BookMetadata"]["Title"],
            "New Title"
        );
        assert_eq!(
            std::fs::read(server.state.layout.original(book)).unwrap(),
            file
        );
    }

    #[tokio::test]
    async fn an_edit_is_tidied_before_it_reaches_a_kobo() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let book = server.add_book(&ann, "Old Title").await;
        let reply = post(
            &server,
            &format!("/books/{book}/edit"),
            &cookie,
            &[
                ("title", "  Be\u{7}ll\u{1b}[31m  "),
                ("authors", "An\u{0}n\n\n   \n Bob "),
                ("series", "Se\tries"),
                ("series_index", ""),
                ("publisher", "   "),
                ("language", "en"),
                ("published_on", ""),
                ("description", "  Line one\nLine two  "),
            ],
        )
        .await;
        assert_eq!(location(&reply), format!("/books/{book}?done=saved"));
        let saved = ann.library_book(book).await.unwrap().unwrap();
        assert_eq!(saved.title, "Bell[31m");
        assert_eq!(saved.authors(), ["Ann", "Bob"]);
        assert_eq!(saved.series.as_deref(), Some("Series"));
        assert_eq!(saved.series_index, None);
        assert_eq!(saved.publisher, None);
        assert_eq!(saved.description.as_deref(), Some("Line one\nLine two"));
    }

    #[tokio::test]
    async fn an_edit_can_only_be_so_long() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let book = server.add_book(&ann, "Old Title").await;
        let path = format!("/books/{book}/edit");

        let authors = "Someone\n".repeat(80);
        let long_title = "t".repeat(600);
        let long_line = "s".repeat(400);
        let long_text = "d".repeat(30_000);
        let reply = post(
            &server,
            &path,
            &cookie,
            &[
                ("title", &long_title),
                ("authors", &authors),
                ("series", &long_line),
                ("series_index", ""),
                ("publisher", ""),
                ("language", ""),
                ("published_on", ""),
                ("description", &long_text),
            ],
        )
        .await;
        assert_eq!(location(&reply), format!("/books/{book}?done=saved"));
        let saved = ann.library_book(book).await.unwrap().unwrap();
        assert_eq!(saved.title.chars().count(), 500);
        assert_eq!(saved.authors().len(), 50);
        assert_eq!(saved.series.unwrap().chars().count(), 300);
        assert_eq!(saved.description.unwrap().chars().count(), 20_000);

        // A form bigger than any honest one is refused before it's even read.
        let huge = "d".repeat(300 * 1024);
        let reply = post(
            &server,
            &path,
            &cookie,
            &[
                ("title", "Too Big"),
                ("authors", ""),
                ("series", ""),
                ("series_index", ""),
                ("publisher", ""),
                ("language", ""),
                ("published_on", ""),
                ("description", &huge),
            ],
        )
        .await;
        assert_eq!(reply.status, 413);
        let title = ann.library_book(book).await.unwrap().unwrap().title;
        assert_ne!(title, "Too Big");
    }

    #[tokio::test]
    async fn an_unknown_visibility_changes_nothing() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let book = server.add_shared_book(&ann, "Ann Shared").await;
        let reply = post(
            &server,
            &format!("/books/{book}/visibility"),
            &cookie,
            &[("visibility", "everyone-and-their-dog")],
        )
        .await;
        assert_eq!(location(&reply), format!("/books/{book}?error=details"));
        let saved = ann.library_book(book).await.unwrap().unwrap();
        assert_eq!(saved.visibility, "shared");
    }

    #[tokio::test]
    async fn a_bad_edit_changes_nothing() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let book = server.add_book(&ann, "Old Title").await;
        for form in [
            edit_form("", "1"),
            edit_form("   \t  ", "1"),
            edit_form("Fine", "not a number"),
            edit_form("Fine", "-1"),
            edit_form("Fine", "inf"),
        ] {
            let reply = post(&server, &format!("/books/{book}/edit"), &cookie, &form).await;
            assert_eq!(location(&reply), format!("/books/{book}?error=details"));
            assert_eq!(
                ann.library_book(book).await.unwrap().unwrap().title,
                "Old Title"
            );
        }
        let reply = post(
            &server,
            &format!("/books/{book}/edit"),
            &cookie,
            &[("title", "missing the other fields")],
        )
        .await;
        assert!(reply.status.is_client_error());
    }

    #[tokio::test]
    async fn add_and_remove_reach_the_kobo() {
        let server = TestServer::start().await;
        let (ann, _) = sign_up(&server, "ann", false).await;
        let (bob, bob_cookie) = sign_up(&server, "bob", false).await;
        let (_, token) = testkit::create_kobo_device(&server.state.db, bob.user_id(), "k")
            .await
            .unwrap();
        let book = server.add_shared_book(&ann, "Shared Book").await;

        assert_eq!(server.sync(&token, None).await.json(), json!([]));

        let reply = post(
            &server,
            &format!("/books/{book}/add"),
            &bob_cookie,
            &[("back", "shared")],
        )
        .await;
        assert_eq!(location(&reply), "/shared?done=added");
        let body = html(&page(&server, "/?done=added", &bob_cookie).await);
        assert!(body.contains("Shared Book") && body.contains("tap sync"));

        let synced = server.sync(&token, None).await;
        let entries = synced.json();
        assert_eq!(
            entries[0]["NewEntitlement"]["BookMetadata"]["Title"],
            "Shared Book"
        );
        let cursor = synced.header("x-kobo-synctoken").unwrap().to_owned();

        // The shared page's button has flipped.
        let body = html(&page(&server, "/shared", &bob_cookie).await);
        assert!(body.contains(&format!("/books/{book}/remove")));

        let reply = post(
            &server,
            &format!("/books/{book}/remove"),
            &bob_cookie,
            &[("back", "somewhere-else")],
        )
        .await;
        assert_eq!(reply.status, 422, "only the fixed places to go back to");

        let reply = post(&server, &format!("/books/{book}/remove"), &bob_cookie, &[]).await;
        assert_eq!(location(&reply), format!("/books/{book}?done=removed"));
        let entries = server.sync(&token, Some(&cursor)).await.json();
        assert_eq!(
            entries[0]["ChangedEntitlement"]["BookEntitlement"]["IsRemoved"],
            true
        );
    }

    #[tokio::test]
    async fn the_uploader_cant_drop_their_own_book_from_their_library() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let book = server.add_shared_book(&ann, "Mine").await;
        let body = html(&page(&server, &format!("/books/{book}"), &cookie).await);
        assert!(!body.contains(&format!("/books/{book}/remove")));
        let reply = post(&server, &format!("/books/{book}/remove"), &cookie, &[]).await;
        assert_eq!(reply.status, 403);
    }

    #[tokio::test]
    async fn unsharing_warns_how_many_and_takes_it_back() {
        let server = TestServer::start().await;
        let (ann, ann_cookie) = sign_up(&server, "ann", false).await;
        let (bob, bob_cookie) = sign_up(&server, "bob", false).await;
        let (carol, _) = sign_up(&server, "carol", false).await;
        let book = server.add_shared_book(&ann, "Shared Book").await;

        let body = html(&page(&server, &format!("/books/{book}"), &ann_cookie).await);
        assert!(!body.contains("next sync"), "nobody to warn yet");

        bob.add_to_library(book).await.unwrap();
        let body = html(&page(&server, &format!("/books/{book}"), &ann_cookie).await);
        assert!(body.contains("1 other person has"), "{body}");
        carol.add_to_library(book).await.unwrap();
        let body = html(&page(&server, &format!("/books/{book}"), &ann_cookie).await);
        assert!(body.contains("2 other people have"), "{body}");

        // Only those who can change it get to see the count.
        let body = html(&page(&server, &format!("/books/{book}"), &bob_cookie).await);
        assert!(!body.contains("other people have"));

        let reply = post(
            &server,
            &format!("/books/{book}/visibility"),
            &ann_cookie,
            &[("visibility", "private")],
        )
        .await;
        assert_eq!(location(&reply), format!("/books/{book}?done=private"));
        assert_eq!(
            page(&server, &format!("/books/{book}"), &bob_cookie)
                .await
                .status,
            404
        );
        assert!(bob.library_book(book).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_file_someone_already_shares_cant_be_shared_twice() {
        let server = TestServer::start().await;
        let (ann, _) = sign_up(&server, "ann", false).await;
        let (bob, bob_cookie) = sign_up(&server, "bob", false).await;

        let new_book = |visibility| simple_books::server::db::models::NewBook {
            id: Uuid::new_v4(),
            title: "Same File".to_owned(),
            authors: vec![],
            cover_id: None,
            original_sha256: "same-hash".to_owned(),
            original_size: 10,
            visibility,
            kepub_size: Some(10),
            details: simple_books::server::db::models::BookDetails::default(),
        };
        ann.create_book(&new_book(Visibility::Shared), true)
            .await
            .unwrap();
        let mine = new_book(Visibility::Private);
        bob.create_book(&mine, true).await.unwrap();

        let reply = post(
            &server,
            &format!("/books/{}/visibility", mine.id),
            &bob_cookie,
            &[("visibility", "shared")],
        )
        .await;
        assert_eq!(
            location(&reply),
            format!("/books/{}?error=already_shared", mine.id)
        );
    }

    #[tokio::test]
    async fn the_plain_cover_form_comes_back_to_the_book() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        let book = server.add_book(&ann, "A Book").await;
        let mut body = Vec::new();
        body.extend_from_slice(
            b"--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.png\"\r\n\r\nnot an image\r\n--b--\r\n",
        );
        let reply = server
            .request(
                Method::POST,
                &format!("/books/{book}/cover"),
                &[
                    ("cookie", &cookie),
                    ("content-type", "multipart/form-data; boundary=b"),
                    ("sec-fetch-site", "same-origin"),
                ],
                body,
            )
            .await;
        assert_eq!(location(&reply), format!("/books/{book}?error=not_image"));
    }
}

mod devices {
    use super::*;

    fn endpoint_in(body: &str) -> String {
        let marker = "api_endpoint=";
        id_in(body, marker).expect("the page shows the address")
    }

    fn token_of(endpoint: &str) -> String {
        endpoint
            .strip_prefix(&format!("{}/kobo/", common::PUBLIC_URL))
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn connecting_gives_an_address_that_works_and_only_once() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        server.add_book(&ann, "A Book").await;

        let reply = post(&server, "/device/connect", &cookie, &[]).await;
        assert_eq!(reply.status, 200, "shown, not redirected: it's a secret");
        assert!(reply.header("location").is_none());
        assert!(reply.header("cache-control").unwrap().contains("no-store"));
        let body = html(&reply);
        let endpoint = endpoint_in(&body);
        let token = token_of(&endpoint);
        assert_eq!(token.len(), 43);
        assert!(body.contains("<leptos-island"), "the patcher is there");

        let synced = server.sync(&token, None).await.json();
        assert_eq!(synced.as_array().unwrap().len(), 1);

        // Never again: the page can't show what isn't stored.
        let later = html(&page(&server, "/device", &cookie).await);
        assert!(!later.contains(&token));
        assert!(!later.contains("api_endpoint="));
        assert!(later.contains("Last synced"), "it shows up as connected");
    }

    #[tokio::test]
    async fn waiting_for_the_first_sync_is_stated() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        post(&server, "/device/connect", &cookie, &[]).await;
        let body = html(&page(&server, "/device", &cookie).await);
        assert!(body.contains("Waiting for its first sync"));
    }

    #[tokio::test]
    async fn connecting_again_keeps_the_old_address_until_the_new_one_is_used() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        let body = html(&post(&server, "/device/connect", &cookie, &[]).await);
        let old = token_of(&endpoint_in(&body));
        assert_eq!(server.sync(&old, None).await.status, 200);

        let page_body = html(&page(&server, "/device", &cookie).await);
        let id = id_in(&page_body, "/device/").unwrap();
        let id = id.split('/').next().unwrap().to_owned();
        let body = html(&post(&server, &format!("/device/{id}/reconnect"), &cookie, &[]).await);
        let new = token_of(&endpoint_in(&body));
        assert_ne!(new, old);

        assert_eq!(server.sync(&old, None).await.status, 200, "still fine");
        assert_eq!(server.sync(&new, None).await.status, 200);
        assert_eq!(server.sync(&old, None).await.status, 401, "now replaced");
        assert_eq!(server.sync(&new, None).await.status, 200);
    }

    #[tokio::test]
    async fn removing_a_device_stops_it() {
        let server = TestServer::start().await;
        let (_, cookie) = sign_up(&server, "ann", false).await;
        let body = html(&post(&server, "/device/connect", &cookie, &[]).await);
        let token = token_of(&endpoint_in(&body));
        let id = server.state.db.list_devices(1).await.unwrap()[0].id;

        let reply = post(&server, &format!("/device/{id}/remove"), &cookie, &[]).await;
        assert_eq!(location(&reply), "/device?done=device_removed");
        assert_eq!(server.sync(&token, None).await.status, 401);
    }

    #[tokio::test]
    async fn sending_everything_again_restarts_the_sync() {
        let server = TestServer::start().await;
        let (ann, cookie) = sign_up(&server, "ann", false).await;
        server.add_book(&ann, "A Book").await;
        let body = html(&post(&server, "/device/connect", &cookie, &[]).await);
        let token = token_of(&endpoint_in(&body));
        let first = server.sync(&token, None).await;
        let cursor = first.header("x-kobo-synctoken").unwrap().to_owned();
        assert_eq!(server.sync(&token, Some(&cursor)).await.json(), json!([]));

        let id = server.state.db.list_devices(ann.user_id()).await.unwrap()[0].id;
        let reply = post(&server, &format!("/device/{id}/resync"), &cookie, &[]).await;
        assert_eq!(location(&reply), "/device?done=resync");
        let again = server.sync(&token, Some(&cursor)).await.json();
        assert!(again[0].get("NewEntitlement").is_some(), "{again}");
    }

    #[tokio::test]
    async fn guessing_tokens_gets_cut_off_without_affecting_others() {
        let server = TestServer::start_with(&[("CLIENT_IP_HEADER", "X-Real-IP")]).await;
        let (ann, _) = sign_up(&server, "ann", false).await;
        let (_, token) = testkit::create_kobo_device(&server.state.db, ann.user_id(), "k")
            .await
            .unwrap();
        let sync = |token: String, ip: &'static str| {
            let path = format!("/kobo/{token}/v1/library/sync");
            let server = &server;
            async move {
                server
                    .request(Method::GET, &path, &[("x-real-ip", ip)], Vec::new())
                    .await
                    .status
            }
        };
        for attempt in 0..60 {
            assert_eq!(sync(format!("guess{attempt}"), "1.1.1.1").await, 401);
        }
        assert_eq!(
            sync(token.clone(), "1.1.1.1").await,
            401,
            "that address is cut off"
        );
        assert_eq!(sync(token, "2.2.2.2").await, 200, "everyone else is fine");
    }
}

mod admin {
    use super::*;

    fn link_in(body: &str) -> String {
        let at = body.find("/setup/").unwrap();
        let start = body[..at].rfind('"').unwrap() + 1;
        let end = at + body[at..].find('"').unwrap();
        body[start..end].to_owned()
    }

    #[tokio::test]
    async fn creating_a_user_shows_a_link_they_can_use() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;

        let reply = post(&server, "/admin/users", &admin, &[("username", "newbie")]).await;
        assert_eq!(reply.status, 200, "shown, not redirected: it's a secret");
        let body = html(&reply);
        let link = link_in(&body);
        assert!(link.starts_with(common::PUBLIC_URL), "{link}");
        assert!(reply.header("cache-control").unwrap().contains("no-store"));

        let later = html(&page(&server, "/admin/users", &admin).await);
        assert!(later.contains("newbie"));
        assert!(!later.contains("/setup/"), "shown only once");
        assert!(later.contains("set a password"), "{later}");

        let path = link.strip_prefix(common::PUBLIC_URL).unwrap();
        let reply = post(
            &server,
            path,
            "",
            &[("password", PASSWORD), ("confirm", PASSWORD)],
        )
        .await;
        assert_eq!(location(&reply), "/");
        let users = server.state.db.list_users().await.unwrap();
        let made = users.iter().find(|u| u.username == "newbie").unwrap();
        assert!(!made.is_admin, "not an admin unless ticked");
        assert_eq!(
            u64::try_from(made.quota_bytes).unwrap(),
            server.state.config.default_quota_bytes
        );
    }

    #[tokio::test]
    async fn the_admin_box_makes_an_admin() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;
        post(
            &server,
            "/admin/users",
            &admin,
            &[("username", "second"), ("admin", "on")],
        )
        .await;
        let users = server.state.db.list_users().await.unwrap();
        assert!(
            users
                .iter()
                .find(|u| u.username == "second")
                .unwrap()
                .is_admin
        );
    }

    #[tokio::test]
    async fn bad_usernames_are_refused() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;
        for (name, error) in [
            ("root", "taken"),
            ("", "username"),
            (" spaced ", "username"),
            ("a/b", "username"),
        ] {
            let reply = post(&server, "/admin/users", &admin, &[("username", name)]).await;
            assert_eq!(
                location(&reply),
                format!("/admin/users?error={error}"),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn a_new_link_resets_a_forgotten_password() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;
        let (user, _) = sign_up(&server, "forgetful", false).await;
        let id = user.user_id();

        let reply = post(
            &server,
            &format!("/admin/users/{id}/setup-link"),
            &admin,
            &[],
        )
        .await;
        assert_eq!(reply.status, 200);
        let link = link_in(&html(&reply));
        let path = link.strip_prefix(common::PUBLIC_URL).unwrap();
        let new_password = "a whole new password";
        let reply = post(
            &server,
            path,
            "",
            &[("password", new_password), ("confirm", new_password)],
        )
        .await;
        assert_eq!(location(&reply), "/");

        let login = post(
            &server,
            "/login",
            "",
            &[("username", "forgetful"), ("password", PASSWORD)],
        )
        .await;
        assert_eq!(
            location(&login),
            "/login?error=wrong",
            "the old password is gone"
        );
    }

    #[tokio::test]
    async fn disabling_ends_everything_and_enabling_brings_it_back() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;
        let (user, cookie) = sign_up(&server, "leaver", false).await;
        let id = user.user_id();
        let (_, token) = testkit::create_kobo_device(&server.state.db, id, "k")
            .await
            .unwrap();

        let reply = post(&server, &format!("/admin/users/{id}/disable"), &admin, &[]).await;
        assert_eq!(location(&reply), "/admin/users?done=disabled");
        assert_eq!(location(&page(&server, "/", &cookie).await), "/login");
        assert_eq!(server.sync(&token, None).await.status, 401);
        let login = post(
            &server,
            "/login",
            "",
            &[("username", "leaver"), ("password", PASSWORD)],
        )
        .await;
        assert_eq!(location(&login), "/login?error=wrong");

        let reply = post(&server, &format!("/admin/users/{id}/enable"), &admin, &[]).await;
        assert_eq!(location(&reply), "/admin/users?done=enabled");
        assert_eq!(server.sync(&token, None).await.status, 200);
    }

    /// Coming back mustn't revive anything that was out there when they were disabled.
    #[tokio::test]
    async fn enabling_someone_doesnt_bring_back_old_sessions_or_links() {
        let server = TestServer::start().await;
        let db = &server.state.db;
        let (_, admin) = sign_up(&server, "root", true).await;
        let (user, cookie) = sign_up(&server, "leaver", false).await;
        let id = user.user_id();
        let link = accounts::setup_link(db, common::PUBLIC_URL, id)
            .await
            .unwrap();
        let old_link = link.strip_prefix(common::PUBLIC_URL).unwrap();

        post(&server, &format!("/admin/users/{id}/disable"), &admin, &[]).await;
        post(&server, &format!("/admin/users/{id}/enable"), &admin, &[]).await;

        assert_eq!(location(&page(&server, "/", &cookie).await), "/login");
        let reply = post(
            &server,
            old_link,
            "",
            &[
                ("password", "a brand new password"),
                ("confirm", "a brand new password"),
            ],
        )
        .await;
        assert_eq!(location(&reply), format!("{old_link}?error=expired"));
        let login = post(
            &server,
            "/login",
            "",
            &[("username", "leaver"), ("password", PASSWORD)],
        )
        .await;
        assert_eq!(location(&login), "/", "their password still works");
    }

    #[tokio::test]
    async fn a_disabled_user_cant_be_given_a_setup_link() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;
        let (user, _) = sign_up(&server, "leaver", false).await;
        let id = user.user_id();
        post(&server, &format!("/admin/users/{id}/disable"), &admin, &[]).await;
        let reply = post(
            &server,
            &format!("/admin/users/{id}/setup-link"),
            &admin,
            &[],
        )
        .await;
        assert_eq!(reply.status, 404);
    }

    #[tokio::test]
    async fn an_admin_cant_disable_themselves() {
        let server = TestServer::start().await;
        let (me, admin) = sign_up(&server, "root", true).await;
        let id = me.user_id();
        let reply = post(&server, &format!("/admin/users/{id}/disable"), &admin, &[]).await;
        assert_eq!(location(&reply), "/admin/users?error=self");
        assert_eq!(page(&server, "/admin/users", &admin).await.status, 200);
    }

    #[tokio::test]
    async fn setting_a_quota() {
        let server = TestServer::start().await;
        let (_, admin) = sign_up(&server, "root", true).await;
        let (user, _) = sign_up(&server, "someone", false).await;
        let id = user.user_id();
        let route = format!("/admin/users/{id}/quota");

        let reply = post(&server, &route, &admin, &[("quota_mb", "2")]).await;
        assert_eq!(location(&reply), "/admin/users?done=quota");
        assert_eq!(user.quota_bytes().await.unwrap(), 2 * 1024 * 1024);

        for bad in ["-1", "lots", "", "99999999999999999999"] {
            let reply = post(&server, &route, &admin, &[("quota_mb", bad)]).await;
            assert_eq!(location(&reply), "/admin/users?error=quota", "{bad}");
        }
        assert_eq!(user.quota_bytes().await.unwrap(), 2 * 1024 * 1024);
    }

    #[tokio::test]
    async fn the_admin_page_shows_the_address_it_sees() {
        let server = TestServer::start_with(&[("CLIENT_IP_HEADER", "X-Real-IP")]).await;
        let (_, admin) = sign_up(&server, "root", true).await;
        let reply = server
            .request(
                Method::GET,
                "/admin/users",
                &[("cookie", &admin), ("x-real-ip", "203.0.113.7")],
                Vec::new(),
            )
            .await;
        assert!(html(&reply).contains("203.0.113.7"));
    }
}

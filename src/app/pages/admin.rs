//! Users. The gate middleware keeps everyone but admins out, and the page checks
//! again, so a path the gate reads differently can't show it.

use leptos::prelude::*;

use super::{Layout, app_state, human_size, request_parts, server_error, signed_in};
use crate::server::{db::accounts::UserSummary, http::client_ip, web::Revealed};

const MIB: i64 = 1024 * 1024;

#[component]
pub fn UsersPage() -> impl IntoView {
    let user = signed_in().filter(|user| user.is_admin);
    let state = app_state();
    let parts = request_parts();
    let revealed = match parts.extensions.get::<Revealed>() {
        Some(Revealed::SetupLink { username, link }) => Some((username.clone(), link.clone())),
        _ => None,
    };
    let seen_ip = client_ip::from_request(&state.config, &parts.headers, &parts.extensions)
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());

    user.map(|user| {
        let me = user.id;
        view! {
            <Layout user title="Users">
                <h1>"Users"</h1>
                {revealed.map(|(username, link)| view! {
                    <section class="callout">
                        <p>
                            "Send this link to " {username}
                            ". It works once, within 72 hours, and sets their password."
                        </p>
                        <input class="reveal" readonly aria-label="Setup link" value=link />
                    </section>
                })}
                <form class="inline" method="post" action="/admin/users">
                    <label>"New user" <input name="username" required autocomplete="off" /></label>
                    <label class="check"><input type="checkbox" name="admin" /> " Admin"</label>
                    <button type="submit">"Create"</button>
                </form>
                <Suspense>
                    {Suspend::new(async move {
                        match state.db.list_users().await {
                            Ok(users) => view! {
                                <ul class="users">
                                    {users.into_iter().map(|user| view! {
                                        <UserRow user=user me=me />
                                    }).collect_view()}
                                </ul>
                            }
                            .into_any(),
                            Err(error) => server_error(&error),
                        }
                    })}
                </Suspense>
                <p class="muted usage">
                    "Your address, as this server sees it: " <code>{seen_ip}</code>
                    ". If this is your proxy's address and not yours, check " <code>"CLIENT_IP_HEADER"</code> "."
                </p>
            </Layout>
        }
    })
}

#[component]
fn UserRow(user: UserSummary, me: i64) -> impl IntoView {
    let id = user.id;
    let status = if user.disabled {
        "Disabled"
    } else if !user.has_password {
        "Hasn't set a password yet"
    } else if user.is_admin {
        "Admin"
    } else {
        "Active"
    };
    let usage = format!(
        "{} of {} used",
        human_size(user.used_bytes),
        human_size(user.quota_bytes)
    );
    let quota_mb = (user.quota_bytes / MIB).to_string();
    let toggle = if user.disabled { "enable" } else { "disable" };
    let toggle_label = if user.disabled { "Enable" } else { "Disable" };
    view! {
        <li class="user">
            <strong>{user.username}</strong>
            <span>{status}</span>
            <span class="muted">{usage}</span>
            <div class="actions">
                <form class="inline" method="post" action=format!("/admin/users/{id}/quota")>
                    <label class="small">"Limit in MB"
                        <input name="quota_mb" inputmode="numeric" value=quota_mb required />
                    </label>
                    <button class="quiet" type="submit">"Set"</button>
                </form>
                {(!user.disabled).then(|| view! {
                    <form method="post" action=format!("/admin/users/{id}/setup-link")>
                        <button class="quiet" type="submit">"New password link"</button>
                    </form>
                })}
                {(id != me).then(|| view! {
                    <form method="post" action=format!("/admin/users/{id}/{toggle}")>
                        <button class="quiet" type="submit">{toggle_label}</button>
                    </form>
                })}
            </div>
        </li>
    }
}

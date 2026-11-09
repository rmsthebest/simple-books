//! Signing in, and setting a password.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::use_params_map;

use super::{Flash, app_state, server_error};
use crate::server::{
    auth::{accounts, password::MIN_PASSWORD_CHARS},
    now,
};

#[component]
pub fn LoginPage() -> impl IntoView {
    view! {
        <Title text="Log in" />
        <main class="narrow">
            <h1>"simple-books"</h1>
            <Flash />
            <form class="stack" method="post" action="/login">
                <label>"Username"
                    <input name="username" autocomplete="username" required autofocus />
                </label>
                <label>"Password"
                    <input name="password" type="password" autocomplete="current-password" required />
                </label>
                <button type="submit">"Log in"</button>
            </form>
        </main>
    }
}

#[component]
pub fn SetupPage() -> impl IntoView {
    let token = use_params_map()
        .read_untracked()
        .get("token")
        .unwrap_or_default();
    let state = app_state();
    let action = format!("/setup/{token}");
    let first_run = state.first_run.is_valid(&token, now());

    view! {
        <Title text=if first_run { "Set up simple-books" } else { "Choose a password" } />
        <main class="narrow">
            <h1>"simple-books"</h1>
            <Suspense>
                {Suspend::new(async move {
                    if first_run {
                        return view! {
                            <Flash />
                            <p>
                                "Welcome. Choose the username and password for the admin account. "
                                "You'll use it to add everyone else."
                            </p>
                            <form class="stack" method="post" action=action.clone()>
                                <label>"Username"
                                    <input name="username" autocomplete="username" required autofocus />
                                </label>
                                <PasswordFields autofocus=false />
                                <button type="submit">"Create and log in"</button>
                            </form>
                        }
                        .into_any();
                    }
                    match accounts::setup_link_user(&state.db, &token).await {
                        Err(error) => server_error(&error),
                        Ok(None) => view! {
                            <p class="flash error">
                                "This link has expired or was already used. Ask your admin for a new one."
                            </p>
                        }
                        .into_any(),
                        Ok(Some(username)) => view! {
                            <Flash />
                            <p>"Hello " {username} ". Choose a password to finish setting up."</p>
                            <form class="stack" method="post" action=action.clone()>
                                <PasswordFields autofocus=true />
                                <button type="submit">"Save and log in"</button>
                            </form>
                        }
                        .into_any(),
                    }
                })}
            </Suspense>
        </main>
    }
}

#[component]
fn PasswordFields(autofocus: bool) -> impl IntoView {
    view! {
        <label>"Password"
            <input
                name="password"
                type="password"
                autocomplete="new-password"
                minlength=MIN_PASSWORD_CHARS.to_string()
                required
                autofocus=autofocus
            />
        </label>
        <label>"The same again"
            <input name="confirm" type="password" autocomplete="new-password" required />
        </label>
    }
}

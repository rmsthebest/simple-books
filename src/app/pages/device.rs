//! Connecting a Kobo or pairing a simple-books reader, and looking after them.

use leptos::prelude::*;

use super::{Layout, app_state, request_parts, server_error, signed_in, time_ago};
use crate::{
    app::connect_kobo::ConnectKobo,
    server::{
        db::{accounts::DeviceSummary, hardcover::Overview},
        now,
        web::Revealed,
    },
};

/// A Kobo that hasn't synced for this long may have lost its connection.
const STALE_SECONDS: i64 = 14 * 24 * 60 * 60;

#[component]
pub fn DevicePage() -> impl IntoView {
    let user = signed_in();
    let state = app_state();
    let endpoint = match request_parts().extensions.get::<Revealed>() {
        Some(Revealed::KoboEndpoint(endpoint)) => Some(endpoint.clone()),
        _ => None,
    };

    user.map(|user| {
        let user_id = user.id;
        let scope = user.scope.clone();
        let hardcover_on = state.hardcover.is_some();
        view! {
            <Layout user title="My Kobo">
                <h1>"My Kobo"</h1>
                {endpoint.map(|endpoint| view! {
                    <section class="callout">
                        <ConnectKobo endpoint=endpoint.clone() />
                        <details>
                            <summary>"Set it by hand instead"</summary>
                            <p>
                                "In " <code>".kobo/Kobo/Kobo eReader.conf"</code> ", under "
                                <code>"[OneStoreServices]"</code> ", set this line:"
                            </p>
                            <input
                                class="reveal"
                                readonly
                                aria-label="The line to set"
                                value=format!("api_endpoint={endpoint}")
                            />
                            <p class="muted">"It's shown only now. Keep it private: it opens your library."</p>
                        </details>
                    </section>
                })}
                <Suspense>
                    {Suspend::new(async move {
                        match state.db.list_devices(user_id).await {
                            Ok(devices) => view! { <DeviceList devices=devices /> }.into_any(),
                            Err(error) => server_error(&error),
                        }
                    })}
                </Suspense>
                {hardcover_on.then(|| view! {
                    <Suspense>
                        {Suspend::new({
                            let scope = scope.clone();
                            async move {
                                match scope.hardcover_overview().await {
                                    Ok(overview) => view! { <HardcoverSection overview=overview /> }.into_any(),
                                    Err(error) => server_error(&error),
                                }
                            }
                        })}
                    </Suspense>
                })}
            </Layout>
        }
    })
}

#[component]
fn DeviceList(devices: Vec<DeviceSummary>) -> impl IntoView {
    let empty = devices.is_empty();
    view! {
        <ul class="devices">
            {devices.into_iter().map(|device| view! { <DeviceRow device=device /> }).collect_view()}
        </ul>
        <form method="post" action="/device/connect">
            <button class=if empty { "" } else { "quiet" } type="submit">
                {if empty { "Connect your Kobo" } else { "Connect another Kobo" }}
            </button>
        </form>
        <details class="pair">
            <summary>"Pair a simple-books reader"</summary>
            <form method="post" action="/device/pair">
                <label>
                    "The code your reader shows"
                    <input
                        name="code"
                        required
                        autocomplete="off"
                        autocapitalize="characters"
                        spellcheck="false"
                        maxlength="16"
                        placeholder="ABCD-EFGH"
                    />
                </label>
                <button type="submit">"Pair"</button>
            </form>
        </details>
    }
}

#[component]
fn DeviceRow(device: DeviceSummary) -> impl IntoView {
    let id = device.id;
    let reader = device.reader;
    let stale = device
        .last_seen_at
        .is_some_and(|seen| now() - seen > STALE_SECONDS);
    let seen = match device.last_seen_at {
        Some(seen) => format!("Connected. Last synced {}.", time_ago(seen)),
        None => "Waiting for its first sync.".to_owned(),
    };
    view! {
        <li class="device">
            <strong>{device.name}</strong>
            {reader.then(|| view! { <span class="muted">"simple-books reader"</span> })}
            <span>{seen}</span>
            {stale.then(|| view! {
                <span class="warning">
                    {if reader {
                        "It hasn't synced in a while. If it should have, remove it and pair it again."
                    } else {
                        "It hasn't synced in a while. If it should have, connect it again."
                    }}
                </span>
            })}
            <div class="actions">
                {(!reader).then(|| view! {
                    <form method="post" action=format!("/device/{id}/reconnect")>
                        <button class="quiet" type="submit">"Connect again"</button>
                    </form>
                })}
                <form method="post" action=format!("/device/{id}/resync")>
                    <button class="quiet" type="submit">"Send everything again"</button>
                </form>
                <details>
                    <summary>"Remove"</summary>
                    <p>"It won't be able to sync here any more."</p>
                    <form method="post" action=format!("/device/{id}/remove")>
                        <button class="danger" type="submit">"Yes, remove it"</button>
                    </form>
                </details>
            </div>
        </li>
    }
}

#[component]
fn HardcoverSection(overview: Overview) -> impl IntoView {
    let Overview {
        username,
        broken,
        code,
        outcome,
        missing,
    } = overview;
    let body = if let Some((code, url)) = code {
        view! {
            <p>"On your phone or computer, go to Hardcover and enter this code:"</p>
            <p class="code" aria-label="Your code">{code}</p>
            <p><a class="button" href=url target="_blank" rel="noopener noreferrer">"Open Hardcover"</a></p>
            <p class="muted">"This page updates by itself once you've approved it there."</p>
            <form method="post" action="/hardcover/cancel">
                <button class="quiet" type="submit">"Cancel"</button>
            </form>
        }
        .into_any()
    } else if let (Some(name), false) = (username.clone(), broken) {
        view! {
            <p>
                "Connected as " <strong>{name}</strong>
                ". What you read on your Kobo shows up there: new books as want to read, then "
                "your progress, then read when you finish."
            </p>
            {(!missing.is_empty()).then(|| view! {
                <p class="warning">"These couldn't be found on Hardcover. Open one to say which book it is:"</p>
                <ul>
                    {missing.into_iter().map(|(id, title)| view! {
                        <li><a href=format!("/books/{id}")>{title}</a></li>
                    }).collect_view()}
                </ul>
            })}
            <details>
                <summary>"Disconnect"</summary>
                <p>"Nothing more is sent to Hardcover. What's there already stays."</p>
                <form method="post" action="/hardcover/disconnect">
                    <button class="danger" type="submit">"Yes, disconnect"</button>
                </form>
            </details>
        }
        .into_any()
    } else {
        let note = if broken {
            Some("Hardcover stopped accepting the connection. Connect again to carry on.")
        } else {
            match outcome.as_deref() {
                Some("denied") => Some("It wasn't approved on Hardcover."),
                Some("expired") => {
                    Some("The code ran out before it was entered. Here's another try.")
                }
                Some(_) => Some("That didn't work. Please try again."),
                None => None,
            }
        };
        view! {
            {note.map(|note| view! { <p class="warning">{note}</p> })}
            <p>"Track what you read on your Kobo on Hardcover, automatically."</p>
            <form method="post" action="/hardcover/connect">
                <button type="submit">"Connect Hardcover"</button>
            </form>
        }
        .into_any()
    };
    view! {
        <section class="hardcover">
            <h2>"Hardcover"</h2>
            {body}
        </section>
    }
}

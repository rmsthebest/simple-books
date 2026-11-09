//! The bootstrap: the first admin comes from the link a new server prints, or from
//! the command line.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use simple_books::server::{auth::accounts, db::Db, storage::layout::Layout};

const URL: &str = "https://books.example.com";
const PASSWORD: &str = "correct horse battery";

fn admin(data: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_simple-books"))
        .env_clear()
        .env("PUBLIC_URL", URL)
        .env("DATA_DIR", data)
        .arg("admin")
        .args(args)
        .output()
        .unwrap()
}

/// The setup token from the printed link. Exactly one line holds it.
fn token_in(output: &std::process::Output) -> String {
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let prefix = format!("{URL}/setup/");
    let links: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .collect();
    assert_eq!(links.len(), 1, "{stdout}");
    links[0].to_owned()
}

#[tokio::test]
async fn first_admin_comes_from_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    let token = token_in(&admin(data, &["create-user", "ann", "--admin"]));
    let bob = token_in(&admin(data, &["create-user", "bob", "--quota-mb", "10"]));

    // The process's umask keeps even files SQLite makes for itself private.
    let database = Layout::new(data).database();
    let mode = std::fs::metadata(&database).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");

    let db = Db::open(&database).await.unwrap();
    let users = db.list_users().await.unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].username, "ann");
    assert!(users[0].is_admin);
    assert!(!users[0].has_password);
    assert_eq!(users[0].quota_bytes, 5000 * 1024 * 1024, "the default");
    assert_eq!(users[1].username, "bob");
    assert!(!users[1].is_admin);
    assert_eq!(users[1].quota_bytes, 10 * 1024 * 1024);
    assert_ne!(token, bob);

    accounts::complete_setup(&db, &token, PASSWORD.to_owned())
        .await
        .unwrap();
    let login = accounts::log_in(&db, "ann", PASSWORD.to_owned());
    assert!(login.await.unwrap().is_some());

    // A new link is how a forgotten password gets reset.
    let again = token_in(&admin(data, &["setup-link", "ann"]));
    accounts::complete_setup(&db, &again, "a brand new password".to_owned())
        .await
        .unwrap();
    let login = accounts::log_in(&db, "ann", PASSWORD.to_owned());
    assert!(login.await.unwrap().is_none());
}

#[test]
fn mistakes_fail_with_a_message_and_no_link() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    token_in(&admin(data, &["create-user", "ann"]));

    for (args, expected) in [
        (&["create-user", "ANN"][..], "taken"),
        (&["create-user", " spaced"][..], "usernames"),
        (&["setup-link", "nobody"][..], "no such user"),
    ] {
        let output = admin(data, args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("/setup/"), "{args:?}: {stdout}");
    }
}

/// Starts the server, and returns the setup link's path it prints, if any, once it's
/// listening. Then stops it.
fn start_and_stop(data: &Path) -> Option<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_simple-books"))
        .env_clear()
        .env("PUBLIC_URL", URL)
        .env("DATA_DIR", data)
        .env("LISTEN_ADDR", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, read) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut printed = None;
    loop {
        let line = read
            .recv_timeout(Duration::from_secs(30))
            .expect("the server to start");
        if let Some(path) = line.strip_prefix(URL) {
            assert!(printed.is_none(), "one link only");
            assert!(path.starts_with("/setup/"), "{line}");
            printed = Some(path.to_owned());
        }
        if line.contains("listening") {
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    printed
}

#[test]
fn a_new_server_prints_a_link_until_someone_exists() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let first = start_and_stop(data).expect("a link with no users");
    let second = start_and_stop(data).expect("a new link on each start");
    assert_ne!(first, second);

    token_in(&admin(data, &["create-user", "ann"]));
    assert_eq!(start_and_stop(data), None);
}

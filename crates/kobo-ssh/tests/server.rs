//! The SSH server against russh's own client, on localhost, with the
//! desktop's `/bin/sh`.

use std::{
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

use kobo_ssh::{AUTHORIZED_KEYS_FILE, Settings, host_key, serve};
use russh::{
    ChannelMsg, MethodKind,
    client::AuthResult,
    client::{self, Handle},
    keys::{
        PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate, ssh_key::private::Ed25519Keypair,
    },
};
use tokio::{net::TcpListener, sync::oneshot};

const PASSWORD: &str = "abcd-efgh";

struct Client;

// The trait's methods return futures, whether they wait or not.
#[allow(clippy::unused_async_trait_impl)]
impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// A fresh app folder for one test, under Cargo's scratch space.
fn folder(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("kobo-ssh-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let _ = std::fs::remove_file(dir.join(AUTHORIZED_KEYS_FILE));
    dir
}

/// Starts a server; dropping the sender stops it.
async fn start(dir: PathBuf) -> (u16, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let key = host_key(&dir).unwrap();
    let settings = Settings {
        dir,
        password: PASSWORD.to_owned(),
        shell: PathBuf::from("/bin/sh"),
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        serve(listener, settings, key, async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    (port, stop, server)
}

async fn connect(port: u16) -> Handle<Client> {
    let config = Arc::new(client::Config::default());
    client::connect(config, ("127.0.0.1", port), Client)
        .await
        .unwrap()
}

async fn logged_in(port: u16) -> Handle<Client> {
    let mut client = connect(port).await;
    let result = client
        .authenticate_password("root", PASSWORD)
        .await
        .unwrap();
    assert!(result.success());
    client
}

/// Runs `command`, sending `input` then EOF; returns stdout, stderr and the
/// exit status.
async fn run(
    client: &Handle<Client>,
    command: &str,
    input: &[u8],
) -> (String, String, Option<u32>) {
    let mut channel = client.channel_open_session().await.unwrap();
    channel.exec(true, command).await.unwrap();
    if !input.is_empty() {
        channel.data(input).await.unwrap();
    }
    channel.eof().await.unwrap();
    let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), None);
    while let Some(message) = tokio::time::timeout(Duration::from_secs(10), channel.wait())
        .await
        .expect("an answer")
    {
        match message {
            ChannelMsg::Data { data } => out.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => err.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
            _ => {}
        }
    }
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        code,
    )
}

#[tokio::test]
async fn the_password_logs_in_and_commands_run() {
    let (port, _stop, _server) = start(folder("password")).await;
    let mut wrong = connect(port).await;
    // A mistyped password can be typed again on the same connection: the
    // server still offers passwords (OpenSSH only tries what's offered),
    // and nothing it can't do.
    match wrong
        .authenticate_password("root", "abcd-efgi")
        .await
        .unwrap()
    {
        AuthResult::Failure {
            remaining_methods, ..
        } => assert_eq!(
            *remaining_methods,
            [MethodKind::Password, MethodKind::PublicKey][..]
        ),
        AuthResult::Success => panic!("a wrong password logged in"),
    }
    assert!(
        wrong
            .authenticate_password("root", PASSWORD)
            .await
            .unwrap()
            .success()
    );
    let mut other_user = connect(port).await;
    assert!(
        !other_user
            .authenticate_password("tony", PASSWORD)
            .await
            .unwrap()
            .success()
    );

    let client = logged_in(port).await;
    let (out, err, code) = run(&client, "echo hi; echo oops >&2; exit 3", b"").await;
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("hi\n", "oops\n", Some(3))
    );
    // Input piped in, as `tar | ssh 'tar -x'` does.
    let (out, _, code) = run(&client, "cat; echo done", b"some bytes\n").await;
    assert_eq!((out.as_str(), code), ("some bytes\ndone\n", Some(0)));
}

#[tokio::test]
async fn a_listed_key_logs_in_and_another_doesnt() {
    let dir = folder("keys");
    let listed = PrivateKey::from(Ed25519Keypair::from_seed(&[7; 32]));
    let unlisted = PrivateKey::from(Ed25519Keypair::from_seed(&[8; 32]));
    let line = listed.public_key().to_openssh().unwrap();
    std::fs::write(dir.join(AUTHORIZED_KEYS_FILE), format!("# mine\n{line}\n")).unwrap();
    let (port, _stop, _server) = start(dir).await;

    for (key, works) in [(unlisted, false), (listed, true)] {
        let mut client = connect(port).await;
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
        let result = client.authenticate_publickey("root", key).await.unwrap();
        assert_eq!(result.success(), works);
    }
}

#[tokio::test]
async fn a_shell_on_a_terminal_answers() {
    let (port, _stop, _server) = start(folder("terminal")).await;
    let client = logged_in(port).await;
    let mut channel = client.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(true).await.unwrap();
    channel
        .data(&b"echo $((6 * 7)); exit 5\n"[..])
        .await
        .unwrap();
    let (mut seen, mut code) = (Vec::new(), None);
    while let Some(message) = tokio::time::timeout(Duration::from_secs(10), channel.wait())
        .await
        .expect("an answer")
    {
        match message {
            ChannelMsg::Data { data } => seen.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
            _ => {}
        }
    }
    let seen = String::from_utf8_lossy(&seen);
    assert!(seen.contains("42"), "{seen}");
    assert_eq!(code, Some(5));
}

#[test]
fn the_server_stops_when_its_stdin_ends() {
    let dir = folder("stdin");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server = Command::new(env!("CARGO_BIN_EXE_simple-books-ssh"))
        .args(["--dir", dir.to_str().unwrap(), "--port", &port.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    writeln!(stdin, "{PASSWORD}").unwrap();
    // It listens.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let client = logged_in(port).await;
        let (out, _, _) = run(&client, "echo up", b"").await;
        assert_eq!(out, "up\n");
    });
    drop(stdin);
    for _ in 0..50 {
        if let Some(status) = server.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = server.kill();
    panic!("still running after its stdin ended");
}

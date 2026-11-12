//! The SSH server the simple-books reader starts on the Kobo, in place of
//! KOReader's dropbear, so nothing else has to be installed.
//!
//! - **Who can log in:** root, with the password the reader shows on its
//!   Sync screen while SSH is on, or with a key listed in `authorized_keys`
//!   in the app folder (OpenSSH format, one per line). Each attempt that
//!   fails waits a second, and a connection gets a few tries.
//! - **What they get:** a shell, with or without a terminal, and commands
//!   run with `sh -c`, so `ssh kobo 'tar -x'` with a pipe works. There's no
//!   port forwarding and no SFTP, so `scp` needs `-O`.
//! - **The host key:** ed25519, made on the first start and kept in
//!   `ssh-host-key` in the app folder.
//!
//! [`serve`] runs until told to stop, then ends every shell it started.

mod session;

use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use russh::{
    Channel, ChannelId, MethodKind, MethodSet,
    keys::{
        HashAlg, PrivateKey, PublicKey,
        ssh_key::{LineEnding, private::Ed25519Keypair},
    },
    server::{
        self, Auth, ChannelOpenHandle, Msg, PublicKeyOffer, Server as _, Session, VerifiedPublicKey,
    },
};
use tokio::net::TcpListener;

/// The host key, in the app folder.
pub const HOST_KEY_FILE: &str = "ssh-host-key";
/// Keys that may log in, in the app folder.
pub const AUTHORIZED_KEYS_FILE: &str = "authorized_keys";
/// The only user.
const USER: &str = "root";
/// Tries at logging in, per connection.
const ATTEMPTS: usize = 4;

/// The ways to log in: nothing else is offered.
fn methods() -> MethodSet {
    MethodSet::from(&[MethodKind::Password, MethodKind::PublicKey][..])
}

/// No: try again. russh's plain reject drops the method just tried, which
/// would make a mistyped password end the connection; this keeps both, and
/// `max_auth_attempts` still limits the tries.
fn again() -> Auth {
    Auth::Reject {
        proceed_with_methods: Some(methods()),
        partial_success: false,
    }
}

/// What the server needs: where the app folder is, and the password.
#[derive(Clone)]
pub struct Settings {
    pub dir: PathBuf,
    pub password: String,
    /// Run for a shell and for each command: `/bin/sh` on the Kobo.
    pub shell: PathBuf,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("dir", &self.dir)
            .field("password", &"…")
            .field("shell", &self.shell)
            .finish()
    }
}

/// Process groups of the shells and commands running, to end them all when
/// the server stops.
#[derive(Debug, Default, Clone)]
pub(crate) struct Running(Arc<Mutex<HashSet<i32>>>);

impl Running {
    pub(crate) fn add(&self, group: i32) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(group);
    }

    pub(crate) fn remove(&self, group: i32) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&group);
    }

    fn end_all(&self) {
        for group in self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
        {
            session::end_group(group);
        }
    }
}

/// Logs one line to stderr, which the reader sends to `reader.log`.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        eprintln!("{now} ssh: {}", format_args!($($arg)*));
    }};
}

/// Loads the host key from the app folder, or makes one and keeps it.
pub fn host_key(dir: &Path) -> io::Result<PrivateKey> {
    let path = dir.join(HOST_KEY_FILE);
    if let Ok(text) = fs::read_to_string(&path) {
        return PrivateKey::from_openssh(text).map_err(io::Error::other);
    }
    let mut seed = [0; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    let text = key.to_openssh(LineEnding::LF).map_err(io::Error::other)?;
    let temporary = path.with_extension("new");
    fs::write(&temporary, text.as_bytes())?;
    fs::rename(&temporary, &path)?;
    log!("made a host key");
    Ok(key)
}

/// Whether `key` is listed in `authorized_keys`. Read each time, so a key
/// added while SSH is on works at once.
fn authorized(dir: &Path, key: &PublicKey) -> bool {
    let Ok(text) = fs::read_to_string(dir.join(AUTHORIZED_KEYS_FILE)) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| PublicKey::from_openssh(line).ok())
        .any(|listed| listed.key_data() == key.key_data())
}

/// Compares in time that doesn't depend on where they differ.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// Serves on `listener` until `stop` finishes, then ends every shell and
/// command it started.
pub async fn serve(
    listener: TcpListener,
    settings: Settings,
    host_key: PrivateKey,
    stop: impl Future<Output = ()>,
) -> io::Result<()> {
    log!(
        "on at {}, host key {}",
        listener.local_addr()?,
        host_key.fingerprint(HashAlg::Sha256)
    );
    let config = Arc::new(server::Config {
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::ZERO),
        max_auth_attempts: ATTEMPTS,
        methods: methods(),
        inactivity_timeout: Some(Duration::from_secs(3600)),
        keys: vec![host_key],
        nodelay: true,
        ..Default::default()
    });
    let running = Running::default();
    let mut server = Server {
        settings: Arc::new(settings),
        running: running.clone(),
    };
    let result = tokio::select! {
        result = server.run_on_socket(config, &listener) => result,
        () = stop => Ok(()),
    };
    running.end_all();
    log!("off");
    result
}

struct Server {
    settings: Arc<Settings>,
    running: Running,
}

impl server::Server for Server {
    type Handler = Client;

    fn new_client(&mut self, address: Option<SocketAddr>) -> Client {
        Client {
            settings: self.settings.clone(),
            running: self.running.clone(),
            address,
        }
    }

    fn handle_session_error(&mut self, error: russh::Error) {
        log!("{error}");
    }
}

/// One connection.
struct Client {
    settings: Arc<Settings>,
    running: Running,
    address: Option<SocketAddr>,
}

impl Client {
    fn from(&self) -> String {
        self.address
            .map_or_else(|| "somewhere".to_owned(), |a| a.ip().to_string())
    }
}

// The trait's methods return futures, whether they wait for anything or not.
#[allow(clippy::unused_async_trait_impl)]
impl server::Handler for Client {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == USER && same(password, &self.settings.password) {
            log!("{} logged in with the password", self.from());
            Ok(Auth::Accept)
        } else {
            log!("{} gave a wrong password", self.from());
            Ok(again())
        }
    }

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        offer: PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(
            if user == USER && authorized(&self.settings.dir, offer.public_key) {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        if user == USER && authorized(&self.settings.dir, key.public_key) {
            log!("{} logged in with a key", self.from());
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        tokio::spawn(session::run(
            channel,
            self.settings.clone(),
            self.running.clone(),
        ));
        Ok(())
    }

    // The channel's task does the work; these only say yes.
    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        log!("no {name} here");
        session.channel_failure(channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_password_is_the_same() {
        assert!(same("abcd-efgh", "abcd-efgh"));
        assert!(!same("abcd-efgh", "abcd-efgi"));
        assert!(!same("abcd-efgh", "abcd-efg"));
        assert!(!same("", "a"));
    }
}

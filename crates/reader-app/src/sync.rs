//! Syncing with the server on another thread, so reading carries on.
//!
//! The session asks for a job ([`Syncer::pair`], [`Syncer::sync`]); the
//! thread brings the network up through the platform's [`Network`], does
//! the job with `reader-sync`, takes the network down, and reports through
//! a channel the session reads on each tick ([`Syncer::updates`]). One job
//! runs at a time.
//!
//! Turning on SSH ([`Syncer::ssh_start`]) is a job too, one that leaves the
//! network up until [`Syncer::ssh_stop`].

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use reader_sync::{Client, Error, PairStatus, Paths, Progress, Report, Spot, State};
use serde::{Deserialize, Serialize};

/// What the platform does to be online: on the Kobo, Wi-Fi is off unless a
/// sync needs it.
pub trait Network: Send + Sync {
    /// Brings the network up, waiting until it's usable. The message says
    /// why not, for the screen.
    fn up(&self) -> Result<(), String>;
    /// Takes it down again once the job's done. Each [`Network::up`] that
    /// worked is matched by one of these, and the network stays up while
    /// any job still needs it.
    fn down(&self);
    /// Whether it's up now, for the status: Wi-Fi shows only on the Kobo,
    /// where it's off most of the time. Mustn't wait for an `up` under way.
    fn is_up(&self) -> bool {
        false
    }
}

/// A way in over the network, for working on the device: SSH on the Kobo.
/// Anyone on the network may be able to log in while it's on, so it's
/// turned off before sleeping.
pub trait Ssh: Send + Sync {
    /// Starts it, with the network up. Returns how to log in, for the
    /// screen, or why it didn't start.
    fn start(&self) -> Result<SshLogin, String>;
    fn stop(&self);
}

/// How to log in while SSH is on, for the sync screen.
#[derive(Clone, PartialEq, Eq)]
pub struct SshLogin {
    /// Such as `ssh -p 2222 root@192.168.1.94`.
    pub command: String,
    /// The password, made fresh each time it's turned on, if there is one.
    pub password: Option<String>,
}

impl std::fmt::Debug for SshLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Shown on the screen, but never logged.
        f.debug_struct("SshLogin")
            .field("command", &self.command)
            .field("password", &self.password.as_ref().map(|_| "…"))
            .finish()
    }
}

/// A network that's always there, as on a desktop.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysOnline;

impl Network for AlwaysOnline {
    fn up(&self) -> Result<(), String> {
        Ok(())
    }

    fn down(&self) {}
}

/// How the platform syncs.
#[derive(Clone)]
pub struct SyncSetup {
    /// The app folder: `sync.json`, the secret, and `books/`.
    pub dir: PathBuf,
    /// The website's address, if one is known.
    pub server: Option<String>,
    /// What the website calls this reader.
    pub name: String,
    pub network: Arc<dyn Network>,
    /// SSH, where the platform has it.
    pub ssh: Option<Arc<dyn Ssh>>,
    /// Whether the app updates itself from the server, as on the Kobo: each
    /// sync then downloads a version the server has that this isn't (see
    /// [`reader_sync::update`]), for [`Device::update`](crate::Device::update).
    pub updates: bool,
}

impl std::fmt::Debug for SyncSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncSetup")
            .field("dir", &self.dir)
            .field("server", &self.server)
            .field("name", &self.name)
            .field("ssh", &self.ssh.is_some())
            .field("updates", &self.updates)
            .finish_non_exhaustive()
    }
}

/// What the thread says as it goes. [`Update::Done`] is always last.
#[derive(Debug)]
pub(crate) enum Update {
    Connecting,
    Progress(Progress),
    /// A code to show while pairing.
    Code {
        code: String,
        url: String,
    },
    Paired {
        user: String,
    },
    Synced(Report),
    /// It didn't work; this says why, for the screen.
    Failed(String),
    /// The server no longer knows this reader: it was removed on the
    /// website. Its secret is forgotten.
    Unpaired,
    /// SSH is on; this says how to log in.
    SshOn(SshLogin),
    /// SSH didn't start; this says why.
    SshFailed(String),
    /// After a sync: the version downloaded and ready to update to, if
    /// there is one.
    NewVersion(Option<String>),
    /// Downloading an update didn't work; this says why.
    NewVersionFailed(String),
    Done,
}

/// What the sync screen shows while a job runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Activity {
    Connecting,
    Pairing { code: Option<(String, String)> },
    Syncing(String),
    StartingSsh,
}

/// Where SSH is, for the sync screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SshState {
    Off {
        problem: Option<String>,
    },
    /// How to log in.
    On(SshLogin),
}

/// How the last job ended, kept across restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Status {
    /// Whose library it's paired with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<Last>,
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Last {
    /// In seconds since 1970.
    pub at: u64,
    pub worked: bool,
    /// What came of it, such as "2 new books".
    pub text: String,
}

impl Last {
    /// As the sync screen puts it, `now` being the time in seconds since
    /// 1970.
    pub(crate) fn shown(&self, now: u64) -> String {
        let when = crate::sync_page::ago(now.saturating_sub(self.at));
        if self.worked {
            format!("Last synced {when}: {}", self.text)
        } else {
            format!("Last tried {when}: {}", self.text)
        }
    }
}

/// What a sync did, in a few words.
pub(crate) fn summary(report: &Report) -> String {
    let mut parts = Vec::new();
    let count = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    if report.sent > 0 {
        parts.push(count(report.sent, "place sent", "places sent"));
    }
    if !report.downloaded.is_empty() {
        parts.push(count(
            report.downloaded.len(),
            "book downloaded",
            "books downloaded",
        ));
    }
    if !report.removed.is_empty() {
        parts.push(count(report.removed.len(), "book removed", "books removed"));
    }
    if !report.changed.is_empty() {
        parts.push(count(report.changed.len(), "book updated", "books updated"));
    }
    if !report.places.is_empty() {
        parts.push(count(
            report.places.len(),
            "place from elsewhere",
            "places from elsewhere",
        ));
    }
    if !report.failed.is_empty() {
        parts.push(count(
            report.failed.len(),
            "download to try again",
            "downloads to try again",
        ));
    }
    if parts.is_empty() {
        "everything was up to date.".to_owned()
    } else {
        parts.join(", ") + "."
    }
}

pub(crate) const STATUS: &str = "sync-status.json";

struct Running {
    updates: mpsc::Receiver<Update>,
    stop: Arc<AtomicBool>,
}

pub(crate) struct Syncer {
    pub setup: SyncSetup,
    paths: Paths,
    running: Option<Running>,
    pub activity: Option<Activity>,
    pub status: Status,
    /// When the last pairing or sync started, to sync again only now and
    /// then.
    pub started: Option<Instant>,
    /// How to log in while SSH is on.
    ssh_on: Option<SshLogin>,
    /// Why SSH last didn't start.
    ssh_problem: Option<String>,
    /// The version downloaded and ready to update to.
    update_ready: Option<String>,
    /// Why the last update didn't download or go in.
    update_problem: Option<String>,
}

impl std::fmt::Debug for Syncer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syncer")
            .field("setup", &self.setup)
            .field("activity", &self.activity)
            .finish_non_exhaustive()
    }
}

impl Syncer {
    pub(crate) fn new(setup: SyncSetup, status: Status) -> Self {
        let paths = Paths::in_dir(&setup.dir);
        let update_ready = if setup.updates {
            reader_sync::update::ready(&setup.dir)
        } else {
            None
        };
        Self {
            setup,
            paths,
            running: None,
            activity: None,
            status,
            started: None,
            ssh_on: None,
            ssh_problem: None,
            update_ready,
            update_problem: None,
        }
    }

    /// The version running, if the app updates itself.
    pub(crate) fn version(&self) -> Option<String> {
        self.setup
            .updates
            .then(|| reader_sync::update::running(&self.setup.dir))
            .flatten()
    }

    /// The version ready to update to, if one has been downloaded.
    pub(crate) fn update_ready(&self) -> Option<&str> {
        self.update_ready.as_deref()
    }

    /// Why the last update didn't download or go in, if it didn't, as a
    /// sentence for the screen.
    pub(crate) fn update_problem(&self) -> Option<&str> {
        self.update_problem.as_deref()
    }

    /// Updating didn't work; this says why.
    pub(crate) fn update_failed(&mut self, why: String) {
        self.update_problem = Some(why);
    }

    /// The update was put in place; it's not ready any more.
    pub(crate) fn updated(&mut self) {
        self.update_ready = None;
    }

    /// Where SSH is, if the platform has it.
    pub(crate) fn ssh(&self) -> Option<SshState> {
        self.setup.ssh.as_ref()?;
        Some(match &self.ssh_on {
            Some(login) => SshState::On(login.clone()),
            None => SshState::Off {
                problem: self.ssh_problem.clone(),
            },
        })
    }

    /// How to log in, while SSH is on.
    pub(crate) fn ssh_login(&self) -> Option<&SshLogin> {
        self.ssh_on.as_ref()
    }

    /// Brings the network up and starts SSH, leaving both on.
    pub(crate) fn ssh_start(&mut self) {
        let Some(ssh) = self.setup.ssh.clone() else {
            return;
        };
        if self.busy() || self.ssh_on.is_some() {
            return;
        }
        self.ssh_problem = None;
        self.activity = Some(Activity::StartingSsh);
        self.spawn(move |send, stop, network| match network.up() {
            Err(why) => send(Update::SshFailed(why)),
            Ok(()) if stop.load(Ordering::Relaxed) => {
                network.down();
                send(Update::SshFailed("Stopped.".into()));
            }
            Ok(()) => match ssh.start() {
                Ok(login) => send(Update::SshOn(login)),
                Err(why) => {
                    network.down();
                    send(Update::SshFailed(why));
                }
            },
        });
    }

    /// Stops SSH, and lets the network go down unless a job needs it.
    /// Waits for both.
    pub(crate) fn ssh_stop(&mut self) {
        if self.ssh_on.take().is_some()
            && let Some(ssh) = &self.setup.ssh
        {
            ssh.stop();
            self.setup.network.down();
        }
    }

    pub(crate) fn paired(&self) -> bool {
        self.paths.secret().is_some()
    }

    pub(crate) fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// What's been synced, as the thread last saved it.
    pub(crate) fn state(&self) -> State {
        State::load(&self.paths.state)
    }

    /// Asks a running job to stop soon.
    pub(crate) fn stop(&self) {
        if let Some(running) = &self.running {
            running.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Forgets the pairing and what was synced. The books stay, as books
    /// on the reader.
    pub(crate) fn unpair(&mut self) {
        self.stop();
        let _ = self.paths.forget();
        self.status.user = None;
    }

    /// Starts pairing, then a first sync once the code's been entered.
    pub(crate) fn pair(&mut self, now: Instant) {
        let Some(server) = self.setup.server.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        let name = self.setup.name.clone();
        let paths = self.paths.clone();
        self.activity = Some(Activity::Connecting);
        self.started = Some(now);
        self.spawn(move |send, stop, network| {
            online(network, send, || {
                let client = Client::new(&server);
                pair(&client, &name, &paths, send, stop);
            });
        });
    }

    /// Starts a sync, sending `local`: where the reader is in each synced
    /// book.
    pub(crate) fn sync(&mut self, local: BTreeMap<String, Spot>, now: Instant) {
        let Some(server) = self.setup.server.clone() else {
            return;
        };
        if self.busy() || !self.paired() {
            return;
        }
        let paths = self.paths.clone();
        let updates = self.setup.updates.then(|| self.setup.dir.clone());
        self.activity = Some(Activity::Connecting);
        self.started = Some(now);
        self.spawn(move |send, stop, network| {
            online(network, send, || {
                let client = Client::new(&server);
                if sync_once(&client, &paths, &local, send, stop)
                    && let Some(dir) = updates
                {
                    stage_update(&client, &paths, &dir, send, stop);
                }
            });
        });
    }

    /// Sends the places the server doesn't have yet, if there are any, and
    /// nothing else: before sleeping. It doesn't count as a sync for
    /// [`Syncer::started`], so waking still checks for new books. True if it
    /// started.
    pub(crate) fn send_places(&mut self, local: BTreeMap<String, Spot>) -> bool {
        let Some(server) = self.setup.server.clone() else {
            return false;
        };
        if self.busy() || !self.paired() || !reader_sync::unsent(&self.state(), &local) {
            return false;
        }
        let paths = self.paths.clone();
        self.activity = Some(Activity::Connecting);
        self.spawn(move |send, stop, network| {
            online(network, send, || {
                let client = Client::new(&server);
                send_once(&client, &paths, &local, send, stop);
            });
        });
        true
    }

    fn spawn(
        &mut self,
        job: impl FnOnce(&dyn Fn(Update), &AtomicBool, &dyn Network) + Send + 'static,
    ) {
        let (sender, updates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let network = Arc::clone(&self.setup.network);
        std::thread::spawn(move || {
            let send = |update| {
                let _ = sender.send(update);
            };
            send(Update::Connecting);
            job(&send, &thread_stop, network.as_ref());
            send(Update::Done);
        });
        self.running = Some(Running { updates, stop });
    }

    /// The thread's news since last asked. Notes what the screen should
    /// show as it goes.
    pub(crate) fn updates(&mut self) -> Vec<Update> {
        let Some(running) = &self.running else {
            return Vec::new();
        };
        let mut updates = Vec::new();
        loop {
            match running.updates.try_recv() {
                Ok(update) => updates.push(update),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Gone without saying it was done, if it panicked.
                    if !updates.iter().any(|u| matches!(u, Update::Done)) {
                        updates.push(Update::Done);
                    }
                    break;
                }
            }
        }
        for update in &updates {
            match update {
                Update::Connecting => {
                    if self.activity.is_none() {
                        self.activity = Some(Activity::Connecting);
                    }
                }
                Update::Code { code, url } => {
                    self.activity = Some(Activity::Pairing {
                        code: Some((code.clone(), url.clone())),
                    });
                }
                Update::Paired { user } => {
                    self.status.user = Some(user.clone());
                    self.activity = Some(Activity::Syncing("Getting your library…".into()));
                }
                Update::Progress(progress) => {
                    self.activity = Some(Activity::Syncing(progress_text(progress)));
                }
                Update::Unpaired => self.status.user = None,
                Update::SshOn(login) => self.ssh_on = Some(login.clone()),
                Update::SshFailed(why) => self.ssh_problem = Some(why.clone()),
                Update::NewVersion(version) => {
                    self.update_ready.clone_from(version);
                    self.update_problem = None;
                }
                Update::NewVersionFailed(why) => {
                    self.update_problem = Some(format!("The update didn't download: {why}"));
                }
                Update::Synced(_) | Update::Failed(_) => {}
                Update::Done => {
                    self.running = None;
                    self.activity = None;
                }
            }
        }
        updates
    }
}

fn progress_text(progress: &Progress) -> String {
    match progress {
        Progress::Sending => "Sending where you are in your books…".to_owned(),
        Progress::Checking => "Checking for changes…".to_owned(),
        Progress::Downloading { done, total, title } => {
            format!("Downloading {} of {total}: {title}", done + 1)
        }
        Progress::Updating => "Checking for a new version of Simple Books…".to_owned(),
    }
}

/// What to say about a failure, for the screen.
fn failure(error: &Error) -> String {
    match error {
        Error::Network(_) => "Couldn't reach the server.".to_owned(),
        Error::NotPaired => "This reader was removed on the website. Pair it again.".to_owned(),
        Error::Stopped => "Stopped.".to_owned(),
        Error::Server { message, .. } => format!("The server said: {message}"),
        Error::NotFound => "The server didn't have something it listed.".to_owned(),
        Error::Damaged(_) => "A download was damaged. It'll be tried again.".to_owned(),
        Error::Io(error) => format!("Couldn't save: {error}"),
    }
}

/// Runs `job` with the network up, or says why it couldn't come up.
fn online(network: &dyn Network, send: &dyn Fn(Update), job: impl FnOnce()) {
    match network.up() {
        Ok(()) => {
            job();
            network.down();
        }
        Err(why) => send(Update::Failed(why)),
    }
}

/// The longest pairing waits, whatever the server says.
const PAIRING_LONGEST: Duration = Duration::from_mins(30);

fn pair(client: &Client, name: &str, paths: &Paths, send: &dyn Fn(Update), stop: &AtomicBool) {
    let start = match client.pair_start(name) {
        Ok(start) => start,
        Err(error) => return send(Update::Failed(failure(&error))),
    };
    send(Update::Code {
        code: start.code.clone(),
        url: start.url.clone(),
    });
    let until =
        Instant::now() + Duration::from_secs(u64::from(start.expires_in)).min(PAIRING_LONGEST);
    let every = Duration::from_secs(u64::from(start.interval.clamp(1, 60)));
    while Instant::now() < until {
        if !wait(every, stop) {
            return send(Update::Failed("Stopped.".into()));
        }
        match client.pair_status(&start.secret) {
            // Still waiting. The network may come back before the code runs
            // out.
            Ok(PairStatus::Pending) | Err(_) => {}
            Ok(PairStatus::Paired { user }) => {
                if let Err(error) = paths.save_secret(&start.secret) {
                    return send(Update::Failed(format!(
                        "Couldn't save the pairing: {error}"
                    )));
                }
                send(Update::Paired { user });
                sync_once(client, paths, &BTreeMap::new(), send, stop);
                return;
            }
            Ok(PairStatus::Expired) => break,
        }
    }
    send(Update::Failed(
        "The code ran out. Pair again for a new one.".into(),
    ));
}

/// Sleeps for `time`, waking early to stop. False if it should stop.
fn wait(time: Duration, stop: &AtomicBool) -> bool {
    let until = Instant::now() + time;
    while Instant::now() < until {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100).min(until - Instant::now()));
    }
    !stop.load(Ordering::Relaxed)
}

/// Syncs once. Whether it worked.
fn sync_once(
    client: &Client,
    paths: &Paths,
    local: &BTreeMap<String, Spot>,
    send: &dyn Fn(Update),
    stop: &AtomicBool,
) -> bool {
    let Some(secret) = paths.secret() else {
        send(Update::Unpaired);
        return false;
    };
    let mut state = State::load(&paths.state);
    let mut progress = |progress| send(Update::Progress(progress));
    match reader_sync::sync(
        client,
        &secret,
        paths,
        &mut state,
        local,
        &mut progress,
        stop,
    ) {
        Ok(report) => {
            send(Update::Synced(report));
            true
        }
        Err(Error::NotPaired) => {
            let _ = paths.forget();
            send(Update::Unpaired);
            false
        }
        Err(error) => {
            send(Update::Failed(failure(&error)));
            false
        }
    }
}

/// After a sync, downloads the version of the app the server has, if it
/// isn't this one.
fn stage_update(
    client: &Client,
    paths: &Paths,
    dir: &std::path::Path,
    send: &dyn Fn(Update),
    stop: &AtomicBool,
) {
    let Some(secret) = paths.secret() else {
        return;
    };
    send(Update::Progress(Progress::Updating));
    match reader_sync::update::stage(client, &secret, dir, stop) {
        Ok(version) => send(Update::NewVersion(version)),
        // Carried on with at the next sync.
        Err(Error::Stopped) => {}
        Err(error) => send(Update::NewVersionFailed(failure(&error))),
    }
}

/// [`reader_sync::send`] once, unless it's to stop first.
fn send_once(
    client: &Client,
    paths: &Paths,
    local: &BTreeMap<String, Spot>,
    send: &dyn Fn(Update),
    stop: &AtomicBool,
) {
    let Some(secret) = paths.secret() else {
        return send(Update::Unpaired);
    };
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let mut state = State::load(&paths.state);
    match reader_sync::send(client, &secret, paths, &mut state, local) {
        Ok(report) => send(Update::Synced(report)),
        Err(Error::NotPaired) => {
            let _ = paths.forget();
            send(Update::Unpaired);
        }
        Err(error) => send(Update::Failed(failure(&error))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_counts_places_sent() {
        assert_eq!(summary(&Report::default()), "everything was up to date.");
        let report = Report {
            sent: 1,
            downloaded: vec!["a".into(), "b".into()],
            ..Report::default()
        };
        assert_eq!(summary(&report), "1 place sent, 2 books downloaded.");
    }
}

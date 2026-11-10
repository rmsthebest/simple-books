//! The requests, one function each.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use sync_proto::{
    BookFile, Changes, PREFIX, PairRequest, PairStart, PairStatus, PlaceAnswer, Problem, Spot,
};
use ureq::{
    Agent,
    http::{Response, StatusCode},
};

/// Why a request didn't work.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server couldn't be reached, or the connection broke.
    #[error("couldn't reach the server: {0}")]
    Network(String),
    /// The secret isn't accepted: the reader was removed on the website, or
    /// its user disabled. It has to pair again.
    #[error("not paired")]
    NotPaired,
    #[error("no such book")]
    NotFound,
    /// The server said no, and why.
    #[error("the server said {status}: {message}")]
    Server { status: u16, message: String },
    /// A download wasn't the file it should have been.
    #[error("the download was damaged: {0}")]
    Damaged(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Io(error) => Self::Network(error.to_string()),
            other => Self::Network(other.to_string()),
        }
    }
}

/// The largest JSON answer read. A page of changes is about 50 kB.
const JSON_LIMIT: u64 = 8 * 1024 * 1024;

/// Talks to one server.
#[derive(Clone)]
pub struct Client {
    base: String,
    agent: Agent,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// `server` is the website's address, such as `https://books.example.com`.
    pub fn new(server: &str) -> Self {
        let agent = Agent::config_builder()
            // Failures are read here, for the server's reason.
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            // A whole book over slow Wi-Fi.
            .timeout_recv_body(Some(Duration::from_mins(10)))
            .user_agent(concat!("simple-books-reader/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            base: format!("{}{PREFIX}", server.trim_end_matches('/')),
            agent,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Asks for a code to pair with. Returns the secret to keep and the code
    /// to show.
    pub fn pair_start(&self, name: &str) -> Result<PairStart, Error> {
        let response = self.agent.post(self.url("/pair")).send_json(PairRequest {
            name: name.to_owned(),
        })?;
        json(response)
    }

    pub fn pair_status(&self, secret: &str) -> Result<PairStatus, Error> {
        let response = self
            .agent
            .get(self.url("/pair"))
            .header("Authorization", bearer(secret))
            .call()?;
        json(response)
    }

    pub fn changes(&self, secret: &str, since: i64) -> Result<Changes, Error> {
        let response = self
            .agent
            .get(self.url(&format!("/changes?since={since}")))
            .header("Authorization", bearer(secret))
            .call()?;
        json(response)
    }

    pub fn put_place(&self, secret: &str, book: &str, spot: &Spot) -> Result<PlaceAnswer, Error> {
        let response = self
            .agent
            .put(self.url(&format!("/books/{book}/place")))
            .header("Authorization", bearer(secret))
            .send_json(spot)?;
        json(response)
    }

    /// The book's cover, as the server has it: a JPEG.
    pub fn cover(&self, secret: &str, book: &str) -> Result<Vec<u8>, Error> {
        let mut response = self
            .agent
            .get(self.url(&format!("/books/{book}/cover")))
            .header("Authorization", bearer(secret))
            .call()?;
        check(&mut response)?;
        Ok(response
            .body_mut()
            .with_config()
            .limit(JSON_LIMIT)
            .read_to_vec()?)
    }

    /// Downloads a book's file to `partial`, carrying on from whatever is
    /// already there, and checks it against `file`. Returns once `partial`
    /// holds the whole, checked file; the caller moves it into place. A
    /// damaged download is deleted, so the next try starts afresh.
    pub fn download(
        &self,
        secret: &str,
        book: &str,
        file: &BookFile,
        partial: &Path,
    ) -> Result<(), Error> {
        let have = fs::metadata(partial).map_or(0, |m| m.len());
        if have > file.size {
            fs::remove_file(partial)?;
            return self.download(secret, book, file, partial);
        }
        if have < file.size {
            let mut request = self
                .agent
                .get(self.url(&format!("/books/{book}")))
                .header("Authorization", bearer(secret));
            if have > 0 {
                request = request.header("Range", format!("bytes={have}-"));
            }
            let mut response = request.call()?;
            check(&mut response)?;
            // The server may send the whole file instead of the rest.
            let append = response.status() == StatusCode::PARTIAL_CONTENT;
            let mut out = OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(partial)?;
            let start = if append { have } else { 0 };
            let mut body = response
                .body_mut()
                .with_config()
                .limit(file.size - start + 1)
                .reader();
            io::copy(&mut body, &mut out).map_err(|error| Error::Network(error.to_string()))?;
            out.sync_all()?;
        }
        let sha256 = sha256_of(partial)?;
        if sha256 != file.sha256 {
            fs::remove_file(partial)?;
            return Err(Error::Damaged(format!(
                "expected {} bytes hashing to {}, got {sha256}",
                file.size, file.sha256
            )));
        }
        Ok(())
    }
}

fn bearer(secret: &str) -> String {
    format!("Bearer {secret}")
}

/// Turns a failure status into an [`Error`].
fn check(response: &mut Response<ureq::Body>) -> Result<(), Error> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(Error::NotPaired);
    }
    if status == StatusCode::NOT_FOUND {
        return Err(Error::NotFound);
    }
    let message = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_json::<Problem>()
        .map_or_else(|_| status.to_string(), |problem| problem.error);
    Err(Error::Server {
        status: status.as_u16(),
        message,
    })
}

fn json<T: DeserializeOwned>(mut response: Response<ureq::Body>) -> Result<T, Error> {
    check(&mut response)?;
    let status = response.status().as_u16();
    response
        .body_mut()
        .with_config()
        .limit(JSON_LIMIT)
        .read_json()
        .map_err(|error| Error::Server {
            status,
            message: format!("an answer that didn't make sense: {error}"),
        })
}

pub(crate) fn sha256_of(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// Writes `value` as JSON to `path` by way of a temporary file, so a crash
/// or a pulled battery leaves the old file or the new, never half of one.
pub(crate) fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let temporary = path.with_extension("json.new");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

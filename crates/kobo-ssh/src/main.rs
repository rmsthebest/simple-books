//! `simple-books-ssh --dir <app folder> [--port 2222]`
//!
//! Started by the reader when SSH is turned on. It reads the password from
//! the first line of stdin and serves until stdin ends, which happens when
//! the reader stops it, or exits for any reason. See the crate docs.

use std::{io, path::PathBuf};

use kobo_ssh::{Settings, host_key, log, serve};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

const PORT: u16 = 2222;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    let Some(dir) = value("--dir").map(PathBuf::from) else {
        eprintln!("usage: simple-books-ssh --dir <app folder> [--port 2222]");
        std::process::exit(2);
    };
    let port = value("--port").and_then(|p| p.parse().ok()).unwrap_or(PORT);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    if let Err(error) = runtime.block_on(run(dir, port)) {
        log!("stopping: {error}");
        std::process::exit(1);
    }
}

async fn run(dir: PathBuf, port: u16) -> io::Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut password = String::new();
    stdin.read_line(&mut password).await?;
    let password = password.trim().to_owned();
    if password.is_empty() {
        return Err(io::Error::other("no password on stdin"));
    }
    terminals()?;
    let key = host_key(&dir)?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let settings = Settings {
        dir,
        password,
        shell: PathBuf::from("/bin/sh"),
    };
    let stop = async move {
        // Until the reader closes its end.
        let mut rest = Vec::new();
        let _ = stdin.read_to_end(&mut rest).await;
    };
    serve(listener, settings, key, stop).await
}

/// The Kobo's init scripts don't mount `/dev/pts`, which shells with a
/// terminal need.
fn terminals() -> io::Result<()> {
    let pts = std::path::Path::new("/dev/pts");
    if pts.join("ptmx").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(pts)?;
    // SAFETY: the arguments are NUL-terminated strings that outlive the call.
    let mounted = unsafe {
        libc::mount(
            c"devpts".as_ptr(),
            c"/dev/pts".as_ptr(),
            c"devpts".as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if mounted < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

//! One session channel: a shell or a command, with or without a terminal,
//! from the first request to the exit status.

use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use russh::{
    ChannelMsg,
    server::Msg,
    {Channel, ChannelWriteHalf},
};
use tokio::{
    io::{AsyncWriteExt, unix::AsyncFd},
    process::{Child, ChildStdin, Command},
    task::JoinHandle,
};

use crate::{Running, Settings, log};

/// What a shell or command starts with: nothing from the server's own.
const PATH: &str = "/sbin:/bin:/usr/sbin:/usr/bin";
/// How long to wait for the last output once a command has exited.
const DRAIN: Duration = Duration::from_secs(2);

/// A terminal's size, from the client.
#[derive(Debug, Clone, Copy)]
struct Size {
    cols: u32,
    rows: u32,
    width: u32,
    height: u32,
}

impl Size {
    fn winsize(self) -> libc::winsize {
        let clamp = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
        libc::winsize {
            ws_row: clamp(self.rows),
            ws_col: clamp(self.cols),
            ws_xpixel: clamp(self.width),
            ws_ypixel: clamp(self.height),
        }
    }
}

/// Where the client's input goes.
enum Input {
    Pipe(Option<ChildStdin>),
    Terminal(Arc<AsyncFd<OwnedFd>>),
}

/// A running shell or command.
struct Process {
    child: Child,
    group: i32,
    input: Input,
    output: Vec<JoinHandle<()>>,
}

/// Ends a process group, as when its channel closes or the server stops.
pub(crate) fn end_group(group: i32) {
    // SAFETY: kill only sends a signal; a group that's gone is an error.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

pub(crate) async fn run(channel: Channel<Msg>, settings: Arc<Settings>, running: Running) {
    let (mut read, write) = channel.split();
    let write = Arc::new(write);
    let mut terminal: Option<(String, Size)> = None;
    let command = loop {
        match read.wait().await {
            Some(ChannelMsg::RequestPty {
                term,
                col_width,
                row_height,
                pix_width,
                pix_height,
                ..
            }) => {
                let size = Size {
                    cols: col_width,
                    rows: row_height,
                    width: pix_width,
                    height: pix_height,
                };
                terminal = Some((term, size));
            }
            Some(ChannelMsg::RequestShell { .. }) => break None,
            Some(ChannelMsg::Exec { command, .. }) => {
                break Some(String::from_utf8_lossy(&command).into_owned());
            }
            Some(ChannelMsg::Eof | ChannelMsg::Close) | None => return,
            Some(_) => {}
        }
    };
    let started = match &terminal {
        Some((term, size)) => start_on_terminal(&settings, command.as_deref(), term, *size, &write),
        None => start_piped(&settings, command.as_deref(), &write),
    };
    let mut process = match started {
        Ok(process) => process,
        Err(error) => {
            log!("couldn't start {}: {error}", settings.shell.display());
            let _ = write
                .data_bytes(format!("couldn't start a shell: {error}\r\n"))
                .await;
            finish(&write, 127).await;
            return;
        }
    };
    running.add(process.group);
    let code = loop {
        tokio::select! {
            message = read.wait() => match message {
                Some(ChannelMsg::Data { data }) => process.input.write(&data).await,
                Some(ChannelMsg::Eof) => process.input.close(),
                Some(ChannelMsg::WindowChange { col_width, row_height, pix_width, pix_height }) => {
                    process.input.resize(Size {
                        cols: col_width,
                        rows: row_height,
                        width: pix_width,
                        height: pix_height,
                    });
                }
                Some(ChannelMsg::Close) | None => break None,
                Some(_) => {}
            },
            status = process.child.wait() => break Some(exit_code(&status)),
        }
    };
    end_group(process.group);
    running.remove(process.group);
    let Some(code) = code else {
        // Closed by the client: nobody to tell.
        return;
    };
    for output in process.output {
        let _ = tokio::time::timeout(DRAIN, output).await;
    }
    finish(&write, code).await;
}

fn exit_code(status: &io::Result<ExitStatus>) -> u32 {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Ok(status) => status
            .code()
            .or_else(|| status.signal().map(|s| 128 + s))
            .map_or(255, |code| code as u32),
        Err(_) => 255,
    }
}

async fn finish(write: &ChannelWriteHalf<Msg>, code: u32) {
    let _ = write.exit_status(code).await;
    let _ = write.eof().await;
    let _ = write.close().await;
}

fn shell_command(settings: &Settings, command: Option<&str>) -> Command {
    let mut shell = Command::new(&settings.shell);
    match command {
        Some(command) => shell.arg("-c").arg(command),
        None => shell.arg("-l"),
    };
    shell
        .env_clear()
        .env("PATH", PATH)
        .env("HOME", "/")
        .env("USER", "root")
        .env("SHELL", &settings.shell)
        .current_dir("/")
        .kill_on_drop(true);
    shell
}

fn start_piped(
    settings: &Settings,
    command: Option<&str>,
    write: &Arc<ChannelWriteHalf<Msg>>,
) -> io::Result<Process> {
    let mut shell = shell_command(settings, command);
    shell
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = shell.spawn()?;
    let group = child.id().map_or(0, |id| id as i32);
    let mut output = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut to = write.make_writer();
        output.push(tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stdout, &mut to).await;
            let _ = to.shutdown().await;
        }));
    }
    if let Some(mut stderr) = child.stderr.take() {
        let mut to = write.make_writer_ext(Some(1));
        output.push(tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut to).await;
            let _ = to.shutdown().await;
        }));
    }
    Ok(Process {
        input: Input::Pipe(child.stdin.take()),
        child,
        group,
        output,
    })
}

fn start_on_terminal(
    settings: &Settings,
    command: Option<&str>,
    term: &str,
    size: Size,
    write: &Arc<ChannelWriteHalf<Msg>>,
) -> io::Result<Process> {
    let (master, slave) = open_terminal(size)?;
    let mut shell = shell_command(settings, command);
    shell
        .env("TERM", if term.is_empty() { "vt100" } else { term })
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe calls, between fork and exec.
    unsafe {
        shell.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = shell.spawn()?;
    // The shell leads its own session, so its group is its pid.
    let group = child.id().map_or(0, |id| id as i32);
    // SAFETY: an OwnedFd keeps its descriptor open, and the same, until
    // it's dropped with the AsyncFd.
    let master = Arc::new(unsafe { AsyncFd::register(master) }?);
    let reading = master.clone();
    let to = write.clone();
    let output = tokio::spawn(async move {
        let mut buffer = vec![0; 4096];
        // Ends with EIO once the shell and everything it started have exited.
        while let Ok(n) = read_terminal(&reading, &mut buffer).await {
            if n == 0 || to.data_bytes(buffer[..n].to_vec()).await.is_err() {
                break;
            }
        }
    });
    Ok(Process {
        child,
        group,
        input: Input::Terminal(master),
        output: vec![output],
    })
}

/// A new pseudo-terminal: the master, non-blocking, and the slave.
fn open_terminal(size: Size) -> io::Result<(OwnedFd, OwnedFd)> {
    let (mut master, mut slave) = (-1, -1);
    let winsize = size.winsize();
    // SAFETY: openpty writes two descriptors, which are owned from here.
    unsafe {
        if libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &raw const winsize,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
        let (master, slave) = (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave));
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(
            master.as_raw_fd(),
            libc::F_SETFL,
            flags | libc::O_NONBLOCK | libc::O_CLOEXEC,
        );
        libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        Ok((master, slave))
    }
}

async fn read_terminal(fd: &AsyncFd<OwnedFd>, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut guard = fd.readable().await?;
        let result = guard.try_io(|inner| {
            // SAFETY: reads into a buffer of the length given.
            let n =
                unsafe { libc::read(inner.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        });
        if let Ok(result) = result {
            return result;
        }
    }
}

async fn write_terminal(fd: &AsyncFd<OwnedFd>, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        let mut guard = fd.writable().await?;
        let result = guard.try_io(|inner| {
            // SAFETY: writes from a slice of the length given.
            let n = unsafe { libc::write(inner.as_raw_fd(), data.as_ptr().cast(), data.len()) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        });
        if let Ok(result) = result {
            data = &data[result?..];
        }
    }
    Ok(())
}

impl Input {
    async fn write(&mut self, data: &[u8]) {
        match self {
            Input::Pipe(Some(stdin)) => {
                if stdin.write_all(data).await.is_err() {
                    self.close();
                }
            }
            Input::Pipe(None) => {}
            Input::Terminal(master) => {
                let _ = write_terminal(master, data).await;
            }
        }
    }

    /// The client has no more input: a command sees the end of its stdin.
    /// A terminal has no such thing, so it's left open.
    fn close(&mut self) {
        if let Input::Pipe(stdin) = self {
            stdin.take();
        }
    }

    fn resize(&self, size: Size) {
        if let Input::Terminal(master) = self {
            let winsize = size.winsize();
            // SAFETY: TIOCSWINSZ reads a winsize.
            unsafe {
                libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const winsize);
            }
        }
    }
}

//! Command-line entry point.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::{
    ffi::OsString, net::SocketAddr, path::PathBuf, process::ExitCode, sync::Arc, time::Duration,
};

use clap::{Parser, Subcommand};
use leptos::config::LeptosOptions;
use simple_books::server::{
    AppState,
    auth::{accounts, first_run::FirstRun},
    config::Config,
    convert::{self, Tools, limited, reader},
    db::Db,
    hardcover::Hardcover,
    http::limit::Limits,
    kobo::resources::Resources,
    library::upload::UploadGate,
    now,
    reader::install::KoboFiles,
    router,
    storage::{files::create_private_dir, layout::Layout, reconcile},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};
use tracing_subscriber::EnvFilter;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Parser)]
#[command(name = "simple-books", about = "A minimal self-hosted ebook server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Runs the server. This is the default.
    Serve,
    /// Probes the running server, for the container health check.
    Healthcheck,
    /// Creates users and their setup links.
    #[command(subcommand)]
    Admin(AdminCommand),
    /// Applies limits to itself and becomes another program. Internal.
    #[command(hide = true)]
    ExecLimited {
        #[arg(long)]
        program: PathBuf,
        #[arg(long)]
        work: PathBuf,
        #[arg(long)]
        read: Vec<PathBuf>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Converts one EPUB to the reader's format, inside limits. Internal.
    #[command(hide = true)]
    ConvertBook {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Creates a user and prints a one-time link for them to set a password.
    CreateUser {
        username: String,
        /// Lets them manage users and shared books.
        #[arg(long)]
        admin: bool,
        /// Storage limit in MiB. Defaults to `DEFAULT_QUOTA_MB`.
        #[arg(long)]
        quota_mb: Option<u64>,
    },
    /// Prints a new setup link for a user, for a forgotten password.
    SetupLink { username: String },
}

fn main() -> ExitCode {
    // Everything this process creates is private, the database included.
    rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o077));
    let cli = Cli::parse();

    // The internal commands come first: they run with a cleared environment
    // and need no settings, and before any runtime threads exist, since they
    // limit the process they're in.
    match &cli.command {
        Some(Command::ExecLimited {
            program,
            work,
            read,
            args,
        }) => {
            let error = limited::exec(&limited::Limits {
                program: program.clone(),
                read: read.clone(),
                work: work.clone(),
                args: args.clone(),
            });
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
        Some(Command::ConvertBook { input, output }) => {
            let job = reader::ConvertBook {
                input: input.clone(),
                output: output.clone(),
            };
            return match reader::convert_book(&job) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        _ => {}
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode, AnyError> {
    let config = Config::from_env()?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            serve(config).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Healthcheck => healthcheck(&config).await,
        Command::ExecLimited { .. } | Command::ConvertBook { .. } => {
            unreachable!("handled in main")
        }
        Command::Admin(command) => {
            run_admin(&config, command).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn open_database(config: &Config) -> Result<(Db, Layout), AnyError> {
    let layout = Layout::new(&config.data_dir);
    create_private_dir(layout.root()).await?;
    let db = Db::open(&layout.database()).await?;
    Ok((db, layout))
}

async fn serve(config: Config) -> Result<(), AnyError> {
    let filter = EnvFilter::try_new(format!("{level},sqlx=warn", level = config.log_level))?;
    tracing_subscriber::fmt().with_env_filter(filter).init();

    for warning in &config.warnings {
        tracing::warn!("{warning}");
    }

    let (db, layout) = open_database(&config).await?;
    let listen_addr = config.listen_addr;

    // With nobody to sign in, the first admin comes from a link only this log shows.
    let first_run = Arc::new(FirstRun::default());
    let first_link = if db.has_users().await? {
        None
    } else {
        let token = first_run.issue(now());
        Some(format!("{}/setup/{token}", config.public_url))
    };
    tokio::spawn(clear_expired_credentials(db.clone()));

    let report = reconcile::run(&layout, &db).await?;
    tracing::info!(?report, "storage checked");
    let tools = Tools::new(config.kepubify_path.clone(), std::env::current_exe()?);

    let leptos = LeptosOptions::builder()
        .output_name("simple-books")
        .site_root(config.site_root.as_str())
        .site_pkg_dir("pkg")
        .site_addr(listen_addr)
        .build();

    let hardcover = match config.hardcover_client_id.as_deref() {
        Some(client_id) => Some(Arc::new(Hardcover::new(&config.hardcover_api, client_id)?)),
        None => None,
    };
    let kobo_files = config.kobo_files.clone();
    let state = AppState {
        config: Arc::new(config),
        db,
        layout,
        kobo_resources: Arc::new(Resources::load()?),
        leptos,
        conversions: Arc::new(Notify::new()),
        uploads: Arc::new(UploadGate::default()),
        limits: Arc::new(Limits::default()),
        hardcover,
        first_run,
        kobo_files: Arc::new(KoboFiles::new(kobo_files)),
    };
    tokio::spawn(convert::run(state.clone(), tools));
    if let Some(hardcover) = state.hardcover.clone() {
        tracing::info!("Hardcover is offered");
        tokio::spawn(simple_books::server::hardcover::worker::run(
            state.db.clone(),
            hardcover,
        ));
    }

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    if let Some(link) = first_link {
        // Printed, not logged, so it never passes through the log formatter.
        println!(
            "No users yet. Open this link to choose the admin's username and password. \
             It works once, within 72 hours, and restarting prints a new one:"
        );
        println!("{link}");
    }
    tracing::info!(%listen_addr, "listening");
    let app = router::build(state).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Expired sessions and links are already refused; this only tidies them away.
async fn clear_expired_credentials(db: Db) {
    let mut hourly = tokio::time::interval(Duration::from_hours(1));
    loop {
        hourly.tick().await;
        if let Err(error) = db.delete_expired_credentials().await {
            tracing::warn!(%error, "couldn't clear expired credentials");
        }
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(error) => {
            tracing::warn!(%error, "couldn't listen for SIGTERM");
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// A distroless image has no curl, so the binary probes itself.
async fn healthcheck(config: &Config) -> Result<ExitCode, AnyError> {
    let port = config.listen_addr.port();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    Ok(if response.starts_with("HTTP/1.1 200") {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn run_admin(config: &Config, command: AdminCommand) -> Result<(), AnyError> {
    let (db, _) = open_database(config).await?;
    let (username, link) = match command {
        AdminCommand::CreateUser {
            username,
            admin,
            quota_mb,
        } => {
            let quota = match quota_mb {
                Some(mb) => i64::try_from(mb.saturating_mul(1024 * 1024))?,
                None => i64::try_from(config.default_quota_bytes)?,
            };
            let id = accounts::create_user(&db, &username, quota, admin).await?;
            let link = accounts::setup_link(&db, &config.public_url, id).await?;
            (username, link)
        }
        AdminCommand::SetupLink { username } => {
            let link = accounts::setup_link_for(&db, &config.public_url, &username).await?;
            (username, link)
        }
    };
    println!("Send this to {username}. It works once, within 72 hours:");
    println!("{link}");
    Ok(())
}

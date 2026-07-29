mod app;
mod auth;
mod bg;
mod brief;
mod event;
mod handler;
mod ui;
mod util;
mod zyris_client;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use rustls::crypto::aws_lc_rs;
use tokio::sync::mpsc;
use zyris::runtime::{Credentials, RunConfig, RunError, Runner};
use zyris::NodeKind;
use zyris_caps::{FileIoServer, LocalFileIo, PtyTerminal, TerminalServer};

use app::{App, BgEvent, BgTx};
use zyris_client::{ApiSlot, DEFAULT_SCOPES};

/// Long enough for the closing frame to reach the server, so the node card flips offline promptly
/// instead of waiting for the heartbeat to lapse.
const CLOSE_GRACE: Duration = Duration::from_millis(200);

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    aws_lc_rs::default_provider()
        .install_default()
        .unwrap_or_else(|e| {
            eprintln!("attacca: failed to install AWS-LC crypto provider: {e:#?}");
            std::process::exit(2);
        });

    init_logging();

    let mut config = RunConfig::from_env();
    config.kind = NodeKind::Cli;
    // `RunConfig::scopes_pinned` is private and only `Runner::request_scopes` consults it, so an
    // operator's `$ZYRIS_SCOPES` has to be honoured by hand here. Assigning unconditionally would
    // make that variable a lie.
    if std::env::var_os("ZYRIS_SCOPES").is_none() {
        config.scopes = DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect();
    }

    warn_about_a_stale_api_key();

    let credentials = match zyris::runtime::credentials::from_env(&config) {
        Ok(credentials) => credentials,
        Err(e) => {
            eprintln!("attacca: {e}");
            return ExitCode::from(2);
        }
    };
    // Wrapped so `/login` can replace the source underneath the runner, which otherwise holds the
    // credential it resolved at startup for the life of the process.
    let credentials = Arc::new(auth::SwappableCredentials::new(credentials));
    let authenticator = Arc::new(auth::Authenticator::new(
        credentials.clone(),
        config.url.clone(),
        config.profile.clone(),
        config.node_name.clone(),
        config.platform().to_string(),
        config.scopes.clone(),
    ));

    println!("attacca — {} as {}", config.url, config.node_name);
    // Enrollment happens here, deliberately before the alternate screen. The device grant prints
    // its code with `println!` rather than `tracing` so it survives `RUST_LOG=error`, and inside a
    // TUI that output would be invisible.
    if let Err(e) = credentials.bearer().await {
        eprintln!("attacca: {e}");
        return ExitCode::from(2);
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let slot = ApiSlot::new();

    let file_root = file_root();
    let with_terminal = std::env::var_os("ATTACCA_NO_TERMINAL").is_none();
    // Built from the same two decisions that configure the capabilities below, so the agent is
    // never told about a capability this node does not actually serve.
    let node_brief = brief::NodeBrief {
        node_name: config.node_name.clone(),
        file_root: file_root.display().to_string(),
        terminal: with_terminal,
    };

    let mut runner =
        Runner::new(config, credentials).capability(FileIoServer(LocalFileIo::rooted(&file_root)));
    // `terminal` runs arbitrary commands as this user with no per-call confirmation. This is the
    // way to serve the file half alone.
    //
    // Rooted at the same directory as `file_io`, not left to `PtyTerminal::default`'s own
    // `current_dir`: the two agree by construction then, so `ATTACCA_FILE_ROOT` cannot leave the
    // shell sitting somewhere other than where relative file paths resolve.
    if with_terminal {
        runner = runner.capability(TerminalServer(PtyTerminal::rooted(&file_root)));
    }
    let runner = runner.on_connect({
        let tx = tx.clone();
        let slot = slot.clone();
        move |conn| {
            let tx = tx.clone();
            let slot = slot.clone();
            async move { zyris_client::on_connect(conn, tx, slot).await }
        }
    });

    // Not a bare `tokio::spawn(runner.try_run())`. A node that permanently gives up has to say so:
    // otherwise the TUI sits on "offline" forever with no explanation and the process exits 0,
    // which is a lie to whatever is supervising it.
    //
    // Ctrl+C is not double-handled despite `serve_until_closed` selecting on `ctrl_c()`. Raw mode
    // clears ISIG, so 0x03 arrives as a key event and no SIGINT is ever delivered; that arm never
    // fires and `/exit` and Ctrl+C both land on the `abort()` below.
    let node = tokio::spawn(supervise(runner, tx.clone()));

    let mut app = App::new(tx, rx, slot.clone(), authenticator, node_brief);
    event::run(&mut app).await;

    // Raw mode is already restored by now, so closing here costs nothing visible.
    if let Some(live) = slot.get() {
        live.conn.close("attacca-cli exiting");
        tokio::time::sleep(CLOSE_GRACE).await;
    }
    // Every path out of `event::run` — including its three setup failures — reaches this. Skipping
    // it would leave the process alive on a websocket with no UI attached.
    node.abort();

    match app.node_stopped.take() {
        Some(message) => {
            eprintln!("attacca: {message}");
            node_exit_code(&app)
        }
        None => ExitCode::SUCCESS,
    }
}

/// Run the node, and report a terminal failure to the UI instead of dying silently.
async fn supervise(runner: Runner, tx: BgTx) {
    if let Err(e) = runner.try_run().await {
        let _ = tx.send(BgEvent::NodeStopped {
            message: e.to_string(),
            needs_operator: needs_operator(&e),
        });
    }
}

/// Whether a person has to intervene, mirroring `RunError::exit_code`'s split. Matched on the
/// variants rather than comparing `ExitCode`, which is not `PartialEq`.
fn needs_operator(e: &RunError) -> bool {
    matches!(
        e,
        RunError::Credentials(zyris::runtime::CredentialsError::NeedsOperator(_))
            | RunError::Build(_)
    )
}

fn node_exit_code(app: &App) -> ExitCode {
    if app.node_needs_operator {
        ExitCode::from(2)
    } else {
        ExitCode::from(1)
    }
}

/// `ATTACCA_API_KEY` is no longer read by anything. zyris already diagnoses an `atk_` key pasted
/// into `ZYRIS_NODE_TOKEN`; this covers the leftover-`.env` half, where the variable is simply
/// ignored and the user is left wondering why they are being asked to enroll.
fn warn_about_a_stale_api_key() {
    let has_key = std::env::var_os("ATTACCA_API_KEY").is_some_and(|v| !v.is_empty());
    let has_token = std::env::var_os("ZYRIS_NODE_TOKEN").is_some()
        || std::env::var_os("ZYRIS_NODE_TOKEN_FILE").is_some();
    if has_key && !has_token {
        eprintln!(
            "attacca: ATTACCA_API_KEY is no longer used — this client authenticates as a Zyris node"
        );
    }
}

/// The working directory both `file_io` and `terminal` are rooted at.
///
/// Not a jail. `zyris-caps` resolves paths with a shared `resolve_under`, which honours an absolute
/// path as given and lets `..` climb out, so this decides where *relative* paths land and nothing
/// more. Anything the user account can reach is reachable through these capabilities.
fn file_root() -> PathBuf {
    std::env::var_os("ATTACCA_FILE_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Logs go to a file or nowhere at all.
///
/// With no subscriber installed, zyris's `tracing` calls are no-ops — which is what keeps them from
/// scribbling over the TUI. Connection state reaches the user as chat notices instead.
fn init_logging() {
    let Some(raw) = std::env::var_os("ATTACCA_LOG") else {
        return;
    };
    let path = if raw.is_empty() {
        PathBuf::from("attacca-cli.log")
    } else {
        PathBuf::from(raw)
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "attacca=info,zyris=info".into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Arc::new(file))
        .init();
}

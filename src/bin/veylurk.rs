use clap::Parser;
use std::{path::PathBuf, process::ExitCode};
use tokio::sync::watch;
use veylurk_probe::{
    app::{self, AppState},
    collector, metadata,
    store::Store,
};

#[derive(Parser)]
#[command(
    name = "veylurk",
    about = "Local Twitch chat observation history; no video-viewing or completeness claims"
)]
struct Args {
    /// SQLite database; history is retained until the file is explicitly removed.
    #[arg(long, default_value = "veylurk-data/observations.sqlite3")]
    database: PathBuf,
    /// Loopback HTTP port. Only 127.0.0.1 is bound.
    #[arg(long,default_value_t=8794,value_parser=clap::value_parser!(u16).range(1..))]
    port: u16,
    /// Add and enable a channel at startup; saved configuration is otherwise reused.
    #[arg(short,long,action=clap::ArgAction::Append)]
    channel: Vec<String>,
    /// Stop gracefully after this duration; omitted for continuous operation.
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..=3600))]
    run_for_seconds: Option<u64>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Veylurk failed: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.port))
        .await
        .map_err(|_| "loopback port is unavailable")?;
    let store = Store::open(&args.database)
        .await
        .map_err(|e| e.to_string())?;
    if !args.channel.is_empty() {
        store
            .track(args.channel, true)
            .await
            .map_err(|e| e.to_string())?;
    }
    let (channel_tx, channel_rx) =
        watch::channel(store.enabled().await.map_err(|e| e.to_string())?);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let status = collector::status(&store);
    let metadata = metadata::initialize(store.clone())
        .await
        .map_err(|e| e.to_string())?;
    let router = app::router(
        AppState {
            store: store.clone(),
            status: status.clone(),
            channels: channel_tx,
            port: args.port,
            configuration: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        },
        metadata.router(),
    );
    let collector_task = tokio::spawn(collector::run(
        store.clone(),
        channel_rx.clone(),
        shutdown_rx.clone(),
        status,
    ));
    let metadata_task = tokio::spawn(metadata.run(channel_rx, shutdown_rx));
    println!(
        "Veylurk is available at http://127.0.0.1:{} . Press Ctrl+C to stop.",
        args.port
    );
    let shutdown = shutdown_tx.clone();
    let duration = args.run_for_seconds;
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _=tokio::signal::ctrl_c()=>{},
                _=async {match duration {Some(seconds)=>tokio::time::sleep(std::time::Duration::from_secs(seconds)).await,None=>std::future::pending::<()>().await}}=>{},
            }
            let _ = shutdown.send(true);
        })
        .await;
    let _ = shutdown_tx.send(true);
    collector_task.await.map_err(|_| "collector task failed")?;
    metadata_task.await.map_err(|_| "metadata task failed")?;
    store.checkpoint().await.map_err(|e| e.to_string())?;
    server.map_err(|_| "local HTTP server failed".to_owned())
}

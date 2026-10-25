use clap::Parser;
use std::{path::PathBuf, process::ExitCode};
use tokio::sync::watch;
use veylurk_probe::{
    app::{self, AppState},
    collector,
    diagnostics::Diagnostics,
    metadata,
    runtime::Runtime,
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
    let diagnostics = Diagnostics::new(&args.database);
    let result = run(args, diagnostics.clone()).await;
    // Never join the writer indefinitely on a failing filesystem. Persistence status distinguishes
    // acknowledged writes from pending or failed attempts while the application is running.
    let _ = diagnostics.flush(std::time::Duration::from_millis(500));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Veylurk failed: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args, diagnostics: Diagnostics) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.port))
        .await
        .map_err(|_| "loopback port is unavailable")?;
    let store = Store::open_with_diagnostics(&args.database, diagnostics.clone())
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
    let (selected_tx, selected_rx) = watch::channel(Vec::new());
    let status = collector::status(&store);
    let metadata = metadata::initialize(store.clone())
        .await
        .map_err(|e| e.to_string())?;
    let runtime = Runtime::initialize(store.clone(), shutdown_tx.clone(), args.port)
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
        metadata.router().merge(runtime.router()),
    );
    let collector_store = store.clone();
    let collector_shutdown = shutdown_rx.clone();
    let collector_stop = shutdown_tx.clone();
    let collector_task = tokio::spawn(async move {
        collector::run(
            collector_store,
            selected_rx,
            collector_shutdown,
            status.clone(),
        )
        .await;
        if status.read().await.state == "storage_error" {
            // A stopped collector must not leave a healthy-looking unattended process.
            // Graceful process failure lets the opt-in scheduler apply bounded retries.
            collector_stop.send_replace(true);
            Err("Collection stopped after a storage failure; uncommitted observations may be missing.")
        } else {
            Ok(())
        }
    });
    let policy_task = tokio::spawn(runtime.run_policy(
        metadata.clone(),
        channel_rx.clone(),
        selected_tx,
        shutdown_rx.clone(),
    ));
    let metadata_task = tokio::spawn(metadata.run(channel_rx, shutdown_rx.clone()));
    println!(
        "Veylurk is available at http://127.0.0.1:{} . Press Ctrl+C to stop.",
        args.port
    );
    let shutdown = shutdown_tx.clone();
    let duration = args.run_for_seconds;
    let api_shutdown = shutdown_rx;
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            veylurk_probe::shutdown::wait(
                tokio::signal::ctrl_c(),
                api_shutdown,
                async {
                    match duration {
                        Some(seconds) => {
                            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await
                        }
                        None => std::future::pending::<()>().await,
                    }
                },
                diagnostics,
            )
            .await;
            let _ = shutdown.send(true);
        })
        .await;
    let _ = shutdown_tx.send(true);
    let collector_result = collector_task.await.map_err(|_| "collector task failed")?;
    metadata_task.await.map_err(|_| "metadata task failed")?;
    policy_task
        .await
        .map_err(|_| "collection policy task failed")?;
    store.checkpoint().await.map_err(|e| e.to_string())?;
    collector_result?;
    server.map_err(|_| "local HTTP server failed".to_owned())
}

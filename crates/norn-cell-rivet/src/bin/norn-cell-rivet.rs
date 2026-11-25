//! `norn-cell-rivet`: a runner process. It serves the cell actors (and, in
//! conformance builds, the probe actor) to the Rivet engine named by
//! `RIVET_ENDPOINT` (`https://<namespace>:<token>@host:6420`; plaintext only to
//! loopback or with `NORN_RIVET_PLAINTEXT_ENGINE=single-host`), and the public
//! router on `NORN_LISTEN` or the platform's `PORT`. Any runner routes to
//! any cell: requests reach the actor through the engine's gateway.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use norn_cell_rivet::config::{self, HostConfig};
use norn_cell_rivet::router::{Router, app, listener as nodelay};
use rivetkit::client::{Client, ClientConfig};
use tokio_util::sync::CancellationToken;

/// `RivetKit` logs every `SQLite` statement's profile at `info`; the host keeps
/// its own `info` and `RivetKit`'s warnings unless `RUST_LOG` says otherwise.
const DEFAULT_LOG: &str =
    "info,rivetkit_core=warn,rivetkit_sqlite_profile=warn,rivet_envoy_client=warn,rivetkit=warn";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_unset| DEFAULT_LOG.into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let config = config::install(HostConfig::from_env());
    let endpoint = std::env::var("RIVET_ENDPOINT").context("RIVET_ENDPOINT names the engine")?;
    config::check_engine_transport(
        &endpoint,
        std::env::var("NORN_RIVET_PLAINTEXT_ENGINE").ok().as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let listen = std::env::var("NORN_LISTEN").unwrap_or_else(|_unset| {
        std::env::var("PORT").map_or_else(
            |_unset| "127.0.0.1:8787".to_owned(),
            |port| format!("0.0.0.0:{port}"),
        )
    });
    let mut runner_config = rivetkit::ServeConfig::from_env()?;
    // The runner joins the named engine and never starts or downloads one.
    runner_config.engine_spawn = rivetkit::EngineSpawnMode::Never;
    runner_config.engine_auto_download = false;
    let shutdown = CancellationToken::new();
    let runner = tokio::spawn(serve_actors(runner_config, shutdown.clone()));
    let client = Client::new(ClientConfig::new(endpoint));
    let router = Arc::new(Router::new(client, Arc::clone(&config))?);
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("listen on {listen}"))?;
    tracing::info!(listen = %listen, conformance = config.conformance, "norn-cell-rivet serving");
    let routing = axum::serve(nodelay(listener), app(router)).with_graceful_shutdown({
        let shutdown = shutdown.clone();
        async move { shutdown.cancelled().await }
    });
    tokio::select! {
        served = routing => served.context("the router stopped")?,
        ran = runner => ran.context("the runner panicked")?.context("the runner stopped")?,
        _signal = tokio::signal::ctrl_c() => {}
    }
    shutdown.cancel();
    Ok(())
}

/// Serve the actors until shutdown. A runner that cannot join the engine (the
/// engine restarting, its storage still settling after a restart, the load
/// balancer between instances) retries with backoff instead of exiting: the
/// cells it hosts come back as soon as the control plane does.
async fn serve_actors(config: rivetkit::ServeConfig, shutdown: CancellationToken) -> Result<()> {
    const FIRST: Duration = Duration::from_secs(1);
    const LONGEST: Duration = Duration::from_secs(30);
    let mut backoff = FIRST;
    loop {
        let started = Instant::now();
        let served = norn_cell_rivet::registry()
            .serve_with_config(config.clone(), shutdown.clone())
            .await;
        if shutdown.is_cancelled() {
            return served;
        }
        match served {
            Ok(()) => {
                tracing::warn!("the engine connection ended; rejoining");
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), retry_in = ?backoff, "joining the engine failed; retrying");
            }
        }
        // A connection that lasted resets the backoff.
        if started.elapsed() > LONGEST.saturating_mul(2) {
            backoff = FIRST;
        }
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            () = shutdown.cancelled() => return Ok(()),
        }
        backoff = backoff.saturating_mul(2).min(LONGEST);
    }
}

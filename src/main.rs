mod chrome;
mod config;
mod file_change_watcher;
mod web;

use chrome::controller::chrome_controller;
use chrome::supervisor::chrome_supervisor;
use web::start_web_server;

use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Context;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc as TokioMpsc;
use tokio::sync::watch;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging()?;

    let config = config::load_config().with_context(|| "Failed to load config".to_string())?;
    let chrome_config_watcher = chrome::config::Watcher::new(
        Arc::new(file_change_watcher::watcher(
            std::time::Duration::from_secs(2),
        )?),
        config.data_base_folder.join("chrome-config"),
    );

    let chrome_config_rx = chrome_config_watcher.channel();

    let (webserver_socket_addr, webserver_handle) = start_web_server(Arc::clone(&config))?;

    let (chrome_info_tx, chrome_info_rx) = watch::channel(None);
    let (chrome_kill_tx, chrome_kill_rx) = TokioMpsc::unbounded_channel();

    let chrome_supervisor_handle = tokio::spawn(chrome_supervisor(
        Arc::clone(&config),
        chrome_info_tx,
        chrome_kill_rx,
    ));

    let chrome_controller_handle = tokio::spawn(chrome_controller(
        Arc::clone(&config),
        chrome_info_rx.clone(),
        chrome_kill_tx.clone(),
        webserver_socket_addr,
        chrome_config_rx,
    ));

    // register signal handlers
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigquit = signal(SignalKind::quit())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    let _ = tokio::select! {
        _ = chrome_supervisor_handle => Err(anyhow!("chrome supervisor finished early")),
        _ = webserver_handle => Err(anyhow!("webserver finished early")),
        _ = chrome_config_watcher.run() => Err(anyhow!("chrome config watcher finished early")),
        _ = chrome_controller_handle => Err(anyhow!("chrome controller finished early")),
        _ = sigterm.recv() => {
            info!("Received SIGTERM");
            Ok(())
        },
        _ = sigquit.recv() => {
            info!("Received SIGQUIT");
            Ok(())
        },
        _ = sigint.recv() => {
            info!("Received SIGINT");
            Ok(())
        },
    }?;

    Ok(())
}

fn init_logging() -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("trace"));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let subscriber = Registry::default().with(env_filter).with(fmt_layer);

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");

    Ok(())
}

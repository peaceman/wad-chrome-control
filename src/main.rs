mod chrome;
mod config;
mod file_change_watcher;
mod web;

use chrome::controller::chrome_controller;
use chrome::supervisor::chrome_supervisor;
use web::start_web_server;

use std::sync::Arc;

use anyhow::Context;
use anyhow::anyhow;
use tokio::sync::mpsc as TokioMpsc;
use tokio::sync::watch;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    let err = tokio::select! {
        _ = chrome_supervisor_handle => anyhow!("chrome supervisor finished early"),
        _ = webserver_handle => anyhow!("webserver finished early"),
        _ = chrome_config_watcher.run() => anyhow!("chrome config watcher finished early"),
        _ = chrome_controller_handle => anyhow!("chrome controller finished early"),
    };

    Err(err.into())
}

fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("trace"));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let subscriber = Registry::default().with(env_filter).with(fmt_layer);

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");

    Ok(())
}

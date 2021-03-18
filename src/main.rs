mod chrome_controller;
mod chrome_supervisor;
mod config;
mod web;
mod file_change_watcher;

use file_change_watcher::Watcher;
use tokio::sync::mpsc as TokioMpsc;
use tokio::sync::watch;

use crate::web::start_web_server;
use anyhow::Context;
use chrome_controller::chrome_controller;
use chrome_supervisor::chrome_supervisor;
use std::sync::Arc;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging()?;

    let config = config::load_config().with_context(|| "Failed to load config".to_string())?;
    let watcher = Arc::new(file_change_watcher::watcher(std::time::Duration::from_secs(1))?);

    let (webserver_socket_addr, webserver_fut) = start_web_server(Arc::clone(&config))?;

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
        Arc::clone(&watcher),
    ));

    tokio::join!(chrome_supervisor_handle, webserver_fut);

    Ok(())
}

fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("trace"));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let subscriber = Registry::default().with(env_filter).with(fmt_layer);

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");

    Ok(())
}

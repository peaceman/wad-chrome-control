use crate::{
    chrome::{config::ChromeConfig, supervisor::ChromeInfo},
    config::AppConfig,
};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc as StdMpsc;
use std::time::{Duration, Instant};

use anyhow::Context;
use headless_chrome::Browser;
use tokio::sync::mpsc as TokioMpsc;
use tokio::sync::watch;
use tracing::{error, info, trace, warn};
use url::Url;

pub async fn chrome_controller(
    config: AppConfig,
    mut chrome_info_rx: watch::Receiver<Option<ChromeInfo>>,
    chrome_kill_tx: TokioMpsc::UnboundedSender<ChromeInfo>,
    webserver_socket_addr: SocketAddr,
    mut chrome_config_rx: watch::Receiver<Option<ChromeConfig>>,
) -> anyhow::Result<()> {
    let webserver_url = Url::parse(format!("http://{}", webserver_socket_addr).as_ref())?;

    loop {
        // loop until we receive a chrome info
        info!("Waiting for chrome info");
        chrome_info_rx.changed().await?;
        let chrome_info = match *chrome_info_rx.borrow() {
            Some(v) => v,
            None => continue,
        };

        info!("Received chrome info {:?}", chrome_info);

        // determine chrome debugging websocket url
        let chrome_base_url = format!("http://localhost:{}", chrome_info.get_debugging_port());
        let chrome_base_url = Url::parse(chrome_base_url.as_str())?;

        let chrome_websocket_url = determine_chrome_websocket_url(chrome_base_url).await;
        let chrome_websocket_url = match chrome_websocket_url {
            Ok(url) => url,
            Err(e) => {
                warn!(
                    "Failed to fetch chrome websocket url; killing chrome {:?}",
                    e
                );
                chrome_kill_tx.send(chrome_info)?;
                continue;
            }
        };

        info!("Spawning chrome driver thread");
        let (inner_chrome_kill_tx, mut inner_chrome_kill_rx) = TokioMpsc::unbounded_channel();
        let (chrome_driver_control_tx, chrome_driver_control_recv) = StdMpsc::channel();
        let _chrome_driver_thread_handle = spawn_chrome_driver_thread(
            inner_chrome_kill_tx,
            chrome_driver_control_recv,
            chrome_websocket_url.clone(),
        );

        let mut heartbeat_interval = tokio::time::interval_at(
            (Instant::now() + Duration::from_secs(2)).into(),
            Duration::from_secs(2),
        );

        if send_chrome_config_update(&chrome_config_rx, &chrome_driver_control_tx, &webserver_url)
            .is_err()
        {
            chrome_kill_tx.send(chrome_info)?;
            continue;
        }

        loop {
            tokio::select! {
                chrome_config = chrome_config_rx.changed() => {
                    info!("Chrome config changed: {:?}", chrome_config);

                    if send_chrome_config_update(&chrome_config_rx, &chrome_driver_control_tx, &webserver_url).is_err() {
                        chrome_kill_tx.send(chrome_info)?;
                        break;
                    }
                }
                _ = heartbeat_interval.tick() => {
                    let control_tx = chrome_driver_control_tx.clone();
                    if send_chrome_driver_heartbeat(control_tx, Duration::from_secs(5)).await.is_err() {
                        chrome_kill_tx.send(chrome_info)?;
                        break;
                    }
                }
                Some(_) = inner_chrome_kill_rx.recv() => {
                    info!("Received chrome kill message, re-dispatching");
                    chrome_kill_tx.send(chrome_info)?;
                    break;
                }
            }
        }

        info!("Finished inner chrome controller loop");
    }
}

fn send_chrome_config_update(
    chrome_config_rx: &watch::Receiver<Option<ChromeConfig>>,
    chrome_driver_control_tx: &StdMpsc::Sender<ChromeControlMessage>,
    webserver_url: &Url,
) -> anyhow::Result<()> {
    match *chrome_config_rx.borrow() {
        Some(ref chrome_config) => {
            chrome_driver_control_tx.send(ChromeControlMessage::ConfigUpdate {
                config: chrome_config.clone(),
            })?
        }
        None => chrome_driver_control_tx.send(ChromeControlMessage::ConfigUpdate {
            config: ChromeConfig {
                url: webserver_url.clone(),
            },
        })?,
    }

    Ok(())
}

async fn send_chrome_driver_heartbeat(
    chrome_driver_control_tx: StdMpsc::Sender<ChromeControlMessage>,
    heartbeat_timeout: Duration,
) -> anyhow::Result<()> {
    trace!("Send heartbeat");
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    chrome_driver_control_tx.send(ChromeControlMessage::Heartbeat { response_tx })?;

    match tokio::time::timeout(heartbeat_timeout, response_rx).await {
        Ok(result) => match result {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!("Received heartbeat failure {:?}", e);
                Err(e.into())
            }
        },
        Err(e) => {
            warn!(
                "Did not receive heartbeat response within {:?}",
                heartbeat_timeout
            );
            Err(e.into())
        }
    }
}

#[derive(thiserror::Error, Debug)]
enum DriveChromeError {
    #[error("Failed to connect to chrome")]
    ConnectionFailure(failure::Error),
    #[error("Chrome was unresponsive")]
    Unresponsive(failure::Error),
    #[error(transparent)]
    Unknown(#[from] anyhow::Error),
}

impl From<failure::Error> for DriveChromeError {
    fn from(source: failure::Error) -> Self {
        anyhow::Error::new(source.compat()).into()
    }
}

#[derive(Debug)]
enum ChromeControlMessage {
    ConfigUpdate {
        config: ChromeConfig,
    },
    Heartbeat {
        response_tx: tokio::sync::oneshot::Sender<()>,
    },
    Shutdown,
}

fn spawn_chrome_driver_thread(
    chrome_kill_tx: TokioMpsc::UnboundedSender<()>,
    driver_control_recv: StdMpsc::Receiver<ChromeControlMessage>,
    chrome_websocket_url: Url,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn({
        move || {
            chrome_kill_on_error(chrome_kill_tx, || {
                chrome_driver(chrome_websocket_url, driver_control_recv)
            })
        }
    })
}

fn chrome_kill_on_error(
    chrome_kill_tx: TokioMpsc::UnboundedSender<()>,
    f: impl FnOnce() -> Result<(), DriveChromeError>,
) {
    if let Err(e) = f() {
        error!("Received error during chrome drive {:?}", e);

        if matches!(
            e,
            DriveChromeError::ConnectionFailure { .. } | DriveChromeError::Unresponsive { .. }
        ) {
            info!("Send chrome kill signal");
            if chrome_kill_tx.send(()).is_err() {
                trace!("The chrome kill signal receiver was already dropped");
            }
        }
    }
}

#[tracing::instrument(fields(%chrome_websocket_url), skip(control_recv))]
fn chrome_driver(
    chrome_websocket_url: Url,
    control_recv: StdMpsc::Receiver<ChromeControlMessage>,
) -> Result<(), DriveChromeError> {
    info!("Establish connection to chrome");
    let browser = Browser::connect(chrome_websocket_url.into_string())
        .map_err(DriveChromeError::ConnectionFailure)?;

    info!("Wait for the initial tab");
    let tab = browser
        .wait_for_initial_tab()
        .map_err(DriveChromeError::Unresponsive)?;

    loop {
        let control_msg = match control_recv.recv() {
            Ok(msg) => msg,
            Err(_) => {
                info!("Control channel has hung up");
                break;
            }
        };

        match control_msg {
            ChromeControlMessage::ConfigUpdate { config } => {
                info!("Received config update {:?}", config);
                tab.navigate_to(config.url.as_str())
                    .map_err(DriveChromeError::Unresponsive)?;
                tab.wait_until_navigated()
                    .map_err(DriveChromeError::Unresponsive)?;
            }
            ChromeControlMessage::Heartbeat { response_tx } => {
                tab.get_bounds().map_err(DriveChromeError::Unresponsive)?;
                trace!("Handled heartbeat");
                if response_tx.send(()).is_err() {
                    trace!("The heartbeat receiver was already dropped");
                }
            }
            ChromeControlMessage::Shutdown => {
                info!("Received shutdown control message");
                break;
            }
        }
    }

    info!("Exiting chrome driver loop");
    Ok(())
}

async fn determine_chrome_websocket_url(chrome_base_url: Url) -> anyhow::Result<Url> {
    let timeout = Duration::from_secs(10);

    tokio::time::timeout(timeout, async move {
        loop {
            match fetch_chrome_websocket_url(&chrome_base_url).await {
                Ok(url) => break url,
                Err(e) => {
                    trace!("Failed to fetch chrome websocket url; try again {:?}", e);
                    continue;
                }
            }
        }
    })
    .await
    .with_context(|| {
        format!(
            "Failed to determine the chrome websocket url in {:?}",
            timeout
        )
    })
}

async fn fetch_chrome_websocket_url(base_url: &Url) -> anyhow::Result<Url> {
    let fetch_websocket_url = base_url.join("/json/version")?;
    let response = reqwest::get(fetch_websocket_url.as_ref())
        .await?
        .json::<HashMap<String, String>>()
        .await?;

    let url = response
        .get("webSocketDebuggerUrl")
        .with_context(|| "Missing webSocketDebuggerUrl in chrome remote debugging response")?;

    let url = Url::parse(url.as_ref())
        .with_context(|| format!("Failed to parse webSocketDebuggerUrl `{}`", url))?;

    Ok(url)
}

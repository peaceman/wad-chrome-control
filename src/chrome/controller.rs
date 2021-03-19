use crate::{
    chrome::{self, config::ChromeConfig, supervisor::ChromeInfo},
    config::AppConfig,
};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc as TokioMpsc;
use tokio::sync::watch;
use tracing::{info, trace, warn};
use url::Url;

#[tracing::instrument(skip(config, chrome_info_rx, chrome_kill_tx, chrome_config_rx, webserver_socket_addr))]
pub async fn chrome_controller(
    config: AppConfig,
    mut chrome_info_rx: watch::Receiver<Option<ChromeInfo>>,
    chrome_kill_tx: TokioMpsc::UnboundedSender<ChromeInfo>,
    webserver_socket_addr: SocketAddr,
    mut chrome_config_rx: watch::Receiver<Option<ChromeConfig>>,
) -> anyhow::Result<()> {
    let webserver_url = Url::parse(format!("http://{}", webserver_socket_addr).as_ref())?;
    let mut got_new_chrome_info = true;

    loop {
        // loop until we receive a chrome info
        info!("Waiting for chrome info");
        let chrome_info = loop {
            let chrome_info = *chrome_info_rx.borrow();
            match (chrome_info, got_new_chrome_info) {
                (Some(chrome_info), true) => break chrome_info,
                _ => {
                    chrome_info_rx.changed().await?;
                    got_new_chrome_info = true;
                    continue;
                }
            }
        };

        got_new_chrome_info = false;

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
        let (inner_chrome_kill_tx, inner_chrome_kill_rx) = tokio::sync::oneshot::channel();
        let chrome_driver = chrome::driver::Driver::new(
            inner_chrome_kill_tx,
            chrome_websocket_url.clone(),
            config.chrome.heartbeat_interval,
        )?;

        // publish the currently available chrome config before waiting for changes
        if send_chrome_config_update(&chrome_config_rx, &chrome_driver, &webserver_url).is_err() {
            continue;
        }

        tokio::pin!(inner_chrome_kill_rx);

        loop {
            tokio::select! {
                Ok(_) = chrome_config_rx.changed() => {
                    info!("Chrome config changed");

                    if send_chrome_config_update(&chrome_config_rx, &chrome_driver, &webserver_url).is_err() {
                        break;
                    }
                }
                _ = &mut inner_chrome_kill_rx => {
                    info!("Received chrome kill signal from chrome driver; re-dispatching");
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
    chrome_driver: &chrome::driver::Driver,
    webserver_url: &Url,
) -> anyhow::Result<()> {
    match *chrome_config_rx.borrow() {
        Some(ref chrome_config) => chrome_driver.update_chrome_config(chrome_config.clone())?,
        None => chrome_driver.update_chrome_config(ChromeConfig {
            url: webserver_url.clone(),
        })?,
    }

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

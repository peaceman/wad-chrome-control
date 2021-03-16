use tokio::process;
use tokio::sync::mpsc as TokioMpsc;
use tokio::sync::watch;
use tracing::{error, info};

use crate::config::AppConfig;
use anyhow::Context;
use warp::addr::remote;

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub struct ChromeInfo {
    debugging_port: u16,
    process_id: u32,
}

impl ChromeInfo {
    pub fn get_debugging_port(&self) -> u16 {
        self.debugging_port
    }
}

pub async fn chrome_supervisor(
    config: AppConfig,
    chrome_info_tx: watch::Sender<Option<ChromeInfo>>,
    mut chrome_kill_rx: TokioMpsc::UnboundedReceiver<ChromeInfo>,
) {
    let mut chrome_handle = None;

    loop {
        if chrome_handle.is_none() {
            let spawn_chrome_result = spawn_chrome(&config);
            if let Err(e) = spawn_chrome_result {
                error!("{:?}", e);
                continue;
            }

            let (handle, port) = spawn_chrome_result.unwrap();
            let chrome_info = ChromeInfo {
                debugging_port: port,
                process_id: handle.id().unwrap(),
            };

            chrome_handle = Some(handle);

            if let Err(e) = chrome_info_tx.send(Some(chrome_info)) {
                error!(
                    "Failed to publish chrome info, closing chrome supervisor {:?}",
                    e
                );
                break;
            }
        }

        let chrome_pid = chrome_handle.as_ref().and_then(|ch| ch.id());

        tokio::select! {
            wait_result = chrome_handle.as_mut().unwrap().wait() => {
                match wait_result {
                    Ok(exit_status) => {
                        info!("Chrome exited with status {:?}", exit_status);
                    }
                    Err(e) => {
                        error!("Failed to wait for chrome process exiting {:?}", e);
                    }
                }

                chrome_handle = None;
            },
            Some(ChromeInfo { process_id, .. }) = chrome_kill_rx.recv() => {
                info!("Received signal to kill chrome");
                match (chrome_pid, Some(process_id)) {
                    (Some(current), Some(requested)) if current == requested => {
                        if let Err(e) = chrome_handle.as_mut().unwrap().start_kill() {
                            error!("Failed to send kill signal to chrome {:?}", e);
                            chrome_handle = None;
                        }
                    }
                    _ => {
                        info!(
                            "Skip killing chrome, pid did not match. Current {:?} Requested {:?}",
                            chrome_pid, process_id
                        );
                    }
                }
            }
        }
    }
}

fn spawn_chrome(config: &AppConfig) -> anyhow::Result<(process::Child, u16)> {
    let remote_debugging_port = portpicker::pick_unused_port()
        .with_context(|| "Failed to allocate unused port to use as chrome remote debugging port")?;

    let mut args = vec![format!("--remote-debugging-port={}", remote_debugging_port)];
    args.extend(config.chrome.args.iter().map(String::clone));

    let child = process::Command::new(&config.chrome.binary_path)
        .args(&args)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "Failed to spawn chrome at `{}` with args `{:?}`",
                config.chrome.binary_path.to_string_lossy(),
                &args
            )
        })?;

    info!(
        "Spawned chrome with pid {:?} and debugging port {:?}",
        child.id(),
        remote_debugging_port
    );

    Ok((child, remote_debugging_port))
}

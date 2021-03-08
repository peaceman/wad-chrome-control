use tokio::process;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tracing::{error, info};

use crate::config::AppConfig;
use anyhow::Context;

pub async fn chrome_supervisor(
    config: AppConfig,
    chrome_debugging_port_tx: watch::Sender<Option<u16>>,
    mut chrome_kill_rx: mpsc::Receiver<()>,
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
            chrome_handle = Some(handle);

            if let Err(e) = chrome_debugging_port_tx.send(Some(port)) {
                error!("Failed to publish chrome remote debugging port, closing chrome supervisor {:?}", e);
                break;
            }
        }

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
            _ = chrome_kill_rx.recv() => {
                info!("Received signal to kill chrome");
                if let Err(e) = chrome_handle.as_mut().unwrap().start_kill() {
                    error!("Failed to send kill signal to chrome {:?}", e);
                    chrome_handle = None;
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

    info!("Spawned chrome with pid {:?}", child.id());

    Ok((child, remote_debugging_port))
}

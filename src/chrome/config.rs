use crate::file_change_watcher::Watcher as FileChangeWatcher;

use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use serde::Deserialize;
use tokio::sync::watch;
use tokio::task;
use tracing::{info, warn};
use url::Url;

#[derive(Debug, Deserialize, Clone)]
pub struct ChromeConfig {
    pub url: Url,
}

pub struct Watcher<W>
where
    W: FileChangeWatcher + Send + Sync + 'static,
{
    config_tx: watch::Sender<Option<ChromeConfig>>,
    config_rx: watch::Receiver<Option<ChromeConfig>>,
    watcher: Arc<W>,
    chrome_config_path: PathBuf,
}

impl<W> Watcher<W>
where
    W: FileChangeWatcher + Send + Sync + 'static,
{
    pub fn new(watcher: Arc<W>, chrome_config_path: impl AsRef<Path>) -> Self {
        let (config_tx, config_rx) = watch::channel(None);

        Self {
            config_tx,
            config_rx,
            watcher,
            chrome_config_path: chrome_config_path.as_ref().to_owned(),
        }
    }

    pub fn run(self) -> task::JoinHandle<anyhow::Result<()>> {
        task::spawn(async move { self.async_task_loop().await })
    }

    pub fn channel(&self) -> watch::Receiver<Option<ChromeConfig>> {
        self.config_rx.clone()
    }

    #[tracing::instrument(name = "ChromeConfigWatcher::async_task_loop", skip(self))]
    async fn async_task_loop(self) -> anyhow::Result<()> {
        // try to read the chrome config initially before waiting for further file changes
        info!("Reading the initial chrome config");
        if let Ok(config) = self.read_chrome_config().await {
            self.config_tx
                .send(Some(config))
                .with_context(|| "Failed to publish initial chrome config")?;
        }

        let mut watch = self.watcher.watch(&self.chrome_config_path).await?;

        // wait for file changes and publish the updated chrome config
        info!("Entering chrome config watch loop");
        loop {
            let _ = watch
                .channel()
                .recv()
                .await
                .with_context(|| "The file watch channel will no longer supply any events")?;

            info!("Chrome config changed");
            match self.read_chrome_config().await {
                Ok(config) => self
                    .config_tx
                    .send(Some(config))
                    .with_context(|| "Failed to publish chrome config")?,
                Err(e) => {
                    warn!("Reading the chrome config failed: {:?}", e);
                    self.config_tx
                        .send(None)
                        .with_context(|| "Failed to publish chrome config")?
                }
            }
        }
    }

    async fn read_chrome_config(&self) -> anyhow::Result<ChromeConfig> {
        let file = File::open(&self.chrome_config_path)?;

        let join_handle = task::spawn_blocking(move || {
            let reader = BufReader::new(file);
            let chrome_config = serde_yaml::from_reader(reader)?;

            Ok(chrome_config)
        });

        join_handle.await?
    }
}

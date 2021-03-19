use crate::chrome::config::ChromeConfig;

use std::time::Duration;
use std::time::Instant;
use std::{sync::Arc, thread};

use headless_chrome::{Browser, Tab};
use tracing::{error, info, warn};
use url::Url;

pub struct Driver {
    event_loop_tx: crossbeam::channel::Sender<EventLoopMsg>,
}

impl Driver {
    pub fn new(
        chrome_kill_tx: tokio::sync::oneshot::Sender<()>,
        websocket_url: Url,
        heartbeat_interval: Duration,
    ) -> anyhow::Result<Self> {
        let event_loop = EventLoop::new(chrome_kill_tx, websocket_url, heartbeat_interval);
        let driver = Self {
            event_loop_tx: event_loop.channel(),
        };

        event_loop.run();

        Ok(driver)
    }

    pub fn update_chrome_config(&self, chrome_config: ChromeConfig) -> anyhow::Result<()> {
        Ok(self
            .event_loop_tx
            .send(EventLoopMsg::UpdateConfig(chrome_config))?)
    }
}

impl Drop for Driver {
    #[tracing::instrument(name = "ChromeDriver::drop", skip(self))]
    fn drop(&mut self) {
        info!("Chrome driver was dropped");

        if let Err(e) = self.event_loop_tx.send(EventLoopMsg::Shutdown) {
            warn!(
                "Failed to send shutdown message to chrome driver event loop during driver drop: {:?}",
                e
            );
        }
    }
}

#[derive(thiserror::Error, Debug)]
enum ChromeError {
    #[error("Failed to connect to chrome")]
    ConnectionFailure(failure::Error),
    #[error("Chrome was unresponsive")]
    Unresponsive(failure::Error),
}

#[derive(Debug)]
enum EventLoopMsg {
    UpdateConfig(ChromeConfig),
    Shutdown,
}

struct EventLoop {
    event_loop_rx: crossbeam::channel::Receiver<EventLoopMsg>,
    event_loop_tx: crossbeam::channel::Sender<EventLoopMsg>,
    heartbeat_ticker_rx: crossbeam::channel::Receiver<Instant>,
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
    websocket_url: Url,
    running: bool,
}

impl EventLoop {
    fn new(
        chrome_kill_tx: tokio::sync::oneshot::Sender<()>,
        websocket_url: Url,
        heartbeat_interval: Duration,
    ) -> Self {
        let (event_loop_tx, event_loop_rx) = crossbeam::channel::unbounded();
        let heartbeat_ticker_rx = crossbeam::channel::tick(heartbeat_interval);

        Self {
            event_loop_rx,
            event_loop_tx,
            heartbeat_ticker_rx,
            kill_tx: Some(chrome_kill_tx),
            websocket_url,
            running: true,
        }
    }

    fn channel(&self) -> crossbeam::channel::Sender<EventLoopMsg> {
        self.event_loop_tx.clone()
    }

    fn run(self) {
        thread::spawn(|| self.event_loop_thread());
    }

    #[tracing::instrument(name = "ChromeDriver::EventLoop::event_loop_thread", skip(self))]
    fn event_loop_thread(mut self) {
        let (browser, tab) = match self.initialize_browser() {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to initialize chrome {:?}", e);
                return;
            }
        };

        loop {
            crossbeam::channel::select! {
                recv(self.event_loop_rx) -> msg => {
                    match msg {
                        Ok(msg) => if let Err(e) = self.handle_event_loop_message(&tab, &msg) {
                            error!("Failed to handle event loop message {:?} {:?}", msg, e);
                            break;
                        },
                        Err(e) => {
                            error!("The event loop channel was disconnected {:?}", e);
                            break;
                        }
                    }
                },
                recv(self.heartbeat_ticker_rx) -> msg => {
                    match msg {
                        Ok(_) => if let Err(e) = self.handle_heartbeat_message(&browser) {
                            error!("Failed to handle heartbeat message {:?}", e);
                            break;
                        },
                        Err(e) => {
                            error!("The heartbeat channel was disconnected {:?}", e);
                            break;
                        }
                    }
                },
                default(Duration::from_millis(100)) => {},
            }

            if !self.running {
                break;
            }
        }
    }

    #[tracing::instrument(skip(self))]
    fn initialize_browser(&self) -> Result<(Browser, Arc<Tab>), ChromeError> {
        info!("Establish connection to chrome");
        let browser = Browser::connect(self.websocket_url.as_str().to_owned())
            .map_err(ChromeError::ConnectionFailure)?;

        info!("Wait for the initial tab");
        let tab = browser
            .wait_for_initial_tab()
            .map_err(ChromeError::Unresponsive)?;

        Ok((browser, tab))
    }

    fn handle_event_loop_message(
        &mut self,
        tab: &Arc<Tab>,
        msg: &EventLoopMsg,
    ) -> Result<(), ChromeError> {
        match msg {
            EventLoopMsg::UpdateConfig(config) => {
                info!("Received config update {:?}", config);
                tab.navigate_to(config.url.as_str())
                    .map_err(ChromeError::Unresponsive)?;
                tab.wait_until_navigated()
                    .map_err(ChromeError::Unresponsive)?;
            }
            EventLoopMsg::Shutdown => {
                info!("Received shutdown message");
                self.running = false;
            },
        }

        Ok(())
    }

    fn handle_heartbeat_message(&self, browser: &Browser) -> Result<(), ChromeError> {
        info!("Handle heartbeat");
        let _ = browser.get_version().map_err(ChromeError::Unresponsive)?;

        Ok(())
    }
}

impl Drop for EventLoop {
    #[tracing::instrument(name = "ChromeDriver::EventLoop::drop", skip(self))]
    fn drop(&mut self) {
        info!("Chrome driver event loop was dropped");

        if let Some(tx) = self.kill_tx.take() {
            if let Err(e) = tx.send(()) {
                warn!("Failed to send chrome kill message during drop of the chrome drivers event loop: {:?}", e);
            }
        }
    }
}

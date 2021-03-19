use std::fs::File;
use std::io::BufReader;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;

pub type AppConfig = Arc<Config>;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub chrome: Chrome,
    pub http: Http,
    pub data_base_folder: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct Chrome {
    pub binary_path: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(with = "humantime_serde")]
    pub heartbeat_interval: Duration,
}

#[derive(Debug, Deserialize)]
pub struct Http {
    pub listen_ip: IpAddr,
}

pub fn load_config() -> anyhow::Result<AppConfig> {
    let config_path = get_config_path()?;
    let file = File::open(&config_path)
        .with_context(|| format!("Failed to open config file {}", &config_path))?;

    Ok(Arc::new(serde_yaml::from_reader(BufReader::new(file))?))
}

fn get_config_path() -> anyhow::Result<String> {
    use std::env;
    use tracing::info;

    env::var("APP_CONFIG").or_else(|e| {
        info!(
            error = format!("{:?}", e).as_str(),
            "Missing or invalid APP_CONFIG env var, fallback to config.yml in current working dir"
        );
        Ok("config.yml".to_string())
    })
}

use anyhow::Context;
use serde::Deserialize;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

pub type AppConfig = Arc<Config>;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub chrome: Chrome,
}

#[derive(Debug, Deserialize)]
pub struct Chrome {
    pub binary_path: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
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

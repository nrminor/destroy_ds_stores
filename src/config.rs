use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub database_path: PathBuf,
    pub cache_window_hours: u64,
}

impl Default for Config {
    fn default() -> Self {
        let home_dir = dirs::home_dir().expect("Could not determine home directory");

        Self {
            database_path: home_dir.join(".dds").join("cache.sqlite"),
            cache_window_hours: 168, // 1 week
        }
    }
}

impl Config {
    pub async fn load() -> Result<Self> {
        let home_dir = dirs::home_dir().expect("Could not determine home directory");
        let dds_dir = home_dir.join(".dds");
        let config_path = dds_dir.join("config.toml");

        if config_path.exists() {
            let contents = tokio::fs::read_to_string(&config_path).await?;
            Ok(toml::from_str(&contents)?)
        } else {
            // Create default config
            let config = Self::default();
            tokio::fs::create_dir_all(&dds_dir).await?;
            let contents = toml::to_string_pretty(&config)?;
            tokio::fs::write(&config_path, contents).await?;
            Ok(config)
        }
    }
}

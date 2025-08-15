use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CliConfig {
    pub server: String,

    #[serde(default)]
    pub output_format: String,

    #[serde(default)]
    pub color: bool,
    pub timeout: u64,
    pub retries: u32,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            server: "http://127.0.0.1:50051".to_string(),
            output_format: "table".to_string(),
            color: true,
            timeout: 30,
            retries: 3,
        }
    }
}

impl CliConfig {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            let config = Self::default();
            config.save()?;
            return Ok(config);
        }

        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config from {:?}", config_path))?;

        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config from {:?}", config_path))?;

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory {:?}", parent))?;
        }

        let contents = toml::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(&config_path, contents)
            .with_context(|| format!("Failed to write config to {:?}", config_path))?;

        Ok(())
    }

    pub fn config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Failed to get home directory")?;

        Ok(home.join(".config").join("stitch").join("config.toml"))
    }
}

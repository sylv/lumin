use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub data_dir: PathBuf,
    pub cache_dir: Option<PathBuf>,
    pub chunk_preload: Option<(u64, u64)>,
    pub mount_path: PathBuf,
    pub allow_other: bool,
    pub mount_unprivileged: bool,
    pub ensure_unmounted: bool,
    pub torbox_key: String,
    pub torbox_username: Option<String>,
    pub torbox_password: Option<String>,
    pub delete_unmapped: bool,
    pub categories: Vec<String>,
    pub max_torrent_size: Option<u64>,
    pub cache_target_size: u64,
    pub cache_max_size: u64,
    pub cache_grace_period_secs: u64,
    pub cache_sweep_interval_secs: u64,
}

static CONFIG: once_cell::sync::Lazy<Config> =
    once_cell::sync::Lazy::new(|| load_config().expect("Failed to load configuration"));

pub fn get_config() -> &'static Config {
    &CONFIG
}

fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let cache_target_size: u64 = 100 * 1024 * 1024 * 1024; // 100GB
    let cache_max_size: u64 = 125 * 1024 * 1024 * 1024; // 125GB
    let chunk_preload: (u32, u32) = (4, 1); // preload the first/last N chunks
    let config = config::Config::builder()
        .add_source(config::Environment::with_prefix("lumin"))
        .set_default("allow_other", false)?
        .set_default("mount_unprivileged", true)?
        .set_default("ensure_unmounted", true)?
        .set_default("cache_target_size", cache_target_size)?
        .set_default("cache_max_size", cache_max_size)?
        .set_default("delete_unmapped", false)?
        .set_default("categories", vec!["sonarr", "radarr"])?
        .set_default("chunk_preload", vec![chunk_preload.0, chunk_preload.1])?
        .set_default("cache_grace_period_secs", 300)? // 5 minutes
        .set_default("cache_sweep_interval_secs", 60)? // 1 minute
        .build()
        .unwrap();

    let mut config: Config = config.try_deserialize()?;
    if !config.data_dir.exists() {
        std::fs::create_dir_all(&config.data_dir)?;
    }

    if !config.cache_dir.is_some() {
        config.cache_dir = Some(config.data_dir.join("cache"));
    }

    if !config.cache_dir.as_ref().unwrap().exists() {
        std::fs::create_dir_all(config.cache_dir.as_ref().unwrap())?;
    }

    let img_dir = config.data_dir.join("images");
    if !img_dir.exists() {
        std::fs::create_dir_all(&img_dir)?;
    }

    if config.cache_target_size + 5000000000 > config.cache_max_size {
        return Err("Cache target size must be less than 5GB less than cache max size".into());
    }

    if config.categories.len() == 1 {
        let first = config.categories.into_iter().next().unwrap();
        config.categories = first.split(",").map(|s| s.to_string()).collect::<Vec<String>>();
    }

    Ok(config)
}

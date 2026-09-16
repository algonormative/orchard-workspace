use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::HostError;

pub(crate) const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AppConfig {
    pub version: u32,
    pub port: Option<u16>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            port: None,
            workspaces: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceConfig {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub mail_path: PathBuf,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub repositories: Vec<RepositoryConfig>,
    #[serde(default)]
    pub task_stores: Vec<TaskStoreConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RepositoryConfig {
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub task_store_id: Option<String>,
    #[serde(default = "default_task_status")]
    pub task_status: String,
    #[serde(default)]
    pub task_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TaskStoreConfig {
    pub id: String,
    pub path: PathBuf,
    pub db_path: PathBuf,
    pub schema_version: u32,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub repository_id: Option<String>,
}

fn default_task_status() -> String {
    "none".to_owned()
}

pub(crate) fn load(data_root: &Path) -> Result<AppConfig, HostError> {
    fs::create_dir_all(data_root)?;
    let path = data_root.join("config.json");
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let mut file = File::open(&path)?;
    let mut source = String::new();
    file.read_to_string(&mut source)?;
    let config: AppConfig = serde_json::from_str(&source)?;
    if config.version != CONFIG_VERSION {
        return Err(HostError::UnsupportedConfigVersion(config.version));
    }
    Ok(config)
}

pub(crate) fn save(data_root: &Path, config: &AppConfig) -> Result<(), HostError> {
    fs::create_dir_all(data_root)?;
    let lock_path = data_root.join("config.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;

    let path = data_root.join("config.json");
    let temporary = data_root.join("config.json.tmp");
    let result = (|| -> Result<(), HostError> {
        let bytes = serde_json::to_vec_pretty(config)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        sync_directory(data_root)?;
        Ok(())
    })();
    let _ = lock.unlock();
    result
}

fn sync_directory(path: &Path) -> Result<(), HostError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

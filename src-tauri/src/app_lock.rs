use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppLockConfig {
    pub enabled: bool,
}

pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("app-lock.json")
}

pub fn load_config(data_dir: &Path) -> AppLockConfig {
    let path = config_path(data_dir);
    let Ok(bytes) = std::fs::read(&path) else {
        return AppLockConfig { enabled: false };
    };
    serde_json::from_slice(&bytes).unwrap_or(AppLockConfig { enabled: false })
}

pub fn save_config(data_dir: &Path, config: &AppLockConfig) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    let path = config_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_file_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!load_config(dir.path()).enabled);
    }

    #[test]
    fn round_trip_enabled_flag() {
        let dir = tempfile::tempdir().unwrap();
        save_config(dir.path(), &AppLockConfig { enabled: true }).unwrap();
        assert!(load_config(dir.path()).enabled);
        let raw = fs::read_to_string(config_path(dir.path())).unwrap();
        assert!(!raw.to_ascii_lowercase().contains("pin"));
        assert!(!raw.contains("password"));
    }
}

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct AppLockGate {
    enabled: AtomicBool,
    unlocked: AtomicBool,
    changed: tokio::sync::Notify,
}

impl AppLockGate {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            unlocked: AtomicBool::new(!enabled),
            changed: tokio::sync::Notify::new(),
        }
    }

    pub fn is_locked(&self) -> bool {
        self.enabled.load(Ordering::Acquire) && !self.unlocked.load(Ordering::Acquire)
    }

    pub fn unlock(&self) {
        self.unlocked.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }

    pub async fn wait_until_unlocked(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.is_locked() {
                return;
            }
            notified.await;
        }
    }
}

pub fn allowed_while_locked(command: &str) -> bool {
    matches!(
        command,
        "app_lock_status"
            | "app_lock_availability"
            | "app_lock_verify"
            | "app_lock_enable"
    ) || crate::migration_gate::allowed_command(command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppLockConfig {
    pub enabled: bool,
}

pub struct AppLockPaths {
    pub data_dir: PathBuf,
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
    fn enabled_gate_starts_locked_and_clears_only_after_unlock() {
        let gate = AppLockGate::new(true);
        assert!(gate.is_locked());
        gate.unlock();
        assert!(!gate.is_locked());
    }

    #[test]
    fn disabled_gate_starts_unlocked() {
        assert!(!AppLockGate::new(false).is_locked());
    }

    #[test]
    fn locked_allow_list_includes_migration_and_excludes_connections() {
        assert!(allowed_while_locked("app_lock_status"));
        assert!(allowed_while_locked("app_lock_verify"));
        assert!(allowed_while_locked("migration_start"));
        assert!(allowed_while_locked("quit_app"));
        assert!(!allowed_while_locked("load_connections"));
        assert!(!allowed_while_locked("app_lock_disable"));
    }

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

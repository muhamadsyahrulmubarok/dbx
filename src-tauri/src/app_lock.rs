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
            | "request_app_close_from_window_controls"
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

pub fn background_services_may_start(migration_ready: bool, locked: bool) -> bool {
    migration_ready && !locked
}

pub async fn wait_until_background_services_may_start(
    migration: &crate::migration_gate::MigrationGate,
    lock: &AppLockGate,
) {
    loop {
        migration.wait().await;
        lock.wait_until_unlocked().await;
        let migration_ready = migration.is_ready();
        let locked = lock.is_locked();
        if background_services_may_start(migration_ready, locked) {
            return;
        }
        log::warn!(
            "[app-lock] background services deferred after a false start sample: migration_ready={migration_ready} locked={locked}"
        );
    }
}

/// Resume and reopen may refresh connections only when a lock gate is present
/// and `background_services_may_start` is true. A missing lock gate returns false
/// and must not refresh. A false sample waits again.
pub async fn wait_until_connections_may_refresh(
    migration: &crate::migration_gate::MigrationGate,
    lock: Option<&AppLockGate>,
) -> bool {
    let Some(lock) = lock else {
        return false;
    };
    loop {
        migration.wait().await;
        lock.wait_until_unlocked().await;
        let migration_ready = migration.is_ready();
        let locked = lock.is_locked();
        if background_services_may_start(migration_ready, locked) {
            return true;
        }
        log::warn!(
            "[app-lock] connection refresh deferred after a false start sample: migration_ready={migration_ready} locked={locked}"
        );
    }
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
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn poll_now<F: Future + ?Sized>(mut fut: std::pin::Pin<&mut F>) -> Poll<F::Output> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(&waker);
        fut.as_mut().poll(&mut cx)
    }

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
    fn background_services_wait_for_both_gates() {
        assert!(!background_services_may_start(false, false));
        assert!(!background_services_may_start(true, true));
        assert!(background_services_may_start(true, false));
    }

    #[tokio::test]
    async fn background_services_wait_returns_after_migration_clears_during_lock() {
        let migration = crate::migration_gate::MigrationGate::new(true);
        let lock = AppLockGate::new(true);
        let wait = wait_until_background_services_may_start(&migration, &lock);
        tokio::pin!(wait);

        assert!(poll_now(wait.as_mut()).is_pending(), "the wait must park on the lock while migration is still ready");
        migration.set_ready(false);
        lock.unlock();
        assert!(
            poll_now(wait.as_mut()).is_pending(),
            "a false sample after unlock must wait again instead of returning"
        );
        migration.set_ready(true);
        assert!(poll_now(wait.as_mut()).is_ready());
    }

    #[tokio::test]
    async fn missing_lock_gate_does_not_allow_connection_refresh() {
        let migration = crate::migration_gate::MigrationGate::new(true);
        assert!(!wait_until_connections_may_refresh(&migration, None).await);
    }

    #[tokio::test]
    async fn resume_waits_until_unlocked_before_connection_refresh() {
        let migration = crate::migration_gate::MigrationGate::new(true);
        let lock = AppLockGate::new(true);
        let wait = wait_until_connections_may_refresh(&migration, Some(&lock));
        tokio::pin!(wait);
        assert!(poll_now(wait.as_mut()).is_pending());
        lock.unlock();
        assert_eq!(poll_now(wait.as_mut()), Poll::Ready(true));
    }

    #[tokio::test]
    async fn connection_refresh_wait_returns_after_migration_clears_during_lock() {
        let migration = crate::migration_gate::MigrationGate::new(true);
        let lock = AppLockGate::new(true);
        let wait = wait_until_connections_may_refresh(&migration, Some(&lock));
        tokio::pin!(wait);

        assert!(poll_now(wait.as_mut()).is_pending(), "the wait must park on the lock while migration is still ready");
        migration.set_ready(false);
        lock.unlock();
        assert!(
            poll_now(wait.as_mut()).is_pending(),
            "a false sample after unlock must wait again instead of returning"
        );
        migration.set_ready(true);
        assert_eq!(poll_now(wait.as_mut()), Poll::Ready(true));
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

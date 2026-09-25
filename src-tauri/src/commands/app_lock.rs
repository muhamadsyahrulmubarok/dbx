use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::app_lock::{load_config, save_config, AppLockConfig, AppLockGate, AppLockPaths};
use crate::hello::{availability, request_verification, HelloAvailability, HelloPrompt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppLockStatus {
    pub enabled: bool,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppLockAvailability {
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppLockVerifyResult {
    pub outcome: String,
}

pub(crate) fn enable_decision(available: bool, prompt: HelloPrompt) -> Result<(), &'static str> {
    if !available || prompt != HelloPrompt::Verified {
        return Err("APP_LOCK_UNAVAILABLE");
    }
    Ok(())
}

pub(crate) fn enable_lock(
    gate: &AppLockGate,
    data_dir: &Path,
    available: bool,
    prompt: HelloPrompt,
) -> Result<(), String> {
    if !available {
        return Err("APP_LOCK_UNAVAILABLE".to_string());
    }
    let from_ticket = gate.take_enable_ticket();
    if !from_ticket {
        enable_decision(true, prompt)?;
    }
    if let Err(err) = save_config(data_dir, &AppLockConfig { enabled: true }) {
        if from_ticket {
            gate.grant_verified_enable_ticket();
        }
        return Err(err);
    }
    gate.unlock();
    Ok(())
}

pub(crate) fn disable_lock(gate: &AppLockGate, data_dir: &Path) -> Result<(), String> {
    if gate.is_locked() {
        return Err("APP_LOCK_REQUIRED".to_string());
    }
    save_config(data_dir, &AppLockConfig { enabled: false })
}

pub(crate) fn verify_outcome(gate: &AppLockGate, prompt: HelloPrompt) -> &'static str {
    match prompt {
        HelloPrompt::Verified => {
            gate.grant_verified_enable_ticket();
            gate.unlock();
            "verified"
        }
        HelloPrompt::Canceled => "canceled",
        HelloPrompt::Unavailable => "unavailable",
    }
}

pub(crate) fn current_status(gate: &AppLockGate, data_dir: &Path) -> AppLockStatus {
    AppLockStatus { enabled: load_config(data_dir).enabled, locked: gate.is_locked() }
}

#[tauri::command]
pub fn app_lock_status(gate: State<'_, Arc<AppLockGate>>, paths: State<'_, AppLockPaths>) -> AppLockStatus {
    current_status(&gate, &paths.data_dir)
}

#[tauri::command]
pub async fn app_lock_availability() -> AppLockAvailability {
    AppLockAvailability { available: matches!(availability().await, HelloAvailability::Available) }
}

#[tauri::command]
pub async fn app_lock_verify(
    app: tauri::AppHandle,
    gate: State<'_, Arc<AppLockGate>>,
) -> Result<AppLockVerifyResult, String> {
    let prompt = request_app_lock_prompt(&app).await;
    Ok(AppLockVerifyResult { outcome: verify_outcome(&gate, prompt).to_string() })
}

#[tauri::command]
pub async fn app_lock_enable(
    app: tauri::AppHandle,
    gate: State<'_, Arc<AppLockGate>>,
    paths: State<'_, AppLockPaths>,
) -> Result<AppLockStatus, String> {
    let available = matches!(availability().await, HelloAvailability::Available);
    // `app_lock_verify` stores a one-shot ticket on Verified. Consume it instead of prompting again.
    let prompt = if gate.has_enable_ticket() { HelloPrompt::Canceled } else { request_app_lock_prompt(&app).await };
    enable_lock(&gate, &paths.data_dir, available, prompt)?;
    Ok(current_status(&gate, &paths.data_dir))
}

async fn request_app_lock_prompt(app: &tauri::AppHandle) -> HelloPrompt {
    #[cfg(windows)]
    {
        request_verification(app).await
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        request_verification().await
    }
}

#[tauri::command]
pub fn app_lock_disable(
    gate: State<'_, Arc<AppLockGate>>,
    paths: State<'_, AppLockPaths>,
) -> Result<AppLockStatus, String> {
    disable_lock(&gate, &paths.data_dir)?;
    Ok(current_status(&gate, &paths.data_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_lock::{load_config, save_config, AppLockConfig, AppLockGate};
    use crate::hello::HelloPrompt;

    #[test]
    fn enable_requires_a_verified_prompt() {
        assert!(enable_decision(true, HelloPrompt::Verified).is_ok());
        assert!(enable_decision(true, HelloPrompt::Canceled).is_err());
        assert!(enable_decision(false, HelloPrompt::Verified).is_err());
    }

    #[test]
    fn disable_rejects_a_locked_gate_and_does_not_clear_the_file() {
        let dir = tempfile::tempdir().unwrap();
        save_config(dir.path(), &AppLockConfig { enabled: true }).unwrap();
        let gate = AppLockGate::new(true);
        let err = disable_lock(&gate, dir.path()).unwrap_err();
        assert_eq!(err, "APP_LOCK_REQUIRED");
        assert!(load_config(dir.path()).enabled);
    }

    #[test]
    fn disable_clears_the_file_when_the_gate_is_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        save_config(dir.path(), &AppLockConfig { enabled: true }).unwrap();
        let gate = AppLockGate::new(true);
        gate.unlock();
        disable_lock(&gate, dir.path()).unwrap();
        assert!(!load_config(dir.path()).enabled);
        assert!(!gate.is_locked());
    }

    #[test]
    fn enable_failure_leaves_the_file_unchanged_and_keeps_the_gate_locked() {
        let dir = tempfile::tempdir().unwrap();
        let gate = AppLockGate::new(true);
        let err = enable_lock(&gate, dir.path(), true, HelloPrompt::Canceled).unwrap_err();
        assert_eq!(err, "APP_LOCK_UNAVAILABLE");
        assert!(gate.is_locked());
        assert!(!load_config(dir.path()).enabled);
    }

    #[test]
    fn unlocked_gate_without_a_ticket_does_not_persist_a_canceled_prompt() {
        let open_dir = tempfile::tempdir().unwrap();
        let open = AppLockGate::new(false);
        assert!(!open.is_locked());
        let err = enable_lock(&open, open_dir.path(), true, HelloPrompt::Canceled).unwrap_err();
        assert_eq!(err, "APP_LOCK_UNAVAILABLE");
        assert!(!load_config(open_dir.path()).enabled);
        assert!(!open.is_locked());

        let locked_dir = tempfile::tempdir().unwrap();
        let locked = AppLockGate::new(true);
        let err = enable_lock(&locked, locked_dir.path(), true, HelloPrompt::Canceled).unwrap_err();
        assert_eq!(err, "APP_LOCK_UNAVAILABLE");
        assert!(locked.is_locked());
        assert!(!load_config(locked_dir.path()).enabled);
    }

    #[test]
    fn verified_ticket_is_consumed_and_persists_without_a_locked_gate() {
        let dir = tempfile::tempdir().unwrap();
        let gate = AppLockGate::new(false);
        assert!(!gate.is_locked());
        assert_eq!(verify_outcome(&gate, HelloPrompt::Verified), "verified");
        enable_lock(&gate, dir.path(), true, HelloPrompt::Canceled).unwrap();
        assert!(load_config(dir.path()).enabled);
        assert!(!gate.is_locked());

        let err = enable_lock(&gate, dir.path(), true, HelloPrompt::Canceled).unwrap_err();
        assert_eq!(err, "APP_LOCK_UNAVAILABLE");
        assert!(load_config(dir.path()).enabled);
    }

    #[test]
    fn enable_persists_the_flag_and_unlocks_after_verification() {
        let dir = tempfile::tempdir().unwrap();
        let gate = AppLockGate::new(true);
        enable_lock(&gate, dir.path(), true, HelloPrompt::Verified).unwrap();
        assert!(load_config(dir.path()).enabled);
        assert!(!gate.is_locked());
    }

    #[test]
    fn verify_unlocks_only_for_verified_and_keeps_a_disabled_gate_open() {
        let locked = AppLockGate::new(true);
        assert_eq!(verify_outcome(&locked, HelloPrompt::Canceled), "canceled");
        assert!(locked.is_locked());
        assert_eq!(verify_outcome(&locked, HelloPrompt::Unavailable), "unavailable");
        assert!(locked.is_locked());
        assert_eq!(verify_outcome(&locked, HelloPrompt::Verified), "verified");
        assert!(!locked.is_locked());

        let open = AppLockGate::new(false);
        assert_eq!(verify_outcome(&open, HelloPrompt::Canceled), "canceled");
        assert!(!open.is_locked());
    }

    #[test]
    fn status_reports_the_saved_flag_and_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        save_config(dir.path(), &AppLockConfig { enabled: true }).unwrap();
        let gate = AppLockGate::new(true);
        let status = current_status(&gate, dir.path());
        assert!(status.enabled);
        assert!(status.locked);
        gate.unlock();
        let status = current_status(&gate, dir.path());
        assert!(status.enabled);
        assert!(!status.locked);
    }
}

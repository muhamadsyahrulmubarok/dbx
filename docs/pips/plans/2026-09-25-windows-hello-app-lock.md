# Windows Hello App Lock Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Opt-in Windows Hello app lock that blocks desktop IPC and in-process data services until the current OS user passes a native verifier prompt.

**Architecture:** A small JSON flag lives beside `dbx.db` and is read before storage opens. An in-memory `AppLockGate` starts locked when that flag is set and is composed with `MigrationGate` inside the existing invoke wrapper. MCP HTTP, the MCP bridge, the backup worker, and Redis pub/sub stay stopped until both gates pass. The Vue lock panel only calls those native commands; it is not the security boundary.

**Tech Stack:** Rust / Tauri 2 invoke handler, `windows` WinRT `UserConsentVerifier` on Windows only, Vue 3 `StartupGate.vue`, Vitest, `cargo test -p dbx`.

**Spec:** Investigation notes for Windows Hello app lock. Do not ship a Vue-only lock.

## Global Constraints

- Windows only, opt-in from Desktop settings. Other platforms keep today's startup path and hide the control.
- Before enabling, `UserConsentVerifier::CheckAvailabilityAsync` must succeed and `RequestVerificationAsync` must return `Verified`.
- Each later launch shows the native prompt before connections load. Cancel or unavailable leaves the app locked, with retry or quit.
- An OS configuration change must not clear the opt-in flag.
- The gate starts locked when the flag is enabled. Allow only lock status, verification, quit, and the existing migration commands until verification succeeds.
- Check migration and lock independently. A finished migration does not unlock the app.
- Put the gate in the invoke dispatcher (`migration_gate::guard_handler`), not only in Vue.
- Store the flag in `app-lock.json` under the resolved data directory, readable before `Storage::open_unmigrated`. Do not store a PIN or Hello secret in the app or in `dbx.db`.
- Unlock state is in-memory only and dies with the process.
- Do not add an explicit Lock action or lock-on-hide in this plan.
- Do not start MCP HTTP, the MCP bridge, the UI backup worker, or Redis pub/sub while locked.
- `dbx` CLI and any standalone MCP process are a separate trust boundary. This plan does not gate them. Say that in the settings copy.
- Windows Hello verifies the current OS user. It does not encrypt `dbx.db`, replace the Credential Manager data key, or stop another same-user process from reading files.
- Plugin commands (`fs`, `updater`, `shell`, clipboard, dialog, process) do not pass through `guard_handler`. Leave them as they are. Updater checks do not read connection secrets and stay available while locked.
- Deep links stay queued. Do not execute them natively while locked. `App.vue` is not imported until unlock, so the UI cannot act on them early.
- Dialect YAML loading does not read connection secrets and may stay at setup. Do not start plugin work that uses stored connections before unlock.

## File structure

- Create `src-tauri/src/app_lock.rs` — flag file, in-memory gate, allow-list, composed IPC guard.
- Create `src-tauri/src/hello.rs` — verifier trait plus Windows `UserConsentVerifier` implementation. Non-Windows returns `Unavailable`.
- Modify `src-tauri/src/migration_gate.rs` — keep migration allow-list; call the composed guard from `guard_handler`.
- Modify `src-tauri/src/lib.rs` — load the flag before opening `dbx.db`, manage `AppLockGate`, delay background services.
- Modify `src-tauri/src/commands/mod.rs` and `src-tauri/src/commands/app_lock.rs` — status, availability, verify, enable, disable commands.
- Modify `src-tauri/Cargo.toml` — Windows-only `windows` crate.
- Modify `apps/desktop/src/StartupGate.vue` and `apps/desktop/src/__tests__/StartupGate.spec.ts` — lock panel before `App.vue`.
- Modify `apps/desktop/src/components/editor/EditorSettingsDialog.vue` — Windows-only opt-in switch.
- Modify `apps/desktop/src/lib/backend/tauri.ts` — command wrappers. Do not add the flag to `DesktopSettings` or `dbx.db`.

---

### Task 1: Lock flag file

**Files:**
- Create: `src-tauri/src/app_lock.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod app_lock;`)
- Test: `src-tauri/src/app_lock.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: a data directory `Path`.
- Produces:
  - `pub struct AppLockConfig { pub enabled: bool }`
  - `pub fn config_path(data_dir: &Path) -> PathBuf` → `data_dir.join("app-lock.json")`
  - `pub fn load_config(data_dir: &Path) -> AppLockConfig` (missing or unreadable file means `enabled: false`)
  - `pub fn save_config(data_dir: &Path, config: &AppLockConfig) -> Result<(), String>`

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo test -p dbx --lib app_lock::tests -- --test-threads=8`

Expected: FAIL because `app_lock` is not a module.

- [ ] **Step 3: Implement the file**

```rust
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
```

Add `mod app_lock;` next to `mod migration_gate;` in `src-tauri/src/lib.rs`.

- [ ] **Step 4: Re-run the test**

Run: `cargo test -p dbx --lib app_lock::tests -- --test-threads=8`

Expected: PASS. The JSON file contains only `{"enabled": true}`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/app_lock.rs src-tauri/src/lib.rs
git commit -m "feat(desktop): store Windows Hello opt-in outside the database"
```

---

### Task 2: In-memory gate and composed IPC guard

**Files:**
- Modify: `src-tauri/src/app_lock.rs`
- Modify: `src-tauri/src/migration_gate.rs`
- Test: both modules

**Interfaces:**
- Consumes: `AppLockConfig.enabled` from Task 1. `migration_gate::allowed_command`.
- Produces:
  - `pub struct AppLockGate { enabled: AtomicBool, unlocked: AtomicBool }`
  - `AppLockGate::new(enabled: bool) -> Self` — when `enabled` is true, `unlocked` starts false.
  - `fn is_locked(&self) -> bool`
  - `fn unlock(&self)`
  - `pub async fn wait_until_unlocked(&self)`
  - `pub fn allowed_while_locked(command: &str) -> bool`
  - `migration_gate::guard_handler` rejects with `APP_LOCK_REQUIRED` when the lock gate is missing or locked and the command is not allowed. Migration denial still returns `DATA_MIGRATION_REQUIRED` and is checked separately.

Allowed while locked, in addition to every command already returned by `migration_gate::allowed_command`:

- `app_lock_status`
- `app_lock_availability`
- `app_lock_verify`
- `app_lock_enable`

`app_lock_disable` is not allowed while locked.

- [ ] **Step 1: Write the failing gate tests in `app_lock.rs`**

```rust
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
```

- [ ] **Step 2: Extend the existing invoke test in `migration_gate.rs`**

In `invoke_dispatch_rejects_business_commands_until_ready`, also `.manage(Arc::new(app_lock::AppLockGate::new(false)))` so today's migration behavior stays green.

Add this test beside it:

```rust
#[test]
fn invoke_dispatch_rejects_business_commands_while_locked_even_after_migration() {
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets};
    let migration = Arc::new(MigrationGate::new(true));
    let lock = Arc::new(crate::app_lock::AppLockGate::new(true));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let command_calls = calls.clone();
    let app = mock_builder()
        .manage(migration)
        .manage(lock.clone())
        .invoke_handler(guard_handler(move |invoke| {
            command_calls.fetch_add(1, Ordering::SeqCst);
            invoke.resolver.resolve("dispatched");
            true
        }))
        .build(mock_context(noop_assets()))
        .unwrap();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
    let request = |command: &str| tauri::webview::InvokeRequest {
        cmd: command.into(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url: "http://tauri.localhost".parse().unwrap(),
        body: tauri::ipc::InvokeBody::default(),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.into(),
    };
    assert_eq!(
        get_ipc_response(&webview, request("load_connections")).unwrap_err(),
        serde_json::json!("APP_LOCK_REQUIRED")
    );
    assert!(get_ipc_response(&webview, request("migration_status")).is_ok());
    assert!(get_ipc_response(&webview, request("app_lock_status")).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    lock.unlock();
    assert!(get_ipc_response(&webview, request("load_connections")).is_ok());
}
```

- [ ] **Step 3: Run both tests and confirm the new one fails**

Run: `cargo test -p dbx --lib migration_gate::tests::invoke_dispatch_rejects_business_commands_while_locked_even_after_migration -- --test-threads=8`

Expected: FAIL to compile (`AppLockGate` missing) or FAIL the assertion.

- [ ] **Step 4: Implement the gate and compose it**

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    matches!(command, "app_lock_status" | "app_lock_availability" | "app_lock_verify" | "app_lock_enable")
        || crate::migration_gate::allowed_command(command)
}
```

Replace the body of `guard_handler` with both checks. Missing lock state denies business commands the same way a missing migration gate does:

```rust
let state = invoke.message.state_ref();
let locked = match state.try_get::<Arc<crate::app_lock::AppLockGate>>() {
    Some(gate) => gate.is_locked(),
    None => true,
};
if locked && !crate::app_lock::allowed_while_locked(invoke.message.command()) {
    invoke.resolver.reject("APP_LOCK_REQUIRED");
    return true;
}
let ready = state.try_get::<Arc<MigrationGate>>().is_some_and(|gate| gate.is_ready());
if !ready && !allowed_command(invoke.message.command()) {
    invoke.resolver.reject("DATA_MIGRATION_REQUIRED");
    return true;
}
handler(invoke)
```

`allowed_while_locked` calls `allowed_command`, and `guard_handler` lives in `migration_gate.rs`, so keep `allowed_while_locked` in `app_lock.rs` and do not create a module cycle: `app_lock` may call `migration_gate::allowed_command` because `migration_gate` must not call back into a function that calls `allowed_command` during init. That one-way call is safe. Do not have `migration_gate` import `allowed_while_locked` from a child that imports `migration_gate` at the top level in a cycle that Rust rejects. If the compiler reports a cycle, move `allowed_command` into `app_lock.rs` as a duplicate match list copied from `migration_gate.rs` lines 33-48 and add a test that both lists contain `migration_start`.

- [ ] **Step 5: Re-run the migration and lock tests**

Run: `cargo test -p dbx --lib migration_gate::tests -- --test-threads=8`

Expected: PASS, including the existing "denies until migration ready" test.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app_lock.rs src-tauri/src/migration_gate.rs
git commit -m "feat(desktop): reject IPC while the app lock is held"
```

---

### Task 3: Windows Hello verifier

**Files:**
- Create: `src-tauri/src/hello.rs`
- Modify: `src-tauri/src/lib.rs` (`mod hello;`)
- Modify: `src-tauri/Cargo.toml`
- Test: `src-tauri/src/hello.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub enum HelloAvailability { Available, Unavailable }`
  - `pub enum HelloPrompt { Verified, Canceled, Unavailable }`
  - `pub fn availability() -> HelloAvailability`
  - `pub async fn request_verification() -> HelloPrompt`
  - Non-Windows: both functions return `Unavailable` and do not prompt.

- [ ] **Step 1: Add the Windows dependency**

In `src-tauri/Cargo.toml`, extend the existing Windows target block:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Threading", "Win32_UI_WindowsAndMessaging"] }
windows = { version = "0.61", features = [
  "Foundation",
  "Security_Credentials_UI",
  "implement",
] }
```

- [ ] **Step 2: Write the non-Windows test and the outcome mapping test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_windows_availability_is_unavailable() {
        if cfg!(windows) {
            return;
        }
        assert_eq!(availability(), HelloAvailability::Unavailable);
    }

    #[test]
    fn canceled_hresult_maps_to_canceled() {
        assert_eq!(prompt_from_hresult(-2147023673), HelloPrompt::Canceled); // HRESULT_FROM_WIN32(ERROR_CANCELLED)
    }
}
```

`prompt_from_hresult` is `pub(crate)` so the mapping can be tested on Linux CI without WinRT.

- [ ] **Step 3: Implement `hello.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloPrompt {
    Verified,
    Canceled,
    Unavailable,
}

pub(crate) fn prompt_from_hresult(code: i32) -> HelloPrompt {
    if code == 0 {
        HelloPrompt::Verified
    } else if code == -2147023673 {
        HelloPrompt::Canceled
    } else {
        HelloPrompt::Unavailable
    }
}

#[cfg(not(windows))]
pub fn availability() -> HelloAvailability {
    HelloAvailability::Unavailable
}

#[cfg(not(windows))]
pub async fn request_verification() -> HelloPrompt {
    HelloPrompt::Unavailable
}
```

On Windows, implement `availability` with `UserConsentVerifier::CheckAvailabilityAsync` and `request_verification` with `UserConsentVerifier::RequestVerificationAsync("Unlock DBX")`. Map `UserConsentVerifierAvailability::Available` to `Available`. Map `UserConsentVerificationResult::Verified` to `Verified` and `Canceled` to `Canceled`. Every other status, including device-not-present and disabled-by-policy, returns `Unavailable`. Do not write the flag from this module.

- [ ] **Step 4: Run the hello tests**

Run: `cargo test -p dbx --lib hello::tests -- --test-threads=8`

Expected: PASS on Linux and macOS. On Windows the same unit tests pass; a manual prompt is Task 8.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/hello.rs src-tauri/src/lib.rs
git commit -m "feat(desktop): add a Windows Hello verifier"
```

---

### Task 4: Lock commands

**Files:**
- Create: `src-tauri/src/commands/app_lock.rs`
- Modify: `src-tauri/src/commands/mod.rs` (`pub mod app_lock;`)
- Modify: `src-tauri/src/lib.rs` invoke handler list, and `setup` so the gate is managed before any window IPC.
- Test: `src-tauri/src/commands/app_lock.rs`

**Interfaces:**
- Consumes: `AppLockGate`, `load_config`, `save_config`, `hello::availability`, `hello::request_verification`.
- Produces commands registered on the invoke handler:
  - `app_lock_status() -> { enabled: bool, locked: bool }`
  - `app_lock_availability() -> { available: bool }`
  - `app_lock_verify() -> { outcome: "verified" | "canceled" | "unavailable" }` — on `verified`, call `gate.unlock()`. On the other outcomes, leave the gate locked when the flag is enabled.
  - `app_lock_enable() -> Result<Status, String>` — require `availability == Available` and `request_verification == Verified`, then `save_config(enabled: true)` and `unlock()`. Failure leaves the file unchanged.
  - `app_lock_disable() -> Result<Status, String>` — reject with `APP_LOCK_REQUIRED` when `is_locked()`. Otherwise `save_config(enabled: false)`.

Manage a `PathBuf` data directory the commands can see. `setup` already computes `data_dir` before `Storage::open_unmigrated`. Load the flag there:

```rust
let lock_config = app_lock::load_config(&data_dir);
let app_lock_gate = Arc::new(app_lock::AppLockGate::new(lock_config.enabled));
app.manage(app_lock_gate.clone());
app.manage(app_lock::AppLockPaths { data_dir: data_dir.clone() });
```

This `manage` call stays before `Storage::open_unmigrated`. Opening the database afterward is still required so migration commands work while locked. Do not put `enabled` on `DesktopSettings`.

- [ ] **Step 1: Write command tests with a temp directory and a fake prompt result**

Test the pure decision function, not WinRT:

```rust
pub(crate) fn enable_decision(available: bool, prompt: HelloPrompt) -> Result<(), &'static str> {
    if !available || prompt != HelloPrompt::Verified {
        return Err("APP_LOCK_UNAVAILABLE");
    }
    Ok(())
}

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
```

`disable_lock` writes the file only when `!gate.is_locked()`.

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p dbx --lib commands::app_lock -- --test-threads=8`

Expected: FAIL to compile.

- [ ] **Step 3: Implement the commands and register them**

Add the five commands to the `tauri::generate_handler![]` list next to `commands::app_settings::load_desktop_settings`.

`app_lock_verify` maps `HelloPrompt` to the strings above. It unlocks only for `Verified`. A canceled prompt on a disabled gate stays unlocked (`AppLockGate::new(false)`).

- [ ] **Step 4: Re-run the command tests and the IPC lock test**

Run: `cargo test -p dbx --lib commands::app_lock migration_gate::tests::invoke_dispatch_rejects_business_commands_while_locked_even_after_migration -- --test-threads=8`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/app_lock.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src-tauri/src/app_lock.rs
git commit -m "feat(desktop): add app-lock status and verification commands"
```

---

### Task 5: Hold background data paths until unlock

**Files:**
- Modify: `src-tauri/src/lib.rs` setup block around the backup, MCP, Redis, and deep-link startup (`lib.rs` roughly lines 1688-1736)
- Modify: `src-tauri/src/app_lock.rs`
- Test: `src-tauri/src/app_lock.rs`

**Interfaces:**
- Consumes: `AppLockGate::wait_until_unlocked`, `MigrationGate::wait`.
- Produces: `pub fn background_services_may_start(migration_ready: bool, locked: bool) -> bool` which is `migration_ready && !locked`.

Current order in `setup` opens storage, then starts the backup child, Redis pub/sub, and schedules MCP HTTP and the MCP bridge on `migration_gate.wait()` only.

- [ ] **Step 1: Write the predicate test**

```rust
#[test]
fn background_services_wait_for_both_gates() {
    assert!(!background_services_may_start(false, false));
    assert!(!background_services_may_start(true, true));
    assert!(background_services_may_start(true, false));
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p dbx --lib app_lock::tests::background_services_wait_for_both_gates -- --test-threads=8`

- [ ] **Step 3: Delay the four services**

Move `BackgroundBackup::new` / `resume` and `start_pubsub_server` into the same spawned task that already waits on the migration gate. Wait on the lock after migration:

```rust
let services_state = state.clone();
let services_dir = data_dir.clone();
let services_migration = migration_gate.clone();
let services_lock = app_lock_gate.clone();
let services_http = mcp_http_server.clone();
tauri::async_runtime::spawn(async move {
    services_migration.wait().await;
    services_lock.wait_until_unlocked().await;
    let backups = background_backup::BackgroundBackup::new(services_state.clone(), services_dir.clone());
    // The existing setup manages BackgroundBackup immediately so commands can find it.
    // Keep app.manage(backups) where it is, but call BackgroundBackup::new only after both waits.
    commands::mcp_http_server::start_if_enabled(services_state.clone(), services_http).await;
    commands::mcp_bridge::start(app_handle, services_state, services_dir);
});
```

`BackgroundBackup` is currently managed synchronously because `database_backup_command` needs the state. Split construction:

- Add `BackgroundBackup::deferred(data_dir)` that stores the path and does not spawn `--ui-backup-worker`.
- Add `BackgroundBackup::start_worker(&self) -> Result<(), String>` with the current `Command::new(current_exe())` spawn and `resume()` call.
- Call `start_worker` only inside the task above.
- `database_backup_command` returns `"APP_LOCK_REQUIRED"` until `start_worker` has run. The invoke guard already blocks that command while locked, so this is the post-unlock path.

Redis: construct `PubSubServerState::unavailable()` at setup, `app.manage` that, and replace it by starting the real listener inside the same task. If `PubSubServerState` cannot be started twice, add `PubSubServerState::start_if_stopped(&self, state: Arc<AppState>)` that binds only when `port` is `None`.

Do not emit deep-link events until `wait_until_unlocked` resolves. Keep `DeepLinkOpenState::push_*` at startup so the links are not dropped. After unlock, emit the queued links once. Single-instance handoff may still call `show_main_window`.

Leave `tauri_plugin_updater` registered. Leave dialect plugin scanning where it is.

- [ ] **Step 4: Re-run the predicate test and backup worker tests**

Run: `cargo test -p dbx --lib app_lock::tests background_backup -- --test-threads=8`

Expected: PASS. A locked gate never reaches `Command::new` for the backup worker. Confirm by reading `start_worker` call sites: the only call is after both waits.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/app_lock.rs src-tauri/src/background_backup.rs src-tauri/src/commands/redis_pubsub_server.rs
git commit -m "feat(desktop): start data services only after app unlock"
```

---

### Task 6: Startup lock panel

**Files:**
- Modify: `apps/desktop/src/StartupGate.vue`
- Modify: `apps/desktop/src/__tests__/StartupGate.spec.ts`
- Modify: `apps/desktop/src/lib/backend/tauri.ts`
- Modify: locale files that already contain `migration.checking` (add `appLock.title`, `appLock.retry`, `appLock.quit`, `appLock.unavailable`)

**Interfaces:**
- Consumes: `app_lock_status`, `app_lock_verify`, and the existing close command used by the desktop quit path (`request_app_close_from_window_controls`).
- Produces: `StartupGate` does not import `App.vue` while `locked === true`.

Add to `tauri.ts`:

```ts
export interface AppLockStatus {
  enabled: boolean;
  locked: boolean;
}

export async function appLockStatus(): Promise<AppLockStatus> {
  if (!isTauriRuntime()) return { enabled: false, locked: false };
  return invoke<AppLockStatus>("app_lock_status");
}

export async function appLockVerify(): Promise<"verified" | "canceled" | "unavailable"> {
  const result = await invoke<{ outcome: "verified" | "canceled" | "unavailable" }>("app_lock_verify");
  return result.outcome;
}
```

- [ ] **Step 1: Extend `StartupGate.spec.ts`**

Mock `appLockStatus` and `appLockVerify` on the existing `@/lib/backend/api` mock (re-export them from the api module the gate imports; if the gate imports `tauri.ts` directly, mock that module the same way).

```ts
it("does not import the business app while the native lock is held", async () => {
  mocks.appLockStatus.mockResolvedValue({ enabled: true, locked: true });
  mocks.appLockVerify.mockResolvedValue("canceled");
  await mountGate();
  await vi.waitFor(() => expect(root.querySelector("[data-app-lock]")).not.toBeNull());
  expect(mocks.appImported).not.toHaveBeenCalled();
  expect(mocks.migrationStatus).not.toHaveBeenCalled();
});

it("imports the app after verification succeeds", async () => {
  mocks.appLockStatus.mockResolvedValue({ enabled: true, locked: true });
  mocks.appLockVerify.mockResolvedValue("verified");
  mocks.appLockStatus
    .mockResolvedValueOnce({ enabled: true, locked: true })
    .mockResolvedValueOnce({ enabled: true, locked: false });
  await mountGate();
  await vi.waitFor(() => expect(mocks.appImported).toHaveBeenCalledTimes(1));
});
```

The first launch calls `appLockVerify` once because the status is locked. A canceled result shows retry and quit and does not import `App.vue`. Retry calls `appLockVerify` again.

- [ ] **Step 2: Run the new tests and confirm they fail**

Run: `pnpm exec vitest run apps/desktop/src/__tests__/StartupGate.spec.ts`

Expected: FAIL because the lock panel is absent.

- [ ] **Step 3: Implement the panel in `StartupGate.vue`**

On desktop, `initialize` first calls `appLockStatus`. When `locked` is true, set `locked = true`, call `appLockVerify` once, and return without `migration.initialize()` when the outcome is not `verified`. When the outcome is `verified`, refresh status and continue into the existing migration path. Web (`!isTauriRuntime()`) skips the lock calls.

Template branch, before the migration wizard:

```vue
<div v-else-if="appLocked" data-app-lock>
  <p>{{ t("appLock.title") }}</p>
  <button type="button" data-app-lock-retry @click="retryUnlock">{{ t("appLock.retry") }}</button>
  <button type="button" data-app-lock-quit @click="quitFromLock">{{ t("appLock.quit") }}</button>
</div>
```

`quitFromLock` invokes `request_app_close_from_window_controls`. Do not catch a canceled prompt and then continue into `App.vue`.

- [ ] **Step 4: Re-run StartupGate tests**

Run: `pnpm exec vitest run apps/desktop/src/__tests__/StartupGate.spec.ts`

Expected: PASS, including the existing migration and web-auth cases.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/StartupGate.vue apps/desktop/src/__tests__/StartupGate.spec.ts apps/desktop/src/lib/backend/tauri.ts apps/desktop/src/i18n
git commit -m "feat(desktop): show a retry or quit panel while the app is locked"
```

---

### Task 7: Settings opt-in

**Files:**
- Modify: `apps/desktop/src/components/editor/EditorSettingsDialog.vue` next to the tray switch around the `v-if="!isWeb"` block at line 7014
- Modify: `apps/desktop/src/lib/backend/tauri.ts`
- Test: a focused Vitest if the dialog already has a settings behavior test that can mount this switch; otherwise a small pure helper test in `apps/desktop/src/lib/startup/appLockSettings.ts`

**Interfaces:**
- Consumes: `app_lock_availability`, `app_lock_enable`, `app_lock_disable`, `app_lock_status`.
- Produces: `enableAppLock(): Promise<"enabled" | "canceled" | "unavailable">` and `disableAppLock(): Promise<void>`.

The switch is visible only when `isTauriRuntime()` and `navigator` platform data from the existing `get_platform` command is `"windows"`. Do not add a field to `DesktopSettings`.

- [ ] **Step 1: Write the helper test**

```ts
import { describe, expect, it } from "vitest";
import { nextLockEnabled } from "./appLockSettings";

describe("app lock settings", () => {
  it("enables only after a verified prompt", () => {
    expect(nextLockEnabled({ available: true, outcome: "verified" })).toBe("enable");
    expect(nextLockEnabled({ available: true, outcome: "canceled" })).toBe("keep");
    expect(nextLockEnabled({ available: false, outcome: "verified" })).toBe("unavailable");
  });
});
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `pnpm exec vitest run apps/desktop/src/lib/startup/appLockSettings.spec.ts`

- [ ] **Step 3: Implement the helper and the switch**

```ts
export function nextLockEnabled(input: {
  available: boolean;
  outcome: "verified" | "canceled" | "unavailable";
}): "enable" | "keep" | "unavailable" {
  if (!input.available || input.outcome === "unavailable") return "unavailable";
  if (input.outcome === "verified") return "enable";
  return "keep";
}
```

Turning the switch on calls `app_lock_availability`, then `app_lock_verify`, then `app_lock_enable` only when `nextLockEnabled` returns `"enable"`. A canceled prompt leaves the switch off. Turning it off calls `app_lock_disable` and does not clear the switch if the command returns `APP_LOCK_REQUIRED`.

Settings copy states that Windows Hello checks the Windows user, does not encrypt the database, and does not stop the `dbx` CLI.

- [ ] **Step 4: Re-run the helper test and typecheck**

Run: `pnpm exec vitest run apps/desktop/src/lib/startup/appLockSettings.spec.ts && pnpm exec vue-tsc --noEmit --project apps/desktop/tsconfig.json`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/lib/startup/appLockSettings.ts apps/desktop/src/lib/startup/appLockSettings.spec.ts apps/desktop/src/components/editor/EditorSettingsDialog.vue apps/desktop/src/lib/backend/tauri.ts apps/desktop/src/i18n
git commit -m "feat(desktop): opt in to Windows Hello from desktop settings"
```

---

### Task 8: Windows manual verification

This task is a checklist for a Windows machine. Linux CI cannot show the Hello dialog.

- [ ] Install Node and pnpm from `.nvmrc` / `package.json`, Rust stable with the MSVC target, Visual Studio C++ Build Tools, WebView2, and the Tauri prerequisites.
- [ ] `pnpm install --frozen-lockfile`
- [ ] `pnpm typecheck && pnpm build`
- [ ] `pnpm tauri build` and inspect the installer under `src-tauri/target/release/bundle`.
- [ ] Disabled flag: launch with no `app-lock.json`. Connections load and MCP starts as they do today. No Hello prompt.
- [ ] Enable: on a Windows user with Hello configured, the settings switch prompts once and writes `app-lock.json` with `{"enabled": true}`.
- [ ] Enable canceled: the file stays absent or `enabled: false`.
- [ ] Enable unavailable: a Windows user without Hello keeps the switch off and the file unchanged.
- [ ] Next launch: the native prompt appears before the connection list. Success loads the app.
- [ ] Cancel: the lock panel stays, `load_connections` invoked from devtools returns `APP_LOCK_REQUIRED`, and `migration_status` still returns.
- [ ] Retry then success loads the app. Quit exits.
- [ ] Restart after success prompts again. The process does not remember the previous unlock.
- [ ] Delete or disable Windows Hello in Windows Settings after the flag is on. The next launch stays locked and does not rewrite `app-lock.json` to `enabled: false`.
- [ ] Migration: a profile that still needs migration, with the flag on, can run `migration_start` while the lock panel is up, and completing it does not unlock the app.
- [ ] Before unlock, nothing listens on the MCP bridge port file, MCP HTTP is not bound, and no `--ui-backup-worker` child is running. After unlock, those start only if they were already enabled.
- [ ] `dbx` CLI can still open `dbx.db` while the desktop is locked. That is the documented limit, not a desktop bug.
- [ ] Check for updates while the lock panel is visible. The updater may run. It must not list connections.

- [ ] **Commit only if this task changes docs or tests. Do not commit a Windows installer.**

## Self-review

- Spec coverage: opt-in, availability plus prompt, launch prompt, cancel/unavailable, no silent disable, native IPC gate, separate flag file, no PIN, in-memory unlock, migration independence, MCP HTTP, MCP bridge, backup, Redis, deep links, updater, CLI boundary, and the Windows build checklist each have a task above.
- Explicit Lock and lock-on-hide are deferred on purpose.
- Vue renders the panel and calls native commands. The deny path is `guard_handler`.
- Placeholder scan: no TBD steps. Windows WinRT call details in Task 3 name the exact WinRT types; the HRESULT map is tested without a device.

## Out of scope follow-up

A later plan can gate `crates/dbx-cli` and standalone `dbx-mcp` by reading the same `app-lock.json` and refusing to open connections while it is enabled. That plan needs its own prompt story, because those processes do not own the desktop WebView. This plan does not implement it.

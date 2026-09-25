<script setup lang="ts">
import { defineAsyncComponent, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import { useI18n } from "vue-i18n";
import StartupLoading from "@/components/layout/StartupLoading.vue";
import { useMigrationStore } from "@/stores/migrationStore";
import { appLockStatus, appLockVerify, requestAppClose } from "@/lib/backend/api";
import { isTauriRuntime } from "@/lib/backend/tauriRuntime";
import { webPath } from "@/lib/common/webPath";
import { loadSavedLocale } from "@/i18n";
import { checkStartupAuthentication, type StartupAuthentication } from "@/lib/startup/startupAuthentication";
import { markStartupPhase } from "@/lib/startup/startupTiming";
import { retryStartupAfterPreloadFailure } from "@/lib/startup/startupPreloadRecovery";

// App setup installs listeners and instantiates business stores, so even its import
// is deferred until authentication and the security migration have completed.
const startupAsyncOptions = {
  loadingComponent: StartupLoading,
  errorComponent: StartupLoading,
  delay: 0,
  suspensible: false,
  onError(error: Error, _retry: () => void, fail: () => void) {
    retryStartupAfterPreloadFailure(error);
    fail();
  },
};
const App = defineAsyncComponent({ ...startupAsyncOptions, loader: () => import("./App.vue") });
const LoginPage = defineAsyncComponent({ ...startupAsyncOptions, loader: () => import("@/components/auth/LoginPage.vue") });
const SecurityMigrationWizard = defineAsyncComponent({ ...startupAsyncOptions, loader: () => import("@/components/migration/SecurityMigrationWizard.vue") });
const props = defineProps<{ localeReady?: Promise<void> }>();
const { t } = useI18n();
const migration = useMigrationStore();
const { blocking } = migration;
const checkingAuth = ref(true);
const appLocked = ref(false);
const appLockUnavailable = ref(false);
const loginRequired = ref(false);
const setupRequired = ref(false);
const authFailed = ref(false);
const startupAuthentication = shallowRef<StartupAuthentication>();
const checkingLocale = ref(Boolean(props.localeReady));
const localeFailed = ref(false);
let initialLocaleReady = props.localeReady;
let authRequest: AbortController | undefined;

watch(migration.blocking, (blocked) => {
  if (!blocked) markStartupPhase("migration-ready");
});

async function initializeLocale() {
  if (!initialLocaleReady && !localeFailed.value) return;
  checkingLocale.value = true;
  localeFailed.value = false;
  try {
    await (initialLocaleReady ?? loadSavedLocale());
    markStartupPhase("locale-ready");
    window.dispatchEvent(new Event("dbx:startup-ready"));
  } catch {
    localeFailed.value = true;
  } finally {
    initialLocaleReady = undefined;
    checkingLocale.value = false;
  }
}
async function resolveAppLock(signal: AbortSignal): Promise<boolean> {
  if (!isTauriRuntime()) return true;
  const status = await appLockStatus();
  if (signal.aborted) return false;
  if (!status.locked) {
    appLocked.value = false;
    appLockUnavailable.value = false;
    return true;
  }
  appLocked.value = true;
  const outcome = await appLockVerify();
  if (signal.aborted) return false;
  if (outcome !== "verified") {
    appLockUnavailable.value = outcome === "unavailable";
    return false;
  }
  const refreshed = await appLockStatus();
  if (signal.aborted) return false;
  appLocked.value = refreshed.locked;
  appLockUnavailable.value = false;
  return !refreshed.locked;
}
async function initialize() {
  authRequest?.abort();
  const request = new AbortController();
  authRequest = request;
  checkingAuth.value = true;
  authFailed.value = false;
  try {
    const unlocked = await resolveAppLock(request.signal);
    if (request.signal.aborted || !unlocked) return;
    if (!isTauriRuntime()) {
      const result = await checkStartupAuthentication(request.signal);
      if (request.signal.aborted) return;
      startupAuthentication.value = result;
      setupRequired.value = result.setup_required === true;
      loginRequired.value = setupRequired.value || (result.required && !result.authenticated);
      if (loginRequired.value) {
        history.replaceState(null, "", webPath("/login"));
        return;
      }
    }
    markStartupPhase("auth-ready");
    await migration.initialize();
  } catch {
    if (!request.signal.aborted) authFailed.value = true;
  } finally {
    if (!request.signal.aborted) checkingAuth.value = false;
  }
}
async function authenticated() {
  history.replaceState(null, "", webPath("/"));
  await initialize();
}
async function retryUnlock() {
  await initialize();
}
async function quitFromLock() {
  await requestAppClose();
}
onMounted(() => {
  void initializeLocale();
  void initialize();
});
onUnmounted(() => authRequest?.abort());
</script>
<template>
  <StartupLoading v-if="checkingLocale || localeFailed || checkingAuth || authFailed" :label="authFailed ? t('migration.authFailed') : !checkingLocale && !localeFailed ? t('migration.checking') : undefined" :error="localeFailed || authFailed" :retry="localeFailed ? initializeLocale : initialize" />
  <LoginPage v-else-if="loginRequired" :setup-mode="setupRequired" @authenticated="authenticated" />
  <div v-else-if="appLocked" data-app-lock>
    <p>{{ t("appLock.title") }}</p>
    <p v-if="appLockUnavailable">{{ t("appLock.unavailable") }}</p>
    <button type="button" data-app-lock-retry @click="retryUnlock">{{ t("appLock.retry") }}</button>
    <button type="button" data-app-lock-quit @click="quitFromLock">{{ t("appLock.quit") }}</button>
  </div>
  <SecurityMigrationWizard v-else-if="blocking" :store="migration" />
  <App v-else :startup-authentication="startupAuthentication" />
</template>

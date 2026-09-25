use std::collections::HashSet;
use std::sync::Mutex;

const CONNECTION_DEEP_LINK_PREFIX: &str = "dbx://connection/new";
const AI_CONFIG_DEEP_LINK_PREFIX: &str = "dbx://settings/ai/new";
const PLUGIN_INSTALL_DEEP_LINK_PREFIX: &str = "dbx://plugins/install";
const APP_OPEN_DEEP_LINK_PREFIX: &str = "dbx://open";

#[tauri::command]
pub fn pending_open_connection_links(state: tauri::State<'_, DeepLinkOpenState>) -> Vec<String> {
    dedupe_links(state.drain_connection_links())
}

#[tauri::command]
pub fn pending_open_ai_config_links(state: tauri::State<'_, DeepLinkOpenState>) -> Vec<String> {
    dedupe_links(state.drain_ai_config_links())
}

#[tauri::command]
pub fn pending_open_plugin_install_links(state: tauri::State<'_, DeepLinkOpenState>) -> Vec<String> {
    dedupe_links(state.drain_plugin_install_links())
}

#[derive(Default)]
struct DeepLinkQueues {
    connection: Vec<String>,
    ai_config: Vec<String>,
    plugin_install: Vec<String>,
    released: bool,
}

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct QueuedDeepLinks {
    pub connection: Vec<String>,
    pub ai_config: Vec<String>,
    pub plugin_install: Vec<String>,
}

enum DeepLinkKind {
    Connection,
    AiConfig,
    PluginInstall,
}

#[derive(Default)]
pub struct DeepLinkOpenState {
    queues: Mutex<DeepLinkQueues>,
}

impl DeepLinkOpenState {
    fn lock(&self) -> std::sync::MutexGuard<'_, DeepLinkQueues> {
        self.queues.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn push_connection_links(&self, links: Vec<String>) {
        let _ = self.stage(DeepLinkKind::Connection, links, true);
    }

    pub fn push_ai_config_links(&self, links: Vec<String>) {
        let _ = self.stage(DeepLinkKind::AiConfig, links, true);
    }

    pub fn push_plugin_install_links(&self, links: Vec<String>) {
        let _ = self.stage(DeepLinkKind::PluginInstall, links, true);
    }

    pub(crate) fn stage_connection_links(&self, links: Vec<String>, locked: bool) -> QueuedDeepLinks {
        self.stage(DeepLinkKind::Connection, links, locked)
    }

    pub(crate) fn stage_ai_config_links(&self, links: Vec<String>, locked: bool) -> QueuedDeepLinks {
        self.stage(DeepLinkKind::AiConfig, links, locked)
    }

    pub(crate) fn stage_plugin_install_links(&self, links: Vec<String>, locked: bool) -> QueuedDeepLinks {
        self.stage(DeepLinkKind::PluginInstall, links, locked)
    }

    pub(crate) fn release_queued_emits(&self) -> QueuedDeepLinks {
        let mut queues = self.lock();
        if queues.released {
            return QueuedDeepLinks::default();
        }
        queues.released = true;
        Self::snapshot(&queues)
    }

    fn stage(&self, kind: DeepLinkKind, links: Vec<String>, locked: bool) -> QueuedDeepLinks {
        if links.is_empty() {
            return QueuedDeepLinks::default();
        }
        let mut queues = self.lock();
        match kind {
            DeepLinkKind::Connection => queues.connection.extend(links.iter().cloned()),
            DeepLinkKind::AiConfig => queues.ai_config.extend(links.iter().cloned()),
            DeepLinkKind::PluginInstall => queues.plugin_install.extend(links.iter().cloned()),
        }
        if locked && !queues.released {
            return QueuedDeepLinks::default();
        }
        if !queues.released {
            queues.released = true;
            return Self::snapshot(&queues);
        }
        match kind {
            DeepLinkKind::Connection => QueuedDeepLinks { connection: links, ..QueuedDeepLinks::default() },
            DeepLinkKind::AiConfig => QueuedDeepLinks { ai_config: links, ..QueuedDeepLinks::default() },
            DeepLinkKind::PluginInstall => QueuedDeepLinks { plugin_install: links, ..QueuedDeepLinks::default() },
        }
    }

    fn snapshot(queues: &DeepLinkQueues) -> QueuedDeepLinks {
        QueuedDeepLinks {
            connection: queues.connection.clone(),
            ai_config: queues.ai_config.clone(),
            plugin_install: queues.plugin_install.clone(),
        }
    }

    fn drain_connection_links(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().connection)
    }

    fn drain_ai_config_links(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().ai_config)
    }

    fn drain_plugin_install_links(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().plugin_install)
    }
}

pub fn connection_deep_links_from_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().filter_map(|arg| connection_deep_link_from_arg(arg.as_ref())).collect()
}

pub fn connection_deep_link_from_arg(arg: &str) -> Option<String> {
    let trimmed = arg.trim();
    matches_deep_link_target(trimmed, CONNECTION_DEEP_LINK_PREFIX).then(|| trimmed.to_string())
}

pub fn ai_config_deep_links_from_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().filter_map(|arg| ai_config_deep_link_from_arg(arg.as_ref())).collect()
}

pub fn ai_config_deep_link_from_arg(arg: &str) -> Option<String> {
    let trimmed = arg.trim();
    matches_deep_link_target(trimmed, AI_CONFIG_DEEP_LINK_PREFIX).then(|| trimmed.to_string())
}

pub fn plugin_install_deep_links_from_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().filter_map(|arg| plugin_install_deep_link_from_arg(arg.as_ref())).collect()
}

pub fn plugin_install_deep_link_from_arg(arg: &str) -> Option<String> {
    let trimmed = arg.trim();
    matches_deep_link_target(trimmed, PLUGIN_INSTALL_DEEP_LINK_PREFIX).then(|| trimmed.to_string())
}

pub fn is_app_open_deep_link(arg: &str) -> bool {
    matches_deep_link_target(arg.trim(), APP_OPEN_DEEP_LINK_PREFIX)
}

fn matches_deep_link_target(value: &str, target: &str) -> bool {
    let Some(suffix) = value.strip_prefix(target) else {
        return false;
    };
    suffix.is_empty()
        || suffix.starts_with('?')
        || suffix.starts_with('#')
        || suffix == "/"
        || suffix.starts_with("/?")
        || suffix.starts_with("/#")
}

fn dedupe_links(links: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for link in links {
        if seen.insert(link.clone()) {
            unique.push(link);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_connection_deep_links() {
        let links = connection_deep_links_from_args([
            "dbx://connection/new?type=mysql&host=127.0.0.1",
            "dbx://settings/ai/new?provider=openai-compatible",
            "--flag",
            "dbx://open?x=1",
            "dbx://connections/new?type=postgres",
            "dbx://connection/newer?type=mysql",
        ]);

        assert_eq!(links, vec!["dbx://connection/new?type=mysql&host=127.0.0.1".to_string()]);
    }

    #[test]
    fn filters_ai_config_deep_links() {
        let links = ai_config_deep_links_from_args([
            "dbx://settings/ai/new?v=1&provider=openai-compatible",
            "--flag",
            "dbx://settings/ai/edit?provider=openai-compatible",
            "dbx://settings/ai/newer?provider=openai-compatible",
            "dbx://connection/new?type=mysql",
        ]);

        assert_eq!(links, vec!["dbx://settings/ai/new?v=1&provider=openai-compatible".to_string()]);
    }

    #[test]
    fn filters_plugin_install_deep_links() {
        let links = plugin_install_deep_links_from_args([
            "dbx://plugins/install?url=https%3A%2F%2Fdl.dbxio.com%2Fplugins%2Fio.dbx.ssh%2F0.4.73%2Fio.dbx.ssh-0.4.73-darwin-arm64.dbxp",
            "--flag",
            "dbx://plugins/installed?url=https://example.com/plugin.dbxp",
            "dbx://plugins/installation?url=https://example.com/plugin.dbxp",
            "dbx://connection/new?type=mysql",
        ]);

        assert_eq!(
            links,
            vec!["dbx://plugins/install?url=https%3A%2F%2Fdl.dbxio.com%2Fplugins%2Fio.dbx.ssh%2F0.4.73%2Fio.dbx.ssh-0.4.73-darwin-arm64.dbxp".to_string()]
        );
    }

    #[test]
    fn recognizes_app_open_deep_links() {
        assert!(is_app_open_deep_link("dbx://open"));
        assert!(is_app_open_deep_link(" dbx://open?source=sponsor "));
        assert!(is_app_open_deep_link("dbx://open/#landing"));
        assert!(!is_app_open_deep_link("dbx://opened"));
        assert!(!is_app_open_deep_link("dbx://open/window"));
        assert!(!is_app_open_deep_link("dbx://connection/new"));
    }

    #[test]
    fn drains_pending_links_once() {
        let state = DeepLinkOpenState::default();
        state.push_connection_links(vec!["dbx://connection/new?type=mysql".to_string()]);
        state.push_ai_config_links(vec!["dbx://settings/ai/new?provider=openai-compatible".to_string()]);
        state.push_plugin_install_links(vec!["dbx://plugins/install?url=https://example.com/plugin.dbxp".to_string()]);

        assert_eq!(state.drain_connection_links(), vec!["dbx://connection/new?type=mysql"]);
        assert_eq!(state.drain_ai_config_links(), vec!["dbx://settings/ai/new?provider=openai-compatible"]);
        assert_eq!(
            state.drain_plugin_install_links(),
            vec!["dbx://plugins/install?url=https://example.com/plugin.dbxp"]
        );
        assert!(state.drain_connection_links().is_empty());
        assert!(state.drain_ai_config_links().is_empty());
        assert!(state.drain_plugin_install_links().is_empty());
    }

    #[test]
    fn dedupes_links_while_preserving_order() {
        assert_eq!(
            dedupe_links(vec![
                "dbx://connection/new?type=mysql".to_string(),
                "dbx://connection/new?type=postgres".to_string(),
                "dbx://connection/new?type=mysql".to_string(),
            ]),
            vec!["dbx://connection/new?type=mysql", "dbx://connection/new?type=postgres"]
        );
    }

    #[test]
    fn holds_events_until_release_then_emits_each_link_once() {
        let state = DeepLinkOpenState::default();
        let held = state.stage_connection_links(vec!["dbx://connection/new?type=mysql".to_string()], true);
        assert_eq!(held, QueuedDeepLinks::default());
        state.push_ai_config_links(vec!["dbx://settings/ai/new?provider=openai-compatible".to_string()]);

        let queued = state.release_queued_emits();
        assert_eq!(queued.connection, vec!["dbx://connection/new?type=mysql"]);
        assert_eq!(queued.ai_config, vec!["dbx://settings/ai/new?provider=openai-compatible"]);
        assert!(state.release_queued_emits().connection.is_empty());

        let live = state.stage_connection_links(vec!["dbx://connection/new?type=postgres".to_string()], false);
        assert_eq!(live.connection, vec!["dbx://connection/new?type=postgres"]);
        assert!(live.ai_config.is_empty());
        assert_eq!(
            state.drain_connection_links(),
            vec!["dbx://connection/new?type=mysql".to_string(), "dbx://connection/new?type=postgres".to_string()]
        );
    }
}

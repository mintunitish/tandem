//! Maps (project root, server) to running clients and aggregates diagnostics.

use crate::{LanguageServerConfig, LspClient, LspError, Result, ServerNotification, builtin_servers};
use crate::client::ClientOptions;
use lsp_types::Diagnostic;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::OnceCell;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientKey {
    pub root: PathBuf,
    pub server_id: String,
}

#[derive(Clone, Debug)]
pub enum ManagerEvent {
    /// Diagnostics for `path` changed; read them with [`LspManager::diagnostics`].
    DiagnosticsChanged { path: PathBuf },
    /// Any notification from a server (progress, logs, lifecycle).
    Server { key: ClientKey, notification: ServerNotification },
}

type DiagMap = HashMap<PathBuf, HashMap<ClientKey, Vec<Diagnostic>>>;

struct Shared {
    diagnostics: RwLock<DiagMap>,
    subscribers: Mutex<Vec<async_channel::Sender<ManagerEvent>>>,
}

impl Shared {
    fn emit(&self, ev: ManagerEvent) {
        self.subscribers.lock().retain(|s| s.try_send(ev.clone()).is_ok());
    }
}

pub struct LspManager {
    workspace_root: PathBuf,
    configs: Vec<LanguageServerConfig>,
    options: ClientOptions,
    clients: Mutex<HashMap<ClientKey, Arc<OnceCell<Arc<LspClient>>>>>,
    shared: Arc<Shared>,
}

impl LspManager {
    /// Manager with the built-in server registry.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self::with_configs(workspace_root, builtin_servers(), ClientOptions::default())
    }

    pub fn with_configs(workspace_root: impl Into<PathBuf>, configs: Vec<LanguageServerConfig>, options: ClientOptions) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            configs,
            options,
            clients: Mutex::new(HashMap::new()),
            shared: Arc::new(Shared { diagnostics: RwLock::new(HashMap::new()), subscribers: Mutex::new(vec![]) }),
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn config_for(&self, path: &Path) -> Option<&LanguageServerConfig> {
        self.configs.iter().find(|c| c.handles(path))
    }

    /// LSP `languageId` for `path`, if any configured server handles it.
    pub fn language_id_for(&self, path: &Path) -> Option<&str> {
        self.config_for(path).and_then(|c| c.language_id_for(path))
    }

    /// Key that `path` maps to: nearest ancestor with one of the server's root markers
    /// (stopping at the workspace root), else the workspace root.
    pub fn key_for(&self, path: &Path) -> Option<ClientKey> {
        let config = self.config_for(path)?;
        let root = find_root(path, &self.workspace_root, &config.root_markers);
        Some(ClientKey { root, server_id: config.id.clone() })
    }

    /// Already-running client for `path` (never starts one).
    pub fn client_for(&self, path: &Path) -> Option<Arc<LspClient>> {
        let key = self.key_for(path)?;
        self.clients.lock().get(&key).and_then(|c| c.get().cloned()).filter(|c| !c.is_dead())
    }

    /// Running client for `path`, starting the server if needed. `Ok(None)` when no
    /// configured server handles the file; `Err(NotInstalled)` when it is missing.
    /// Concurrent callers for the same key share one start.
    pub async fn ensure_client(&self, path: &Path) -> Result<Option<Arc<LspClient>>> {
        let Some(key) = self.key_for(path) else { return Ok(None) };
        let Some(config) = self.config_for(path).cloned() else { return Ok(None) };
        let cell = {
            let mut clients = self.clients.lock();
            if clients.get(&key).and_then(|c| c.get()).is_some_and(|c| c.is_dead()) {
                clients.remove(&key); // crashed for good: start a fresh one
            }
            clients.entry(key.clone()).or_default().clone()
        };
        let client = cell
            .get_or_try_init(|| async {
                let client = LspClient::start_with_options(config, &key.root, self.options.clone()).await?;
                self.attach(key.clone(), &client);
                Ok::<_, LspError>(Arc::new(client))
            })
            .await?;
        Ok(Some(client.clone()))
    }

    fn attach(&self, key: ClientKey, client: &LspClient) {
        let rx = client.subscribe();
        let shared = Arc::downgrade(&self.shared);
        tokio::spawn(async move {
            while let Ok(n) = rx.recv().await {
                let Some(shared) = shared.upgrade() else { return };
                match &n {
                    ServerNotification::Diagnostics { path: Some(path), diagnostics, .. } => {
                        {
                            let mut map = shared.diagnostics.write();
                            let per_file = map.entry(path.clone()).or_default();
                            if diagnostics.is_empty() {
                                per_file.remove(&key);
                            } else {
                                per_file.insert(key.clone(), diagnostics.clone());
                            }
                            if per_file.is_empty() {
                                map.remove(path);
                            }
                        }
                        shared.emit(ManagerEvent::DiagnosticsChanged { path: path.clone() });
                    }
                    ServerNotification::Exited { restarting: false, .. } => {
                        clear_server(&shared, &key);
                    }
                    _ => {}
                }
                shared.emit(ManagerEvent::Server { key: key.clone(), notification: n });
            }
        });
    }

    /// All diagnostics for `path`, merged across servers.
    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.shared
            .diagnostics
            .read()
            .get(path)
            .map(|m| m.values().flatten().cloned().collect())
            .unwrap_or_default()
    }

    pub fn all_diagnostics(&self) -> HashMap<PathBuf, Vec<Diagnostic>> {
        self.shared
            .diagnostics
            .read()
            .iter()
            .map(|(p, m)| (p.clone(), m.values().flatten().cloned().collect()))
            .collect()
    }

    /// Broadcast stream of diagnostics changes and server notifications.
    pub fn subscribe(&self) -> async_channel::Receiver<ManagerEvent> {
        let (tx, rx) = async_channel::unbounded();
        self.shared.subscribers.lock().push(tx);
        rx
    }

    pub fn running(&self) -> Vec<(ClientKey, Arc<LspClient>)> {
        self.clients
            .lock()
            .iter()
            .filter_map(|(k, c)| c.get().map(|c| (k.clone(), c.clone())))
            .collect()
    }

    /// Shut down one server and drop its diagnostics.
    pub async fn stop(&self, key: &ClientKey) -> Result<()> {
        let cell = self.clients.lock().remove(key);
        if let Some(client) = cell.and_then(|c| c.get().cloned()) {
            client.shutdown().await?;
        }
        clear_server(&self.shared, key);
        Ok(())
    }

    pub async fn shutdown_all(&self) {
        let keys: Vec<_> = self.clients.lock().keys().cloned().collect();
        for key in keys {
            if let Err(e) = self.stop(&key).await {
                tracing::warn!("stopping {key:?}: {e}");
            }
        }
    }
}

fn clear_server(shared: &Shared, key: &ClientKey) {
    let mut changed = vec![];
    {
        let mut map = shared.diagnostics.write();
        map.retain(|path, per| {
            if per.remove(key).is_some() {
                changed.push(path.clone());
            }
            !per.is_empty()
        });
    }
    for path in changed {
        shared.emit(ManagerEvent::DiagnosticsChanged { path });
    }
}

fn find_root(path: &Path, workspace_root: &Path, markers: &[String]) -> PathBuf {
    if !markers.is_empty() {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if markers.iter().any(|m| d.join(m).exists()) {
                return d.to_path_buf();
            }
            if d == workspace_root {
                break;
            }
            dir = d.parent();
        }
    }
    if path.starts_with(workspace_root) {
        workspace_root.to_path_buf()
    } else {
        path.parent().map_or_else(|| workspace_root.to_path_buf(), Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_resolution() {
        let dir = tempfile::tempdir().expect("tmp");
        let ws = dir.path();
        std::fs::create_dir_all(ws.join("web/src")).expect("mk");
        std::fs::write(ws.join("web/package.json"), "{}").expect("w");
        std::fs::create_dir_all(ws.join("py")).expect("mk");
        let m = LspManager::new(ws);
        let k = m.key_for(&ws.join("web/src/a.ts")).expect("key");
        assert_eq!(k, ClientKey { root: ws.join("web"), server_id: "typescript".into() });
        let k = m.key_for(&ws.join("py/a.py")).expect("key");
        assert_eq!(k.root, ws.to_path_buf());
        assert!(m.key_for(&ws.join("x.unknown")).is_none());
        assert_eq!(m.language_id_for(&ws.join("a.jsx")), Some("javascriptreact"));
        assert!(m.client_for(&ws.join("web/src/a.ts")).is_none());
    }
}

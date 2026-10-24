//! Editor ↔ `tandem-lsp`: one `LspHub` per window owns the `LspManager`, keeps open documents in
//! sync (debounced full-text `didChange`: keystrokes only hand over a rope snapshot, the text is
//! built and hashed when the debounce fires), collects diagnostics and progress, and hands out the
//! gpui-kit provider objects (`CompletionProvider`, `HoverProvider`, `DefinitionProvider`,
//! `CodeActionProvider`) that the editor calls.
//!
//! All LSP I/O runs on the shared tokio runtime (`crate::runtime`); results come back over
//! `async_channel` receivers awaited from gpui tasks.

use super::pane::EditorArea;
use crate::runtime;
use anyhow::{Result, anyhow};
use gpui_kit::component::input::{CodeActionProvider, CompletionProvider, DefinitionProvider, EditorState, HoverProvider, Rope};
use gpui_kit::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tandem_lsp::lsp_types::{self as lsp, CodeAction, CodeActionOrCommand, CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, InsertTextFormat};
use tandem_lsp::{ChangeDebouncer, ClientKey, LspClient, LspError, LspManager, ManagerEvent, ServerNotification};

/// Completion items shown at most.
const MAX_COMPLETIONS: usize = 150;
/// `didChange` is sent this long after the last edit...
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(50);
/// ...or this long after the first edit of a continuous burst.
const CHANGE_MAX_WAIT: Duration = Duration::from_millis(500);

struct Doc {
    client: Arc<LspClient>,
    debouncer: ChangeDebouncer,
    version: i32,
    text_hash: u64,
    /// Latest unsynced buffer snapshot (cheap rope clone) and when the burst started.
    pending: Option<(Rope, Instant)>,
    flush: Task<()>,
}

pub enum LspHubEvent {
    /// Diagnostics for this file changed (read them from `LspHub::diagnostics`).
    Diagnostics(PathBuf),
    /// Server started/stopped/progress/missing-server hint changed.
    Status,
}

pub struct LspHub {
    pub manager: Arc<LspManager>,
    docs: HashMap<PathBuf, Doc>,
    debouncers: HashMap<ClientKey, ChangeDebouncer>,
    starting: HashSet<PathBuf>,
    /// Latest diagnostics per absolute path (only non-empty entries).
    pub diagnostics: BTreeMap<PathBuf, Vec<lsp::Diagnostic>>,
    /// Active `$/progress` tokens (server is indexing).
    progress: HashSet<String>,
    /// `(server name, install hint)` of the last server that was needed but not installed.
    pub missing: Option<(String, String)>,
    _events: Task<()>,
}

impl EventEmitter<LspHubEvent> for LspHub {}

fn hash_text(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl LspHub {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let manager = Arc::new(LspManager::new(root));
        let rx = manager.subscribe();
        let events = cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let alive = this.update(cx, |hub, cx| hub.on_manager_event(ev, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            manager,
            docs: HashMap::new(),
            debouncers: HashMap::new(),
            starting: HashSet::new(),
            diagnostics: BTreeMap::new(),
            progress: HashSet::new(),
            missing: None,
            _events: events,
        }
    }

    fn on_manager_event(&mut self, ev: ManagerEvent, cx: &mut Context<Self>) {
        match ev {
            ManagerEvent::DiagnosticsChanged { path } => {
                let diags = self.manager.diagnostics(&path);
                if diags.is_empty() {
                    self.diagnostics.remove(&path);
                } else {
                    self.diagnostics.insert(path.clone(), diags);
                }
                cx.emit(LspHubEvent::Diagnostics(path));
            }
            ManagerEvent::Server { notification, key } => match notification {
                ServerNotification::Progress(p) => {
                    let token = format!("{}:{:?}", key.server_id, p.token);
                    let done = matches!(
                        p.value,
                        lsp::ProgressParamsValue::WorkDone(lsp::WorkDoneProgress::End(_))
                    );
                    let changed = if done { self.progress.remove(&token) } else { self.progress.insert(token) };
                    if changed {
                        cx.emit(LspHubEvent::Status);
                    }
                }
                ServerNotification::Exited { restarting: false, .. } => {
                    self.docs.retain(|_, d| !d.client.is_dead());
                    self.debouncers.remove(&key);
                    cx.emit(LspHubEvent::Status);
                }
                _ => {}
            },
        }
        cx.notify();
    }

    /// Status-bar text: idle / indexing / N servers.
    pub fn status_text(&self) -> SharedString {
        let running = self.manager.running().into_iter().filter(|(_, c)| !c.is_dead()).count();
        if !self.progress.is_empty() || !self.starting.is_empty() {
            "LSP: indexing…".into()
        } else if running == 0 {
            "LSP: idle".into()
        } else if running == 1 {
            "LSP: 1 server".into()
        } else {
            format!("LSP: {running} servers").into()
        }
    }

    /// Start (or reuse) the server for `path` and `didOpen` it. Silently does nothing for files
    /// no server handles; records an install hint when the server is not installed.
    pub fn open(&mut self, path: &Path, state: WeakEntity<EditorState>, cx: &mut Context<Self>) {
        if self.docs.contains_key(path) || self.starting.contains(path) {
            return;
        }
        let Some(config) = self.manager.config_for(path).cloned() else { return };
        if !config.is_installed() {
            self.missing = Some((config.name.clone(), config.install_hint.clone()));
            cx.emit(LspHubEvent::Status);
            cx.notify();
            return;
        }
        let language_id = config.language_id_for(path).unwrap_or("plaintext").to_string();
        self.starting.insert(path.to_path_buf());
        cx.emit(LspHubEvent::Status);
        let manager = self.manager.clone();
        let p = path.to_path_buf();
        let rx = runtime::spawn_bridge(async move { manager.ensure_client(&p).await });
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let res = rx.recv().await.unwrap_or(Err(LspError::ServerExited));
            let _ = this.update(cx, |hub, cx| {
                hub.starting.remove(&path);
                match res {
                    Ok(Some(client)) => {
                        let text = state.upgrade().map(|s| s.read(cx).value().to_string());
                        if let Some(text) = text {
                            hub.attach(path.clone(), client, &language_id, text);
                        }
                    }
                    Ok(None) => {}
                    Err(LspError::NotInstalled { server, hint }) => {
                        hub.missing = Some((server, hint));
                    }
                    Err(err) => {
                        hub.missing = Some((config.name.clone(), format!("failed to start: {err}")));
                    }
                }
                cx.emit(LspHubEvent::Status);
                cx.notify();
            });
        })
        .detach();
    }

    fn attach(&mut self, path: PathBuf, client: Arc<LspClient>, language_id: &str, text: String) {
        let _rt = runtime::tokio().enter();
        let key = ClientKey { root: client.root().to_path_buf(), server_id: client.config().id.clone() };
        // The hub debounces (see `change`); the shared debouncer only serializes and flushes.
        let debouncer =
            self.debouncers.entry(key).or_insert_with(|| ChangeDebouncer::with_delay((*client).clone(), Duration::ZERO, Duration::ZERO)).clone();
        let _ = client.did_open(&path, language_id, &text);
        self.docs.insert(path, Doc { client, debouncer, version: 0, text_hash: hash_text(&text), pending: None, flush: Task::ready(()) });
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.docs.contains_key(path)
    }

    /// Record an edit: keeps the snapshot and (re)arms the debounce timer.
    pub fn change(&mut self, path: &Path, text: Rope, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else { return };
        let since = doc.pending.as_ref().map_or_else(Instant::now, |p| p.1);
        doc.pending = Some((text, since));
        let delay = CHANGE_DEBOUNCE.min(CHANGE_MAX_WAIT.saturating_sub(since.elapsed()));
        let p = path.to_path_buf();
        doc.flush = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |hub, _| hub.flush_pending(&p));
        });
    }

    /// Send the pending snapshot now (if any).
    fn flush_pending(&mut self, path: &Path) {
        let Some((rope, _)) = self.docs.get_mut(path).and_then(|d| d.pending.take()) else { return };
        self.sync_text(path, &rope.to_string());
    }

    /// Queue a full-text `didChange` of `text`. No-op when the text did not change.
    fn sync_text(&mut self, path: &Path, text: &str) {
        let Some(doc) = self.docs.get_mut(path) else { return };
        doc.pending = None;
        doc.flush = Task::ready(());
        let h = hash_text(text);
        if h == doc.text_hash {
            return;
        }
        doc.text_hash = h;
        doc.version += 1;
        doc.debouncer.push(path, doc.version, text.to_string());
    }

    /// Flush pending changes, then `didSave`.
    pub fn save(&mut self, path: &Path) {
        self.flush_pending(path);
        let Some(doc) = self.docs.get(path) else { return };
        let (client, deb, p) = (doc.client.clone(), doc.debouncer.clone(), path.to_path_buf());
        runtime::tokio().spawn(async move {
            deb.flush().await;
            let _ = client.did_save(&p);
        });
    }

    pub fn close(&mut self, path: &Path) {
        self.flush_pending(path);
        if let Some(doc) = self.docs.remove(path) {
            let p = path.to_path_buf();
            runtime::tokio().spawn(async move {
                doc.debouncer.flush().await;
                let _ = doc.client.did_close(&p);
            });
        }
    }

    pub fn client(&self, path: &Path) -> Option<Arc<LspClient>> {
        self.docs.get(path).map(|d| d.client.clone())
    }

    /// Run `f` against the file's server after syncing `text` (if given) and flushing pending
    /// changes. `None` when no server is attached to the file.
    pub fn request<T, F, Fut>(&mut self, path: &Path, text: Option<&str>, f: F) -> Option<async_channel::Receiver<T>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<LspClient>, PathBuf) -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        match text {
            Some(text) => self.sync_text(path, text),
            None => self.flush_pending(path),
        }
        let doc = self.docs.get(path)?;
        let (client, deb, p) = (doc.client.clone(), doc.debouncer.clone(), path.to_path_buf());
        Some(runtime::spawn_bridge(async move {
            deb.flush().await;
            f(client, p).await
        }))
    }
}

/// Await a bridged LSP result, mapping a closed channel to an error.
pub async fn recv<T>(rx: async_channel::Receiver<Result<T, LspError>>) -> Result<T> {
    match rx.recv().await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(anyhow!(e)),
        Err(_) => Err(anyhow!("language server went away")),
    }
}

/// Byte offset → LSP (UTF-16) position.
pub fn position(text: &str, offset: usize) -> lsp::Position {
    tandem_lsp::offset_to_position(text, offset.min(text.len()))
}

/// Byte range of the identifier that ends at `offset` (the completion prefix).
pub fn word_prefix(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '$')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(offset);
    start..offset
}

/// Strip snippet placeholders: `${1:name}` → `name`, `$0`/`$1` → ``.
pub fn strip_snippet(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            out.push(b[i + 1] as char);
            i += 2;
        } else if b[i] == b'$' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        } else if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
            i += 2;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && b[i] == b':' {
                i += 1;
            }
            let start = i;
            let mut depth = 1;
            while i < b.len() {
                match b[i] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            out.push_str(&strip_snippet(&s[start..i.min(s.len())]));
            i += 1;
        } else {
            let ch = s[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Filter + normalize server items for the gpui-kit menu, which neither filters nor replaces the
/// typed prefix by itself: every item gets an explicit `text_edit` over the prefix.
pub fn prepare_completions(items: Vec<CompletionItem>, text: &str, offset: usize) -> Vec<CompletionItem> {
    let prefix_range = word_prefix(text, offset);
    let prefix = text[prefix_range.clone()].to_lowercase();
    let replace = lsp::Range::new(position(text, prefix_range.start), position(text, offset));
    let mut scored: Vec<(i32, CompletionItem)> = items
        .into_iter()
        .filter_map(|mut item| {
            let key = item.filter_text.clone().unwrap_or_else(|| item.label.clone()).to_lowercase();
            let score = if prefix.is_empty() {
                0
            } else if key.starts_with(&prefix) {
                100
            } else if key.contains(&prefix) {
                50
            } else if is_subsequence(&prefix, &key) {
                10
            } else {
                return None;
            };
            let snippet = item.insert_text_format == Some(InsertTextFormat::SNIPPET);
            match item.text_edit.take() {
                Some(CompletionTextEdit::Edit(mut e)) => {
                    if snippet {
                        e.new_text = strip_snippet(&e.new_text);
                    }
                    item.text_edit = Some(CompletionTextEdit::Edit(e));
                }
                Some(CompletionTextEdit::InsertAndReplace(e)) => {
                    let new_text = if snippet { strip_snippet(&e.new_text) } else { e.new_text };
                    item.text_edit = Some(CompletionTextEdit::Edit(lsp::TextEdit::new(e.replace, new_text)));
                }
                None => {
                    let raw = item.insert_text.clone().unwrap_or_else(|| item.label.clone());
                    let new_text = if snippet { strip_snippet(&raw) } else { raw };
                    item.text_edit = Some(CompletionTextEdit::Edit(lsp::TextEdit::new(replace, new_text)));
                }
            }
            item.insert_text = None;
            let sort = item.sort_text.clone().unwrap_or_default();
            Some((score, item)).map(|(s, i)| (s - (sort.len() as i32).min(5), i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.sort_text.cmp(&b.1.sort_text)).then_with(|| a.1.label.cmp(&b.1.label)));
    scored.truncate(MAX_COMPLETIONS);
    scored.into_iter().map(|(_, i)| i).collect()
}

fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

/// The provider object installed on every `EditorState` whose file has a server.
pub struct LspProviders {
    pub hub: Entity<LspHub>,
    pub path: PathBuf,
    pub area: WeakEntity<EditorArea>,
}

impl LspProviders {
    fn request<T, F, Fut>(&self, text: Option<&str>, cx: &mut App, f: F) -> Option<async_channel::Receiver<T>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<LspClient>, PathBuf) -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let path = self.path.clone();
        self.hub.update(cx, |hub, _| hub.request(&path, text, f))
    }
}

impl CompletionProvider for LspProviders {
    fn completions(&self, rope: &Rope, offset: usize, _trigger: CompletionContext, _: &mut Window, cx: &mut App) -> Task<Result<CompletionResponse>> {
        let text = rope.to_string();
        let pos = position(&text, offset);
        let Some(rx) = self.request(Some(&text), cx, move |c, p| async move { c.completion(&p, pos).await }) else {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        };
        cx.background_spawn(async move {
            let items = recv(rx).await.unwrap_or_default();
            Ok(CompletionResponse::Array(prepare_completions(items, &text, offset)))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text.chars().last().is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '<' | '"' | '\'' | '/' | '@' | '#' | '$'))
    }
}

impl HoverProvider for LspProviders {
    fn hover(&self, rope: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Option<lsp::Hover>>> {
        let text = rope.to_string();
        let pos = position(&text, offset);
        let Some(rx) = self.request(Some(&text), cx, move |c, p| async move { c.hover(&p, pos).await }) else {
            return Task::ready(Ok(None));
        };
        cx.background_spawn(async move { Ok(recv(rx).await.ok().flatten()) })
    }
}

impl DefinitionProvider for LspProviders {
    fn definitions(&self, rope: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Vec<lsp::LocationLink>>> {
        let text = rope.to_string();
        let pos = position(&text, offset);
        let Some(rx) = self.request(Some(&text), cx, move |c, p| async move { c.definition(&p, pos).await }) else {
            return Task::ready(Ok(vec![]));
        };
        cx.background_spawn(async move {
            let locs = recv(rx).await.unwrap_or_default();
            Ok(locs
                .into_iter()
                .map(|l| lsp::LocationLink { origin_selection_range: None, target_uri: l.uri, target_range: l.range, target_selection_range: l.range })
                .collect())
        })
    }
}

impl CodeActionProvider for LspProviders {
    fn id(&self) -> SharedString {
        "tandem-lsp".into()
    }

    fn code_actions(&self, state: Entity<EditorState>, range: Range<usize>, _: &mut Window, cx: &mut App) -> Task<Result<Vec<CodeAction>>> {
        let text = state.read(cx).value().to_string();
        let lsp_range = lsp::Range::new(position(&text, range.start), position(&text, range.end));
        let diags: Vec<lsp::Diagnostic> = self
            .hub
            .read(cx)
            .diagnostics
            .get(&self.path)
            .map(|d| d.iter().filter(|d| d.range.start.line <= lsp_range.end.line && d.range.end.line >= lsp_range.start.line).cloned().collect())
            .unwrap_or_default();
        let Some(rx) = self.request(Some(&text), cx, move |c, p| async move { c.code_actions(&p, lsp_range, diags).await }) else {
            return Task::ready(Ok(vec![]));
        };
        cx.background_spawn(async move {
            let items = recv(rx).await.unwrap_or_default();
            Ok(items
                .into_iter()
                .map(|a| match a {
                    CodeActionOrCommand::CodeAction(a) => a,
                    CodeActionOrCommand::Command(c) => CodeAction { title: c.title.clone(), command: Some(c), ..Default::default() },
                })
                .collect())
        })
    }

    fn perform_code_action(&self, _state: Entity<EditorState>, action: CodeAction, _push: bool, window: &mut Window, cx: &mut App) -> Task<Result<()>> {
        let client = self.hub.read(cx).client(&self.path);
        let area = self.area.clone();
        window.spawn(cx, async move |cx| {
            let Some(client) = client else { return Ok(()) };
            let mut action = action;
            if action.edit.is_none() && action.command.is_none() {
                let c = client.clone();
                let a = action.clone();
                let rx = runtime::spawn_bridge(async move {
                    let v = c.request_raw("codeAction/resolve", serde_json::to_value(&a)?).await?;
                    Ok::<CodeAction, LspError>(serde_json::from_value(v)?)
                });
                if let Ok(resolved) = recv(rx).await {
                    action = resolved;
                }
            }
            if let Some(edit) = action.edit.clone() {
                area.update_in(cx, |area, window, cx| area.apply_workspace_edit(&edit, window, cx))?;
            }
            if let Some(cmd) = action.command {
                let rx = runtime::spawn_bridge(async move {
                    let params = serde_json::json!({ "command": cmd.command, "arguments": cmd.arguments.unwrap_or_default() });
                    client.request_raw("workspace/executeCommand", params).await.map(|_| ())
                });
                let _ = recv(rx).await;
            }
            Ok(())
        })
    }
}

/// Install all providers on `state`, plus a show-document hook that opens other files in tabs.
pub fn install(state: &Entity<EditorState>, providers: Rc<LspProviders>, cx: &mut App) {
    let area = providers.area.clone();
    let this_path = providers.path.clone();
    state.update(cx, |s, cx| {
        let lsp = s.lsp_mut();
        lsp.completion_provider = Some(providers.clone());
        lsp.hover_provider = Some(providers.clone());
        lsp.definition_provider = Some(providers.clone());
        lsp.code_action_providers = vec![providers.clone()];
        lsp.show_document = Some(Rc::new(move |params: &lsp::ShowDocumentParams, window: &mut Window, cx: &mut App| {
            let Some(path) = tandem_lsp::uri_to_path(&params.uri) else { return false };
            if path == this_path {
                return false;
            }
            let line = params.selection.map(|r| r.start).unwrap_or_default();
            let area = area.clone();
            // The editor is mid-update; open the other tab right after.
            window.defer(cx, move |window, cx| {
                let _ = area.update(cx, |a, cx| a.open_at(path, line, window, cx));
            });
            true
        }));
        cx.notify();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn snippets_are_flattened() {
        assert_eq!(strip_snippet("log(${1:message})$0"), "log(message)");
        assert_eq!(strip_snippet("foo($1)"), "foo()");
    }

    #[test]
    fn completions_get_prefix_edits() {
        let text = "console.lo";
        let items = vec![CompletionItem::new_simple("log".into(), "".into()), CompletionItem::new_simple("error".into(), "".into())];
        let out = prepare_completions(items, text, text.len());
        assert_eq!(out.len(), 1);
        match &out[0].text_edit {
            Some(CompletionTextEdit::Edit(e)) => {
                assert_eq!(e.new_text, "log");
                assert_eq!(e.range.start.character, 8);
            }
            _ => panic!("expected edit"),
        }
    }
}

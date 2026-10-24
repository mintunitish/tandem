//! Database tool window: saved connections (`tandem_db::ConnectionStore`, passwords in the
//! Keychain), schema tree (schemas → tables → columns), SQL console with the risk class shown
//! before running, results grid, and the per-connection "Expose to agents" switch that
//! `crate::tasks::db` turns into a `tandem-db-mcp` server for new tasks.

use crate::assets::icon;
use crate::panels::chip;
use crate::runtime::spawn_bridge;
use crate::tasks::db::DbAgents;
use crate::theme::{MONO_FONT, Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tandem_db::{ColumnInfo, ConnectOptions, ConnectionConfig, ConnectionStore, Db, DbKind, QueryOptions, QueryResult, Secret, SqlClass, Table};

/// Read-only grid of strings; shared by Database results and the Harness tables.
pub struct StringTable {
    pub cols: Vec<(SharedString, f32)>,
    pub rows: Vec<Vec<SharedString>>,
    pub mono: bool,
}

impl StringTable {
    pub fn new(cols: Vec<(SharedString, f32)>, rows: Vec<Vec<SharedString>>, mono: bool) -> Self {
        Self { cols, rows, mono }
    }

    /// Columns sized from their content (80..=360 px).
    pub fn auto(headers: Vec<String>, rows: Vec<Vec<String>>, mono: bool) -> Self {
        let cols = headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let longest = rows.iter().take(200).map(|r| r.get(i).map_or(0, |c| c.chars().count())).max().unwrap_or(0).max(h.chars().count());
                (SharedString::from(h.clone()), (longest as f32 * 7.5 + 24.).clamp(80., 360.))
            })
            .collect();
        Self { cols, rows: rows.into_iter().map(|r| r.into_iter().map(SharedString::from).collect()).collect(), mono }
    }
}

impl TableDelegate for StringTable {
    fn columns_count(&self, _: &App) -> usize {
        self.cols.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (name, w) = &self.cols[col_ix];
        Column::new(name.clone(), name.clone()).width(px(*w))
    }
    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let text = self.rows.get(row_ix).and_then(|r| r.get(col_ix)).cloned().unwrap_or_default();
        let muted = text.as_ref() == "NULL";
        let p = pal(cx);
        div()
            .text_size(px(12.))
            .text_color(if muted { p.muted } else { p.text })
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .when(self.mono, |d| d.font_family(MONO_FONT))
            .child(text)
    }
    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.rows.get(row_ix).and_then(|r| r.get(col_ix)).map(|s| s.to_string()).unwrap_or_default()
    }
}

struct SchemaNode {
    name: String,
    open: bool,
    tables: Vec<TableNode>,
}

struct TableNode {
    table: Table,
    open: bool,
    columns: Option<Result<Vec<ColumnInfo>, String>>,
}

#[derive(Default)]
struct ConnNode {
    open: bool,
    loading: bool,
    schemas: Option<Result<Vec<SchemaNode>, String>>,
}

pub struct DatabasePanel {
    pub root: PathBuf,
    /// Cached from the policy file at construction and on `reload()`; read per render otherwise.
    sandbox_on: bool,
    store: Option<Arc<ConnectionStore>>,
    store_error: Option<String>,
    pub connections: Vec<ConnectionConfig>,
    nodes: HashMap<String, ConnNode>,
    pub exposure: DbAgents,
    /// Connections opened read-only (schema + Read statements) and read-write.
    ro: HashMap<String, Db>,
    rw: HashMap<String, Db>,
    pub active: Option<String>,
    console: Entity<EditorState>,
    results: Entity<TableState<StringTable>>,
    pub status: SharedString,
    pub running: bool,
    form: Entity<ConnectionForm>,
}

impl DatabasePanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let console = cx.new(|cx| EditorState::new(window, cx).language("sql").line_number(true).default_value("SELECT 1;"));
        let results = cx.new(|cx| TableState::new(StringTable::new(vec![], vec![], true), window, cx));
        let (store, store_error) = match ConnectionStore::open_default() {
            Ok(s) => (Some(Arc::new(s)), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let form = cx.new(|cx| ConnectionForm::new(window, cx));
        let mut this = Self {
            exposure: DbAgents::load(&root),
            sandbox_on: crate::state::load_policy(&root).0.sandbox,
            root,
            store,
            store_error,
            connections: vec![],
            nodes: HashMap::new(),
            ro: HashMap::new(),
            rw: HashMap::new(),
            active: None,
            console,
            results,
            status: "Run a query with ⌘⏎".into(),
            running: false,
            form,
        };
        this.reload();
        #[cfg(feature = "test-support")]
        if let Ok(sql) = std::env::var("TANDEM_DEMO_SQL") {
            // `--screenshot` support: open the first connection and run `sql` on it.
            this.set_console_text(&sql, window, cx);
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| {
                    if let Some(c) = this.connections.first().map(|c| c.name.clone()) {
                        this.toggle_connection(&c, cx);
                        this.execute(c, sql, SqlClass::Read, None, cx);
                    }
                });
            })
            .detach();
        }
        this
    }

    pub fn reload(&mut self) {
        self.sandbox_on = crate::state::load_policy(&self.root).0.sandbox;
        if let Some(s) = &self.store {
            match s.list() {
                Ok(list) => self.connections = list,
                Err(e) => self.store_error = Some(e.to_string()),
            }
        }
        if self.active.as_ref().is_none_or(|a| !self.connections.iter().any(|c| &c.name == a)) {
            self.active = self.connections.first().map(|c| c.name.clone());
        }
    }

    fn config(&self, name: &str) -> Option<ConnectionConfig> {
        self.connections.iter().find(|c| c.name == name).cloned()
    }

    pub fn console_text(&self, cx: &App) -> String {
        self.console.read(cx).value().to_string()
    }

    pub fn set_console_text(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.console.update(cx, |c, cx| c.set_value(sql.to_string(), window, cx));
    }

    pub fn results(&self) -> &Entity<TableState<StringTable>> {
        &self.results
    }

    /// Risk class of the console text for the active connection's dialect.
    pub fn classify_console(&self, cx: &App) -> SqlClass {
        let kind = self.active.as_deref().and_then(|a| self.config(a)).map(|c| c.kind);
        let sql = self.console_text(cx);
        match kind {
            Some(k) => tandem_db::classify_for(k, &sql),
            None => tandem_db::classify(&sql),
        }
    }

    /// Connect (cached) on tokio. Password from the Keychain.
    fn with_db(&mut self, name: &str, write: bool) -> Option<async_channel::Receiver<Result<Db, String>>> {
        let cache = if write { &self.rw } else { &self.ro };
        if let Some(db) = cache.get(name).cloned() {
            let (tx, rx) = async_channel::bounded(1);
            let _ = tx.try_send(Ok(db));
            return Some(rx);
        }
        let cfg = self.config(name)?;
        let store = self.store.clone()?;
        Some(spawn_bridge(async move {
            let pw = tokio::task::spawn_blocking(move || store.password(&cfg).map(|p| (cfg, p))).await.map_err(|e| e.to_string())?;
            let (cfg, pw) = pw.map_err(|e| e.to_string())?;
            let opts = if write { ConnectOptions::read_write() } else { ConnectOptions::default() };
            Db::connect(&cfg, pw.as_ref(), opts).await.map_err(|e| e.to_string())
        }))
    }

    pub fn toggle_connection(&mut self, name: &str, cx: &mut Context<Self>) {
        self.active = Some(name.to_string());
        let node = self.nodes.entry(name.to_string()).or_default();
        node.open = !node.open;
        if node.open && node.schemas.is_none() && !node.loading {
            node.loading = true;
            self.load_schema(name.to_string(), cx);
        }
        cx.notify();
    }

    fn load_schema(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(conn) = self.with_db(&name, false) else { return };
        cx.spawn(async move |this, cx| {
            let db = conn.recv().await.unwrap_or_else(|_| Err("connection task ended".into()));
            let db2 = db.clone();
            let schema = match db {
                Ok(db) => {
                    let rx = spawn_bridge(async move {
                        let mut out = vec![];
                        for s in db.schemas().await.map_err(|e| e.to_string())? {
                            let tables = db.tables(Some(&s)).await.map_err(|e| e.to_string())?;
                            out.push((s, tables));
                        }
                        Ok::<_, String>(out)
                    });
                    rx.recv().await.unwrap_or_else(|_| Err("schema task ended".into()))
                }
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |this, cx| {
                if let Ok(db) = db2 {
                    this.ro.insert(name.clone(), db);
                }
                let node = this.nodes.entry(name.clone()).or_default();
                node.loading = false;
                let single = schema.as_ref().is_ok_and(|s| s.len() == 1);
                node.schemas = Some(schema.map(|v| {
                    v.into_iter()
                        .map(|(name, tables)| SchemaNode { name, open: single, tables: tables.into_iter().map(|table| TableNode { table, open: false, columns: None }).collect() })
                        .collect()
                }));
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_table(&mut self, conn: &str, si: usize, ti: usize, cx: &mut Context<Self>) {
        let kind = self.config(conn).map(|c| c.kind);
        let Some(Some(Ok(schemas))) = self.nodes.get_mut(conn).map(|n| n.schemas.as_mut()) else { return };
        let Some(s) = schemas.get_mut(si) else { return };
        let schema = s.name.clone();
        let Some(t) = s.tables.get_mut(ti) else { return };
        t.open = !t.open;
        if !t.open || t.columns.is_some() {
            cx.notify();
            return;
        }
        let table = if kind == Some(DbKind::Sqlite) && schema == "main" { t.table.name.clone() } else { format!("{schema}.{}", t.table.name) };
        let Some(db) = self.ro.get(conn).cloned() else { return };
        let rx = spawn_bridge(async move { db.columns(&table).await.map_err(|e| e.to_string()) });
        let conn = conn.to_string();
        cx.spawn(async move |this, cx| {
            let cols = rx.recv().await.unwrap_or_else(|_| Err("task ended".into()));
            let _ = this.update(cx, |this, cx| {
                if let Some(Some(Ok(schemas))) = this.nodes.get_mut(&conn).map(|n| n.schemas.as_mut())
                    && let Some(t) = schemas.get_mut(si).and_then(|s| s.tables.get_mut(ti))
                {
                    t.columns = Some(cols);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// ⌘⏎: classify, confirm when needed, run.
    pub fn run_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.active.clone() else {
            self.status = "Add a connection first".into();
            cx.notify();
            return;
        };
        let sql = self.console_text(cx);
        if sql.trim().is_empty() {
            return;
        }
        let class = self.classify_console(cx);
        match class {
            SqlClass::Read => self.execute(conn, sql, class, None, cx),
            SqlClass::Write | SqlClass::Ddl => self.confirm(conn, sql, class, false, window, cx),
            SqlClass::Dangerous => self.confirm(conn, sql, class, true, window, cx),
        }
    }

    fn confirm(&mut self, conn: String, sql: String, class: SqlClass, dangerous: bool, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.config(&conn).map(|c| c.kind);
        let detail = tandem_db::classify_detailed(kind, &sql);
        let reasons: Vec<String> = detail.statements.iter().filter(|s| s.class >= SqlClass::Write).map(|s| format!("{}: {}", s.class, s.reason)).chain(detail.parse_error.clone()).collect();
        let view = cx.entity().downgrade();
        let p = pal(cx).clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let (view, conn, sql, p) = (view.clone(), conn.clone(), sql.clone(), p.clone());
            let edge = if dangerous { p.error } else { p.accent };
            let title = if dangerous { format!("Run destructive SQL on {conn}?") } else { format!("Run {class} statement on {conn}?") };
            let ok = {
                let (view, conn, sql) = (view.clone(), conn.clone(), sql.clone());
                Button::new("db-confirm-run").small().label(if dangerous { "Run destructive SQL" } else { "Run" }).when(dangerous, |b| b.danger()).when(!dangerous, |b| b.primary()).on_click(
                    move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            if dangerous {
                                this.approve_then_execute(conn.clone(), sql.clone(), class, cx);
                            } else {
                                this.execute(conn.clone(), sql.clone(), class, None, cx);
                            }
                        });
                        window.close_dialog(cx);
                    },
                )
            };
            dialog
                .title(div().text_color(edge).child(title))
                .w(px(560.))
                .child(
                    v_flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .p(px(8.))
                                .rounded(px(4.))
                                .border_1()
                                .border_color(edge)
                                .bg(if dangerous { p.danger_bg } else { p.editor_bg })
                                .font_family(MONO_FONT)
                                .text_size(px(12.))
                                .text_color(p.text_bright)
                                .child(sql.trim().to_string()),
                        )
                        .children(reasons.iter().map(|r| div().text_size(px(12.)).text_color(edge).child(r.clone())))
                        .child(div().text_size(px(12.)).text_color(p.muted).child(if dangerous {
                            "This statement cannot be undone. It runs on a read-write connection after you confirm, and agents exposed to this database with dangerous SQL allowed may run it too."
                        } else {
                            "Runs on a read-write connection."
                        })),
                )
                .footer(
                    h_flex()
                        .gap(px(8.))
                        .justify_end()
                        .child(Button::new("db-confirm-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(ok),
                )
        });
    }

    /// A confirmed destructive statement: record the approval (for agents exposed with
    /// dangerous SQL, `~/.tandem/db/confirm/`) off the UI thread first, then run it. If the
    /// approval cannot be written the statement does not run.
    fn approve_then_execute(&mut self, conn: String, sql: String, class: SqlClass, cx: &mut Context<Self>) {
        self.status = "Recording approval…".into();
        cx.notify();
        let root = self.root.clone();
        let (c, q) = (conn.clone(), sql.clone());
        let write = cx.background_spawn(async move { crate::tasks::db::approve_statement(&root, &c, &q) });
        cx.spawn(async move |this, cx| {
            let res = write.await;
            let _ = this.update(cx, |this, cx| match res {
                Ok(()) => this.execute(conn.clone(), sql, class, Some(conn), cx),
                Err(e) => {
                    this.status = format!("Not run: the approval could not be recorded ({e})").into();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Run `sql`; `confirm` is the connection name the user confirmed a Dangerous statement for
    /// in the red confirm dialog.
    pub fn execute(&mut self, conn: String, sql: String, class: SqlClass, confirm: Option<String>, cx: &mut Context<Self>) {
        let policy = crate::tasks::db::panel_policy(confirm.as_deref() == Some(conn.as_str()));
        if let Err(why) = policy.check(class, &sql) {
            self.status = format!("Refused: {why}").into();
            cx.notify();
            return;
        }
        let write = class != SqlClass::Read;
        let Some(db) = self.with_db(&conn, write) else { return };
        self.running = true;
        self.status = "Running…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let db = db.recv().await.unwrap_or_else(|_| Err("connection task ended".into()));
            let (db_ok, result) = match db {
                Ok(db) => {
                    let d = db.clone();
                    let rx = spawn_bridge(async move { d.query(&sql, &[], &QueryOptions::default()).await.map_err(|e| e.to_string()) });
                    (Some(db), rx.recv().await.unwrap_or_else(|_| Err("query task ended".into())))
                }
                Err(e) => (None, Err(e)),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(db) = db_ok {
                    if write { this.rw.insert(conn.clone(), db) } else { this.ro.insert(conn.clone(), db) };
                }
                this.running = false;
                match result {
                    Ok(r) => this.show_result(r, cx),
                    Err(e) => this.status = format!("Error: {e}").into(),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_result(&mut self, r: QueryResult, cx: &mut Context<Self>) {
        let headers: Vec<String> = r.columns.iter().map(|c| c.name.clone()).collect();
        let rows: Vec<Vec<String>> = r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect()).collect();
        self.status = if headers.is_empty() {
            format!("{} rows affected in {} ms", r.affected.unwrap_or(0), r.elapsed_ms)
        } else {
            format!("{} row{} in {} ms{}", r.row_count, if r.row_count == 1 { "" } else { "s" }, r.elapsed_ms, if r.truncated { " · truncated (showing the first rows only)" } else { "" })
        }
        .into();
        self.results.update(cx, |t, cx| {
            *t.delegate_mut() = StringTable::auto(headers, rows, true);
            t.refresh(cx);
        });
    }

    pub fn set_exposed(&mut self, name: &str, on: bool, cx: &mut Context<Self>) {
        self.exposure.set_exposed(&self.root, name, on);
        let _ = self.exposure.save(&self.root);
        cx.notify();
    }

    /// Allow-write for agents is a Review-class action: confirmed in a dialog.
    fn ask_allow_write(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.exposure.connections.get(&name).is_some_and(|e| e.allow_write) {
            if let Some(e) = self.exposure.connections.get_mut(&name) {
                e.allow_write = false;
            }
            let _ = self.exposure.save(&self.root);
            cx.notify();
            return;
        }
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (view, name) = (view.clone(), name.clone());
            let p = pal(cx).clone();
            dialog
                .title(format!("Let agents write to {name}?"))
                .w(px(480.))
                .child(div().text_size(px(13.)).text_color(p.text).child(
                    "New agent tasks get this database with --allow-write: INSERT/UPDATE are allowed, DDL and destructive statements stay refused by the MCP server.",
                ))
                .footer(
                    h_flex()
                        .gap(px(8.))
                        .justify_end()
                        .child(Button::new("aw-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("aw-ok").small().primary().label("Allow writes").on_click(move |_, window, cx| {
                            let name = name.clone();
                            let _ = view.update(cx, |this, cx| {
                                if let Some(e) = this.exposure.connections.get_mut(&name) {
                                    e.allow_write = true;
                                }
                                let _ = this.exposure.save(&this.root);
                                cx.notify();
                            });
                            window.close_dialog(cx);
                        })),
                )
        });
    }

    pub fn open_new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = self.form.clone();
        form.update(cx, |f, cx| f.reset(window, cx));
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let (form, view) = (form.clone(), view.clone());
            let (f1, f2) = (form.clone(), form.clone());
            dialog
                .title("New connection")
                .w(px(520.))
                .child(form.clone())
                .footer(
                    h_flex()
                        .gap(px(8.))
                        .child(Button::new("conn-test").small().label("Test").on_click(move |_, _, cx| f1.update(cx, |f, cx| f.test(cx))))
                        .child(div().flex_1())
                        .child(Button::new("conn-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("conn-save").small().primary().label("Save").on_click(move |_, window, cx| {
                            let saved = f2.update(cx, |f, cx| f.save(cx));
                            if let Some(name) = saved {
                                let _ = view.update(cx, |this, cx| {
                                    this.reload();
                                    this.active = Some(name);
                                    cx.notify();
                                });
                                window.close_dialog(cx);
                            }
                        })),
                )
        });
    }

    pub fn delete_connection(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(s) = &self.store {
            let _ = s.remove(name);
        }
        self.exposure.set_exposed(&self.root, name, false);
        let _ = self.exposure.save(&self.root);
        self.ro.remove(name);
        self.rw.remove(name);
        self.nodes.remove(name);
        self.reload();
        cx.notify();
    }

    fn render_tree(&self, p: &Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut tree: Vec<AnyElement> = Vec::new();
        let sandbox_on = self.sandbox_on;
        for (i, c) in self.connections.iter().enumerate() {
            let node = self.nodes.get(&c.name);
            let open = node.is_some_and(|n| n.open);
            let name = c.name.clone();
            let exposed = self.exposure.connections.get(&c.name);
            let active = self.active.as_deref() == Some(c.name.as_str());
            tree.push(
                h_flex()
                    .id(("db-conn", i))
                    .h(px(26.))
                    .px(px(8.))
                    .gap(px(4.))
                    .when(active, |d| d.bg(p.row_selected_inactive))
                    .hover(|s| s.bg(p.hover))
                    .cursor_pointer()
                    .on_click(cx.listener({
                        let name = name.clone();
                        move |this, _, _, cx| this.toggle_connection(&name, cx)
                    }))
                    .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 14., p.muted))
                    .child(icon(IconName::Database, 16., p.accent))
                    .child(div().flex_1().min_w_0().text_color(p.text).whitespace_nowrap().overflow_hidden().text_ellipsis().child(c.name.clone()))
                    .child(div().text_color(p.muted).text_size(px(11.)).child(c.kind.to_string()))
                    .into_any_element(),
            );
            // Agents row: expose switch + write chip.
            let n2 = name.clone();
            let n3 = name.clone();
            tree.push(
                h_flex()
                    .h(px(24.))
                    .pl(px(30.))
                    .pr(px(8.))
                    .gap(px(6.))
                    .child(
                        Switch::new(("db-expose", i)).small().checked(exposed.is_some()).label("Expose to agents").on_click(cx.listener(move |this, on: &bool, _, cx| {
                            this.set_exposed(&n2, *on, cx);
                        })),
                    )
                    .when(sandbox_on, |d| d.child(div().text_size(px(11.)).text_color(p.muted).child("Requires sandbox off (v0.1)")))
                    .when_some(exposed, |d, e| {
                        let label = if e.allow_write { "read + write" } else { "read-only" };
                        let color = if e.allow_write { p.warning } else { p.ok };
                        d.child(
                            div()
                                .id(("db-write", i))
                                .cursor_pointer()
                                .child(chip(label, color, color.opacity(0.15)))
                                .on_click(cx.listener(move |this, _, window, cx| this.ask_allow_write(n3.clone(), window, cx))),
                        )
                    })
                    .into_any_element(),
            );
            if !open {
                continue;
            }
            match node.and_then(|n| n.schemas.as_ref()) {
                None => tree.push(div().pl(px(44.)).h(px(22.)).text_size(px(12.)).text_color(p.muted).child("Connecting…").into_any_element()),
                Some(Err(e)) => tree.push(div().pl(px(44.)).pr(px(8.)).py(px(4.)).text_size(px(12.)).text_color(p.error).child(e.clone()).into_any_element()),
                Some(Ok(schemas)) => {
                    for (si, s) in schemas.iter().enumerate() {
                        let (n, s_open) = (name.clone(), s.open);
                        tree.push(
                            h_flex()
                                .id(SharedString::from(format!("db-schema-{i}-{si}")))
                                .h(px(24.))
                                .pl(px(24.))
                                .gap(px(4.))
                                .hover(|st| st.bg(p.hover))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(Some(Ok(schemas))) = this.nodes.get_mut(&n).map(|n| n.schemas.as_mut())
                                        && let Some(s) = schemas.get_mut(si)
                                    {
                                        s.open = !s.open;
                                    }
                                    cx.notify();
                                }))
                                .child(icon(if s_open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.muted))
                                .child(icon(IconName::Folder, 14., p.muted))
                                .child(div().text_color(p.text).child(s.name.clone()))
                                .child(div().text_size(px(11.)).text_color(p.muted).child(format!("{}", s.tables.len())))
                                .into_any_element(),
                        );
                        if !s_open {
                            continue;
                        }
                        for (ti, t) in s.tables.iter().enumerate() {
                            let n = name.clone();
                            tree.push(
                                h_flex()
                                    .id(SharedString::from(format!("db-table-{i}-{si}-{ti}")))
                                    .h(px(24.))
                                    .pl(px(40.))
                                    .gap(px(6.))
                                    .hover(|st| st.bg(p.hover))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_table(&n, si, ti, cx)))
                                    .child(icon(if t.open { IconName::ChevronDown } else { IconName::ChevronRight }, 12., p.muted))
                                    .child(icon(if t.table.kind == tandem_db::TableKind::View { IconName::Eye } else { IconName::Table }, 14., p.muted))
                                    .child(div().text_color(p.text).child(t.table.name.clone()))
                                    .children(t.table.row_estimate.map(|r| div().text_size(px(11.)).text_color(p.muted).child(format!("~{r}"))))
                                    .into_any_element(),
                            );
                            if !t.open {
                                continue;
                            }
                            match &t.columns {
                                None => tree.push(div().pl(px(64.)).h(px(22.)).text_size(px(12.)).text_color(p.muted).child("Loading…").into_any_element()),
                                Some(Err(e)) => tree.push(div().pl(px(64.)).text_size(px(12.)).text_color(p.error).child(e.clone()).into_any_element()),
                                Some(Ok(cols)) => {
                                    for col in cols {
                                        tree.push(
                                            h_flex()
                                                .h(px(22.))
                                                .pl(px(60.))
                                                .gap(px(6.))
                                                .child(icon(if col.primary_key { IconName::Lock } else { IconName::Minus }, 12., if col.primary_key { p.warning } else { p.muted }))
                                                .child(div().text_size(px(12.)).text_color(p.text).child(col.name.clone()))
                                                .child(div().text_size(px(11.)).text_color(p.muted).child(format!("{}{}", col.data_type, if col.nullable { "" } else { " not null" })))
                                                .into_any_element(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        tree
    }
}

fn class_chip(class: SqlClass, p: &Palette) -> Div {
    let (color, label) = match class {
        SqlClass::Read => (p.ok, "READ"),
        SqlClass::Write => (p.accent, "WRITE"),
        SqlClass::Ddl => (p.warning, "DDL"),
        SqlClass::Dangerous => (p.error, "DANGEROUS"),
    };
    chip(label, color, color.opacity(0.18))
}

impl Render for DatabasePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let tree = self.render_tree(&p, cx);
        let class = self.classify_console(cx);
        let active = self.active.clone().unwrap_or_else(|| "no connection".into());
        let empty = self.connections.is_empty();
        h_flex()
            .size_full()
            .items_start()
            .text_size(px(13.))
            .child(
                v_flex()
                    .w(px(260.))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(p.border)
                    .child(
                        h_flex()
                            .h(px(30.))
                            .px(px(8.))
                            .gap(px(2.))
                            .child(crate::panels::header_icon("db-add", IconName::Plus, &p).on_click(cx.listener(|this, _, window, cx| this.open_new_connection(window, cx))))
                            .child(crate::panels::header_icon("db-refresh", IconName::RefreshCw, &p).on_click(cx.listener(|this, _, _, cx| {
                                this.nodes.clear();
                                this.ro.clear();
                                this.rw.clear();
                                this.reload();
                                cx.notify();
                            })))
                            .when_some(self.active.clone(), |d, a| {
                                d.child(crate::panels::header_icon("db-remove", IconName::Minus, &p).on_click(cx.listener(move |this, _, _, cx| this.delete_connection(&a, cx))))
                            })
                            .child(div().flex_1())
                            .child(div().text_size(px(11.)).text_color(p.muted).child("agents: read-only by default")),
                    )
                    .when_some(self.store_error.clone(), |d, e| d.child(div().px(px(8.)).text_size(px(12.)).text_color(p.error).child(e)))
                    .when(empty, |d| {
                        d.child(
                            v_flex()
                                .p(px(12.))
                                .gap(px(8.))
                                .text_size(px(12.))
                                .text_color(p.muted)
                                .child("No connections yet.")
                                .child(Button::new("db-new").small().label("New connection…").on_click(cx.listener(|this, _, window, cx| this.open_new_connection(window, cx)))),
                        )
                    })
                    .child(v_flex().id("db-tree").flex_1().overflow_y_scroll().children(tree)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .h(px(30.))
                            .flex_none()
                            .px(px(8.))
                            .gap(px(6.))
                            .border_b_1()
                            .border_color(p.border)
                            .child(Button::new("db-run").xsmall().ghost().loading(self.running).icon(gpui_kit::component::IconName::Play).tooltip("Run (⌘⏎)").on_click(cx.listener(|this, _, window, cx| this.run_console(window, cx))))
                            .child(div().text_size(px(12.)).text_color(p.muted).child(format!("console · {active}")))
                            .child(div().flex_1())
                            .child(class_chip(class, &p)),
                    )
                    .child(
                        div()
                            .id("db-console")
                            .h(px(120.))
                            .flex_none()
                            .bg(p.editor_bg)
                            .border_b_1()
                            .border_color(p.border)
                            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                                if ev.keystroke.key == "enter" && ev.keystroke.modifiers.platform {
                                    cx.stop_propagation();
                                    this.run_console(window, cx);
                                }
                            }))
                            .child(Editor::new(&self.console).bordered(false).h_full().font_family(MONO_FONT).text_size(px(13.))),
                    )
                    .child(h_flex().h(px(26.)).flex_none().px(px(10.)).text_size(px(12.)).text_color(p.muted).child(format!("Result · {}", self.status)))
                    .child(div().flex_1().min_h_0().child(DataTable::new(&self.results).stripe(false).bordered(false))),
            )
    }
}

// ------------------------------------------------------------------ new connection form

pub struct ConnectionForm {
    kind: DbKind,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    database: Entity<InputState>,
    password: Entity<InputState>,
    message: Option<(bool, String)>,
    testing: bool,
}

impl ConnectionForm {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |ph: &str, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        Self {
            kind: DbKind::Postgres,
            name: input("local-postgres", window, cx),
            host: input("localhost", window, cx),
            port: input("5432", window, cx),
            user: input("postgres", window, cx),
            database: input("app_dev   (SQLite: path to the .db file)", window, cx),
            password: cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("stored in the macOS Keychain")),
            message: None,
            testing: false,
        }
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for i in [&self.name, &self.host, &self.port, &self.user, &self.database, &self.password] {
            i.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        self.message = None;
        cx.notify();
    }

    fn value(i: &Entity<InputState>, cx: &App) -> Option<String> {
        let v = i.read(cx).value().trim().to_string();
        (!v.is_empty()).then_some(v)
    }

    fn build(&self, cx: &App) -> Result<(ConnectionConfig, Option<Secret>), String> {
        let name = Self::value(&self.name, cx).ok_or("Name is required")?;
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return Err("Name: letters, digits, '-', '_' and '.' only".into());
        }
        let mut cfg = ConnectionConfig::new(name, self.kind);
        cfg.database = Self::value(&self.database, cx);
        if self.kind == DbKind::Sqlite {
            if cfg.database.is_none() {
                return Err("SQLite needs the database file path".into());
            }
            return Ok((cfg, None));
        }
        cfg.host = Self::value(&self.host, cx).or(Some("localhost".into()));
        cfg.port = match Self::value(&self.port, cx) {
            Some(p) => Some(p.parse().map_err(|_| "Port must be a number")?),
            None => self.kind.default_port(),
        };
        cfg.user = Self::value(&self.user, cx);
        let pw = Self::value(&self.password, cx).map(Secret::new);
        Ok((cfg, pw))
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        let (cfg, pw) = match self.build(cx) {
            Ok(v) => v,
            Err(e) => {
                self.message = Some((false, e));
                cx.notify();
                return;
            }
        };
        self.testing = true;
        self.message = Some((true, "Connecting…".into()));
        let rx = spawn_bridge(async move {
            let db = Db::connect(&cfg, pw.as_ref(), ConnectOptions::default()).await.map_err(|e| e.to_string())?;
            let r = db.query("SELECT 1", &[], &QueryOptions::default()).await.map_err(|e| e.to_string());
            db.close().await;
            r.map(|r| r.elapsed_ms)
        });
        cx.spawn(async move |this, cx| {
            let r = rx.recv().await.unwrap_or_else(|_| Err("task ended".into()));
            let _ = this.update(cx, |this, cx| {
                this.testing = false;
                this.message = Some(match r {
                    Ok(ms) => (true, format!("Connected · SELECT 1 in {ms} ms")),
                    Err(e) => (false, e),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Save to the connection store (password → Keychain). Returns the name on success.
    fn save(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let r = self.build(cx).and_then(|(cfg, pw)| {
            let store = ConnectionStore::open_default().map_err(|e| e.to_string())?;
            let name = cfg.name.clone();
            store.upsert(cfg, pw.as_ref()).map_err(|e| e.to_string())?;
            Ok(name)
        });
        match r {
            Ok(n) => Some(n),
            Err(e) => {
                self.message = Some((false, e));
                cx.notify();
                None
            }
        }
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let row = |label: &str, input: &Entity<InputState>| {
            h_flex().gap(px(8.)).child(div().w(px(80.)).text_size(px(12.)).text_color(p.muted).child(label.to_string())).child(div().flex_1().child(Input::new(input).small()))
        };
        let kinds = [(DbKind::Postgres, "PostgreSQL"), (DbKind::MySql, "MySQL"), (DbKind::Sqlite, "SQLite")];
        let sqlite = self.kind == DbKind::Sqlite;
        v_flex()
            .gap(px(8.))
            .child(
                h_flex().gap(px(8.)).child(div().w(px(80.)).text_size(px(12.)).text_color(p.muted).child("Kind")).children(kinds.into_iter().enumerate().map(|(i, (k, label))| {
                    let on = self.kind == k;
                    Button::new(("conn-kind", i)).xsmall().label(label).when(on, |b| b.primary()).on_click(cx.listener(move |this, _, _, cx| {
                        this.kind = k;
                        cx.notify();
                    }))
                })),
            )
            .child(row("Name", &self.name))
            .when(!sqlite, |d| d.child(row("Host", &self.host)).child(row("Port", &self.port)).child(row("User", &self.user)))
            .child(row(if sqlite { "File" } else { "Database" }, &self.database))
            .when(!sqlite, |d| d.child(row("Password", &self.password)))
            .children(self.message.clone().map(|(ok, m)| div().text_size(px(12.)).text_color(if ok { p.ok } else { p.error }).child(m)))
    }
}

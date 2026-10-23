//! Engine-independent database handle.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::TryStreamExt;
use sqlx::mysql::{MySql, MySqlArguments, MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow, MySqlSslMode};
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgRow, PgSslMode, Postgres};
use sqlx::query::Query;
use sqlx::sqlite::{Sqlite, SqliteArguments, SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, Json};
use sqlx::{Column as _, Connection as _, Executor as _, Row, TypeInfo, ValueRef};
use tokio::sync::Notify;

use crate::config::{ConnectionConfig, DbKind, Secret, SslMode};
use crate::error::DbError;
use crate::value::{Column, QueryOptions, QueryResult, StreamItem, Value};

/// Options for [`Db::connect`].
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// Enforce read-only at the engine level (default `true`).
    pub read_only: bool,
    pub max_connections: u32,
    pub connect_timeout: Duration,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self { read_only: true, max_connections: 4, connect_timeout: Duration::from_secs(10) }
    }
}

impl ConnectOptions {
    pub fn read_write() -> Self {
        Self { read_only: false, ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug)]
enum CancelHandle {
    None,
    /// pg backend pid / mysql connection id.
    Backend(i64),
    /// Raw `sqlite3*` of a checked-out connection (only used while the owning future is alive).
    Sqlite(usize),
}

enum Pools {
    Pg(PgPool),
    My(MySqlPool),
    Lite(SqlitePool),
}

struct Inner {
    pools: Pools,
    kind: DbKind,
    read_only: bool,
    name: String,
    cancel: Notify,
    /// Running queries and how to interrupt them.
    active: Mutex<HashMap<u64, CancelHandle>>,
    next_id: AtomicU64,
}

/// A connected database. Cheap to clone.
///
/// In read-only mode every Postgres/MySQL statement runs inside a `READ ONLY`
/// transaction that is always rolled back (so even session-level `SET`s smuggled
/// past the classifier are reverted), and SQLite files are opened with
/// `SQLITE_OPEN_READONLY` plus `PRAGMA query_only`.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("name", &self.inner.name)
            .field("kind", &self.inner.kind)
            .field("read_only", &self.inner.read_only)
            .finish()
    }
}

pub(crate) struct Outcome {
    columns: Option<Vec<Column>>,
    affected: Option<u64>,
}

pub(crate) enum Sink {
    Collect { columns: Vec<Column>, rows: Vec<Vec<Value>>, max: usize, truncated: bool },
    Stream {
        tx: async_channel::Sender<Result<StreamItem, DbError>>,
        batch: Vec<Vec<Value>>,
        batch_size: usize,
        count: usize,
        columns_sent: bool,
    },
}

impl Sink {
    async fn columns(&mut self, cols: &[Column]) {
        match self {
            Sink::Collect { columns, .. } => *columns = cols.to_vec(),
            Sink::Stream { tx, columns_sent, .. } => {
                if !*columns_sent {
                    *columns_sent = true;
                    let _ = tx.send(Ok(StreamItem::Columns(cols.to_vec()))).await;
                }
            }
        }
    }
    /// Returns `false` to stop fetching.
    async fn push(&mut self, row: Vec<Value>) -> bool {
        match self {
            Sink::Collect { rows, max, truncated, .. } => {
                if rows.len() >= *max {
                    *truncated = true;
                    return false;
                }
                rows.push(row);
                true
            }
            Sink::Stream { tx, batch, batch_size, count, .. } => {
                batch.push(row);
                *count += 1;
                if batch.len() >= *batch_size {
                    let b = std::mem::take(batch);
                    return tx.send(Ok(StreamItem::Rows(b))).await.is_ok();
                }
                true
            }
        }
    }
    async fn flush(&mut self) {
        if let Sink::Stream { tx, batch, .. } = self
            && !batch.is_empty()
        {
            let b = std::mem::take(batch);
            let _ = tx.send(Ok(StreamItem::Rows(b))).await;
        }
    }
}

fn col_meta<C: sqlx::Column>(c: &C) -> Column {
    Column { name: c.name().to_string(), type_name: c.type_info().name().to_string() }
}

fn looks_like_rows(sql: &str) -> bool {
    let s = sql.trim_start().to_ascii_uppercase();
    ["SELECT", "WITH", "VALUES", "PRAGMA", "SHOW", "EXPLAIN", "DESCRIBE", "DESC ", "TABLE"]
        .iter()
        .any(|p| s.starts_with(p))
}

macro_rules! fetch_into {
    ($exec:expr, $sql:expr, $params:expr, $bind:ident, $decode:ident, $sink:expr) => {{
        let mut q = sqlx::query($sql);
        for p in $params {
            q = $bind(q, p);
        }
        let mut columns: Option<Vec<Column>> = None;
        let mut affected: Option<u64> = None;
        {
            #[allow(deprecated)]
            let mut stream = q.fetch_many($exec);
            while let Some(item) = stream.try_next().await? {
                match item {
                    sqlx::Either::Left(r) => *affected.get_or_insert(0) += r.rows_affected(),
                    sqlx::Either::Right(row) => {
                        if columns.is_none() {
                            let c: Vec<Column> = row.columns().iter().map(col_meta).collect();
                            $sink.columns(&c).await;
                            columns = Some(c);
                        }
                        let vals: Vec<Value> = (0..row.len()).map(|i| $decode(&row, i)).collect();
                        if !$sink.push(vals).await {
                            break;
                        }
                    }
                }
            }
        }
        if columns.is_none() && looks_like_rows($sql) {
            if let Ok(d) = $exec.describe($sql).await {
                let c: Vec<Column> = d.columns().iter().map(col_meta).collect();
                if !c.is_empty() {
                    $sink.columns(&c).await;
                    columns = Some(c);
                }
            }
        }
        Outcome { columns, affected }
    }};
}

impl Db {
    /// Connects using `cfg`. `password` is only used to build driver options and is
    /// scrubbed from any error message.
    pub async fn connect(cfg: &ConnectionConfig, password: Option<&Secret>, opts: ConnectOptions) -> Result<Db, DbError> {
        let secret = password.map(|p| p.expose());
        let pools = Self::make_pools(cfg, password, &opts).await.map_err(|e| e.scrub(secret))?;
        tracing::debug!(connection = %cfg.name, url = %cfg.redacted_url(), read_only = opts.read_only, "connected");
        Ok(Db {
            inner: Arc::new(Inner {
                pools,
                kind: cfg.kind,
                read_only: opts.read_only,
                name: cfg.name.clone(),
                cancel: Notify::new(),
                active: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        })
    }

    async fn make_pools(cfg: &ConnectionConfig, password: Option<&Secret>, opts: &ConnectOptions) -> Result<Pools, DbError> {
        let ro = opts.read_only;
        Ok(match cfg.kind {
            DbKind::Postgres => {
                let mut o = PgConnectOptions::new()
                    .host(cfg.host.as_deref().unwrap_or("localhost"))
                    .port(cfg.port.unwrap_or(5432))
                    .ssl_mode(match cfg.ssl {
                        SslMode::Disable => PgSslMode::Disable,
                        SslMode::Prefer => PgSslMode::Prefer,
                        SslMode::Require => PgSslMode::Require,
                        SslMode::VerifyFull => PgSslMode::VerifyFull,
                    })
                    .application_name("tandem-db");
                if let Some(u) = &cfg.user {
                    o = o.username(u);
                }
                if let Some(d) = &cfg.database {
                    o = o.database(d);
                }
                if let Some(p) = password {
                    o = o.password(p.expose());
                }
                for (k, v) in &cfg.params {
                    o = o.options([(k.as_str(), v.as_str())]);
                }
                if ro {
                    // Last so it overrides any user-supplied param.
                    o = o.options([("default_transaction_read_only", "on")]);
                }
                Pools::Pg(
                    PgPoolOptions::new()
                        .max_connections(opts.max_connections)
                        .acquire_timeout(opts.connect_timeout)
                        .connect_with(o)
                        .await?,
                )
            }
            DbKind::MySql => {
                let mut o = MySqlConnectOptions::new()
                    .host(cfg.host.as_deref().unwrap_or("localhost"))
                    .port(cfg.port.unwrap_or(3306))
                    .ssl_mode(match cfg.ssl {
                        SslMode::Disable => MySqlSslMode::Disabled,
                        SslMode::Prefer => MySqlSslMode::Preferred,
                        SslMode::Require => MySqlSslMode::Required,
                        SslMode::VerifyFull => MySqlSslMode::VerifyIdentity,
                    });
                if let Some(u) = &cfg.user {
                    o = o.username(u);
                }
                if let Some(d) = &cfg.database {
                    o = o.database(d);
                }
                if let Some(p) = password {
                    o = o.password(p.expose());
                }
                Pools::My(
                    MySqlPoolOptions::new()
                        .max_connections(opts.max_connections)
                        .acquire_timeout(opts.connect_timeout)
                        .after_connect(move |conn, _| {
                            Box::pin(async move {
                                if ro {
                                    conn.execute("SET SESSION TRANSACTION READ ONLY").await?;
                                }
                                Ok(())
                            })
                        })
                        .connect_with(o)
                        .await?,
                )
            }
            DbKind::Sqlite => {
                let path = cfg.database.as_deref().unwrap_or(":memory:");
                let memory = path == ":memory:" || path.is_empty();
                let mut o = if memory {
                    SqliteConnectOptions::from_str("sqlite::memory:")?
                } else {
                    SqliteConnectOptions::new().filename(path).create_if_missing(!ro)
                };
                for (k, v) in &cfg.params {
                    o = o.pragma(k.clone(), v.clone());
                }
                if ro {
                    o = o.read_only(true).pragma("query_only", "ON");
                }
                Pools::Lite(
                    SqlitePoolOptions::new()
                        .max_connections(if memory { 1 } else { opts.max_connections })
                        .acquire_timeout(opts.connect_timeout)
                        .connect_with(o)
                        .await?,
                )
            }
        })
    }

    pub fn kind(&self) -> DbKind {
        self.inner.kind
    }
    pub fn is_read_only(&self) -> bool {
        self.inner.read_only
    }
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Runs `sql` and collects up to `opts.max_rows` rows.
    pub async fn query(&self, sql: &str, params: &[Value], opts: &QueryOptions) -> Result<QueryResult, DbError> {
        let start = Instant::now();
        let mut sink = Sink::Collect { columns: vec![], rows: vec![], max: opts.max_rows, truncated: false };
        let out = self.run_guarded(sql, params, opts.timeout, &mut sink).await?;
        let Sink::Collect { columns, rows, truncated, .. } = sink else { unreachable!() };
        let columns = out.columns.unwrap_or(columns);
        Ok(QueryResult {
            columns,
            row_count: rows.len(),
            rows,
            truncated,
            elapsed_ms: start.elapsed().as_millis() as u64,
            affected: out.affected,
        })
    }

    /// Streams rows in batches of `batch_size`. Must be called inside a tokio runtime.
    /// The final item is `StreamItem::Done` (or an `Err`). Dropping the receiver stops the query.
    pub fn query_stream(
        &self,
        sql: impl Into<String>,
        params: Vec<Value>,
        batch_size: usize,
    ) -> async_channel::Receiver<Result<StreamItem, DbError>> {
        let (tx, rx) = async_channel::bounded(4);
        let db = self.clone();
        let sql = sql.into();
        tokio::spawn(async move {
            let start = Instant::now();
            let mut sink =
                Sink::Stream { tx: tx.clone(), batch: vec![], batch_size: batch_size.max(1), count: 0, columns_sent: false };
            match db.run_guarded(&sql, &params, None, &mut sink).await {
                Ok(out) => {
                    sink.flush().await;
                    let Sink::Stream { count, .. } = sink else { unreachable!() };
                    let _ = tx
                        .send(Ok(StreamItem::Done {
                            row_count: count,
                            affected: out.affected,
                            elapsed_ms: start.elapsed().as_millis() as u64,
                        }))
                        .await;
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                }
            }
        });
        rx
    }

    /// Executes a statement and returns rows affected. Subject to engine read-only mode.
    pub async fn execute(&self, sql: &str) -> Result<u64, DbError> {
        let opts = QueryOptions { max_rows: 0, ..QueryOptions::default() };
        Ok(self.query(sql, &[], &opts).await?.affected.unwrap_or(0))
    }

    /// The engine-specific EXPLAIN text for `sql` (never `ANALYZE`).
    pub fn explain_sql(&self, sql: &str) -> String {
        let sql = sql.trim().trim_end_matches(';');
        match self.inner.kind {
            DbKind::Sqlite => format!("EXPLAIN QUERY PLAN {sql}"),
            _ => format!("EXPLAIN {sql}"),
        }
    }

    /// Returns the query plan without executing the statement.
    pub async fn explain(&self, sql: &str) -> Result<QueryResult, DbError> {
        self.query(&self.explain_sql(sql), &[], &QueryOptions::default()).await
    }

    /// Cancels all queries currently running on this handle (backend cancel on pg/mysql).
    pub fn cancel(&self) {
        self.inner.cancel.notify_waiters();
    }

    pub async fn close(&self) {
        match &self.inner.pools {
            Pools::Pg(p) => p.close().await,
            Pools::My(p) => p.close().await,
            Pools::Lite(p) => p.close().await,
        }
    }

    fn set_cancel_handle(&self, id: u64, h: CancelHandle) {
        if let Some(slot) = self.inner.active.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) {
            *slot = h;
        }
    }

    async fn backend_cancel(&self, pid: i64) {
        let r = match &self.inner.pools {
            Pools::Pg(p) => sqlx::query("SELECT pg_cancel_backend($1)").bind(pid as i32).execute(p).await.map(|_| ()),
            Pools::My(p) => p.execute(format!("KILL QUERY {pid}").as_str()).await.map(|_| ()),
            Pools::Lite(_) => Ok(()),
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "backend cancel failed");
        }
    }

    pub(crate) async fn run_guarded(
        &self,
        sql: &str,
        params: &[Value],
        timeout: Option<Duration>,
        sink: &mut Sink,
    ) -> Result<Outcome, DbError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.active.lock().unwrap_or_else(|e| e.into_inner()).insert(id, CancelHandle::None);
        let notified = self.inner.cancel.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let deadline = async {
            match timeout {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
        };
        let mut work = self.run_inner(id, sql, params, sink);
        let res = tokio::select! {
            biased;
            _ = &mut notified => Err(DbError::Cancelled),
            _ = deadline => Err(DbError::Timeout),
            r = &mut work => r,
        };
        let handle = self.inner.active.lock().unwrap_or_else(|e| e.into_inner()).remove(&id).unwrap_or(CancelHandle::None);
        if matches!(res, Err(DbError::Timeout | DbError::Cancelled)) {
            match handle {
                CancelHandle::Backend(pid) => self.backend_cancel(pid).await,
                CancelHandle::Sqlite(ptr) => {
                    // SAFETY: `work` (which owns the checked-out connection) has not been dropped
                    // yet, so the sqlite3 handle is alive; sqlite3_interrupt is thread-safe.
                    unsafe { libsqlite3_sys::sqlite3_interrupt(ptr as *mut libsqlite3_sys::sqlite3) };
                }
                CancelHandle::None => {}
            }
        }
        drop(work); // drops (and thereby releases) the connection only after interrupting it
        res
    }

    fn run_inner<'a>(
        &'a self,
        id: u64,
        sql: &'a str,
        params: &'a [Value],
        sink: &'a mut Sink,
    ) -> futures::future::BoxFuture<'a, Result<Outcome, DbError>> {
        match &self.inner.pools {
            Pools::Pg(pool) => Box::pin(self.run_pg(pool, id, sql, params, sink)),
            Pools::My(pool) => Box::pin(self.run_my(pool, id, sql, params, sink)),
            Pools::Lite(pool) => Box::pin(self.run_lite(pool, id, sql, params, sink)),
        }
    }

    async fn run_pg(&self, pool: &PgPool, id: u64, sql: &str, params: &[Value], sink: &mut Sink) -> Result<Outcome, DbError> {
        let mut conn = pool.acquire().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *conn).await?;
        self.set_cancel_handle(id, CancelHandle::Backend(pid as i64));
        if self.inner.read_only {
            let mut tx = conn.begin().await?;
            tx.execute("SET TRANSACTION READ ONLY").await?;
            let out = fetch_into!(&mut *tx, sql, params, bind_pg, decode_pg, sink);
            tx.rollback().await?;
            Ok(out)
        } else {
            Ok(fetch_into!(&mut *conn, sql, params, bind_pg, decode_pg, sink))
        }
    }

    async fn run_my(&self, pool: &MySqlPool, id: u64, sql: &str, params: &[Value], sink: &mut Sink) -> Result<Outcome, DbError> {
        let mut conn = pool.acquire().await?;
        let cid: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()").fetch_one(&mut *conn).await?;
        self.set_cancel_handle(id, CancelHandle::Backend(cid as i64));
        if self.inner.read_only {
            conn.execute("SET TRANSACTION READ ONLY").await?;
            let mut tx = conn.begin().await?;
            let out = fetch_into!(&mut *tx, sql, params, bind_my, decode_my, sink);
            tx.rollback().await?;
            Ok(out)
        } else {
            Ok(fetch_into!(&mut *conn, sql, params, bind_my, decode_my, sink))
        }
    }

    async fn run_lite(&self, pool: &SqlitePool, id: u64, sql: &str, params: &[Value], sink: &mut Sink) -> Result<Outcome, DbError> {
        let mut conn = pool.acquire().await?;
        let raw = conn.lock_handle().await?.as_raw_handle().as_ptr() as usize;
        self.set_cancel_handle(id, CancelHandle::Sqlite(raw));
        Ok(fetch_into!(&mut *conn, sql, params, bind_lite, decode_lite, sink))
    }
}

// ---------------------------------------------------------------------------
// Binding
// ---------------------------------------------------------------------------

macro_rules! bind_fn {
    ($name:ident, $db:ty, $args:ty) => {
        fn $name<'q>(q: Query<'q, $db, $args>, v: &Value) -> Query<'q, $db, $args> {
            match v {
                Value::Null => q.bind(None::<String>),
                Value::Bool(b) => q.bind(*b),
                Value::Int(i) => q.bind(*i),
                Value::Float(f) => q.bind(*f),
                Value::Text(s) | Value::Timestamp(s) | Value::Date(s) | Value::Decimal(s) => q.bind(s.clone()),
                Value::Bytes(b) => q.bind(b.clone()),
                Value::Json(j) => q.bind(Json(j.clone())),
            }
        }
    };
}
bind_fn!(bind_pg, Postgres, PgArguments);
bind_fn!(bind_my, MySql, MySqlArguments);
bind_fn!(bind_lite, Sqlite, SqliteArguments<'q>);

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

fn ts_naive(t: NaiveDateTime) -> Value {
    Value::Timestamp(t.format("%Y-%m-%dT%H:%M:%S%.f").to_string())
}
fn ts_utc(t: DateTime<Utc>) -> Value {
    Value::Timestamp(t.to_rfc3339())
}

fn bytes_or_text(b: Vec<u8>) -> Value {
    match String::from_utf8(b) {
        Ok(s) => Value::Text(s),
        Err(e) => Value::Bytes(e.into_bytes()),
    }
}

fn decode_pg(row: &PgRow, i: usize) -> Value {
    let Ok(raw) = row.try_get_raw(i) else { return Value::Null };
    if raw.is_null() {
        return Value::Null;
    }
    let ty = raw.type_info().name().to_string();
    let r: Result<Value, sqlx::Error> = match ty.as_str() {
        "BOOL" => row.try_get::<bool, _>(i).map(Value::Bool),
        "INT2" => row.try_get::<i16, _>(i).map(|v| Value::Int(v.into())),
        "INT4" => row.try_get::<i32, _>(i).map(|v| Value::Int(v.into())),
        "INT8" => row.try_get::<i64, _>(i).map(Value::Int),
        "OID" => row.try_get::<sqlx::postgres::types::Oid, _>(i).map(|v| Value::Int(v.0.into())),
        "FLOAT4" => row.try_get::<f32, _>(i).map(|v| Value::Float(v.into())),
        "FLOAT8" => row.try_get::<f64, _>(i).map(Value::Float),
        "NUMERIC" => row.try_get::<BigDecimal, _>(i).map(|v| Value::Decimal(v.to_string())),
        "BYTEA" => row.try_get::<Vec<u8>, _>(i).map(Value::Bytes),
        "JSON" | "JSONB" => row.try_get::<serde_json::Value, _>(i).map(Value::Json),
        "TIMESTAMPTZ" => row.try_get::<DateTime<Utc>, _>(i).map(ts_utc),
        "TIMESTAMP" => row.try_get::<NaiveDateTime, _>(i).map(ts_naive),
        "DATE" => row.try_get::<NaiveDate, _>(i).map(|d| Value::Date(d.to_string())),
        "TIME" => row.try_get::<NaiveTime, _>(i).map(|t| Value::Text(t.to_string())),
        "UUID" => row.try_get_unchecked::<Vec<u8>, _>(i).map(|b| {
            let h = crate::value::hex(&b);
            if h.len() == 32 {
                Value::Text(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
            } else {
                Value::Bytes(b)
            }
        }),
        "INTERVAL" => row
            .try_get::<sqlx::postgres::types::PgInterval, _>(i)
            .map(|v| Value::Text(format!("{} mons {} days {} us", v.months, v.days, v.microseconds))),
        "TEXT[]" | "VARCHAR[]" | "NAME[]" => {
            row.try_get::<Vec<String>, _>(i).map(|v| Value::Json(serde_json::json!(v)))
        }
        "INT4[]" => row.try_get::<Vec<i32>, _>(i).map(|v| Value::Json(serde_json::json!(v))),
        "INT8[]" => row.try_get::<Vec<i64>, _>(i).map(|v| Value::Json(serde_json::json!(v))),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "CHAR" | "UNKNOWN" | "CITEXT" => {
            row.try_get::<String, _>(i).map(Value::Text)
        }
        _ => row.try_get_unchecked::<Vec<u8>, _>(i).map(bytes_or_text),
    };
    r.unwrap_or_else(|e| Value::Text(format!("<undecodable {ty}: {e}>")))
}

fn decode_my(row: &MySqlRow, i: usize) -> Value {
    let Ok(raw) = row.try_get_raw(i) else { return Value::Null };
    if raw.is_null() {
        return Value::Null;
    }
    let ty = raw.type_info().name().to_string();
    let r: Result<Value, sqlx::Error> = match ty.as_str() {
        "BOOLEAN" => row.try_get::<bool, _>(i).map(Value::Bool),
        t if t.ends_with("UNSIGNED") => row.try_get_unchecked::<u64, _>(i).map(|v| match i64::try_from(v) {
            Ok(x) => Value::Int(x),
            Err(_) => Value::Decimal(v.to_string()),
        }),
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => {
            row.try_get_unchecked::<i64, _>(i).map(Value::Int)
        }
        "FLOAT" => row.try_get::<f32, _>(i).map(|v| Value::Float(v.into())),
        "DOUBLE" => row.try_get::<f64, _>(i).map(Value::Float),
        "DECIMAL" => row.try_get::<BigDecimal, _>(i).map(|v| Value::Decimal(v.to_string())),
        "JSON" => row.try_get::<serde_json::Value, _>(i).map(Value::Json),
        "DATETIME" => row.try_get::<NaiveDateTime, _>(i).map(ts_naive),
        "TIMESTAMP" => row.try_get::<DateTime<Utc>, _>(i).map(ts_utc),
        "DATE" => row.try_get::<NaiveDate, _>(i).map(|d| Value::Date(d.to_string())),
        "TIME" => row.try_get::<NaiveTime, _>(i).map(|t| Value::Text(t.to_string())),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" | "BIT" | "GEOMETRY" => {
            row.try_get_unchecked::<Vec<u8>, _>(i).map(Value::Bytes)
        }
        _ => row.try_get_unchecked::<Vec<u8>, _>(i).map(bytes_or_text),
    };
    r.unwrap_or_else(|e| Value::Text(format!("<undecodable {ty}: {e}>")))
}

fn decode_lite(row: &SqliteRow, i: usize) -> Value {
    let Ok(raw) = row.try_get_raw(i) else { return Value::Null };
    if raw.is_null() {
        return Value::Null;
    }
    let ty = raw.type_info().name().to_string();
    let declared = row.columns()[i].type_info().name().to_ascii_uppercase();
    let r: Result<Value, sqlx::Error> = match ty.as_str() {
        "INTEGER" | "INT8" | "BOOLEAN" => {
            let v = row.try_get_unchecked::<i64, _>(i);
            if declared == "BOOLEAN" || declared == "BOOL" {
                v.map(|x| Value::Bool(x != 0))
            } else {
                v.map(Value::Int)
            }
        }
        "REAL" | "NUMERIC" => row.try_get_unchecked::<f64, _>(i).map(Value::Float),
        "BLOB" => row.try_get_unchecked::<Vec<u8>, _>(i).map(Value::Bytes),
        _ => row.try_get_unchecked::<String, _>(i).map(Value::Text),
    };
    r.unwrap_or_else(|e| Value::Text(format!("<undecodable {ty}: {e}>")))
}

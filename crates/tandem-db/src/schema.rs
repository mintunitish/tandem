//! Schema introspection for Postgres (pg_catalog), MySQL (information_schema) and
//! SQLite (sqlite_master + pragma table-valued functions).

use serde::{Deserialize, Serialize};

use crate::config::DbKind;
use crate::db::Db;
use crate::error::DbError;
use crate::value::{QueryOptions, QueryResult, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Table,
    View,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub kind: TableKind,
    /// Planner estimate (pg `reltuples`, mysql `table_rows`); `None` when unknown.
    pub row_estimate: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
}

/// Quotes one identifier for `kind` (`"x"` for pg/sqlite, `` `x` `` for mysql).
pub fn quote_ident(kind: DbKind, ident: &str) -> String {
    match kind {
        DbKind::MySql => format!("`{}`", ident.replace('`', "``")),
        _ => format!("\"{}\"", ident.replace('"', "\"\"")),
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    for (a, b) in [('"', '"'), ('`', '`'), ('[', ']')] {
        if s.len() >= 2 && s.starts_with(a) && s.ends_with(b) {
            let inner = &s[1..s.len() - 1];
            return match a {
                '"' => inner.replace("\"\"", "\""),
                '`' => inner.replace("``", "`"),
                _ => inner.to_string(),
            };
        }
    }
    s.to_string()
}

/// Splits `schema.table` (quotes respected) into `(schema, table)`.
pub fn split_table(t: &str) -> (Option<String>, String) {
    let mut in_quote: Option<char> = None;
    for (i, c) in t.char_indices() {
        match (in_quote, c) {
            (None, '"' | '`') => in_quote = Some(c),
            (Some(q), c) if c == q => in_quote = None,
            (None, '.') => return (Some(unquote(&t[..i])), unquote(&t[i + 1..])),
            _ => {}
        }
    }
    (None, unquote(t))
}

/// Fully quoted, schema-qualified table reference.
pub fn qualified(kind: DbKind, table: &str) -> String {
    match split_table(table) {
        (Some(s), t) => format!("{}.{}", quote_ident(kind, &s), quote_ident(kind, &t)),
        (None, t) => quote_ident(kind, &t),
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
fn opt_s(v: &Value) -> Option<String> {
    if v.is_null() { None } else { Some(s(v)) }
}
fn csv(v: &Value) -> Vec<String> {
    s(v).split(',').filter(|x| !x.is_empty()).map(|x| x.trim().to_string()).collect()
}
fn text(v: Option<String>) -> Value {
    v.map(Value::Text).unwrap_or(Value::Null)
}

impl Db {
    async fn meta(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DbError> {
        self.query(sql, params, &QueryOptions { max_rows: 100_000, ..QueryOptions::default() }).await
    }

    /// Schemas (pg/mysql) or attached databases (sqlite).
    pub async fn schemas(&self) -> Result<Vec<String>, DbError> {
        let (sql, col) = match self.kind() {
            DbKind::Postgres => (
                "SELECT nspname::text FROM pg_namespace WHERE nspname NOT IN ('pg_catalog','information_schema') \
                 AND nspname NOT LIKE 'pg_toast%' AND nspname NOT LIKE 'pg_temp%' ORDER BY 1",
                0,
            ),
            DbKind::MySql => (
                "SELECT schema_name FROM information_schema.schemata WHERE schema_name NOT IN \
                 ('mysql','information_schema','performance_schema','sys') ORDER BY 1",
                0,
            ),
            DbKind::Sqlite => ("PRAGMA database_list", 1),
        };
        Ok(self.meta(sql, &[]).await?.rows.iter().map(|r| s(&r[col])).collect())
    }

    /// Tables and views in `schema` (default: current schema / `main`).
    pub async fn tables(&self, schema: Option<&str>) -> Result<Vec<Table>, DbError> {
        let schema_v = text(schema.map(str::to_string));
        let rows = match self.kind() {
            DbKind::Postgres => {
                self.meta(
                    "SELECT c.relname::text, c.relkind::text, c.reltuples::float8 FROM pg_class c \
                     JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname::text = COALESCE($1::text, current_schema()::text) \
                     AND c.relkind IN ('r','p','v','m','f') ORDER BY 1",
                    &[schema_v],
                )
                .await?
                .rows
            }
            DbKind::MySql => {
                self.meta(
                    "SELECT table_name, table_type, table_rows FROM information_schema.tables \
                     WHERE table_schema = COALESCE(?, DATABASE()) ORDER BY table_name",
                    &[schema_v],
                )
                .await?
                .rows
            }
            DbKind::Sqlite => {
                let sch = quote_ident(DbKind::Sqlite, schema.unwrap_or("main"));
                self.meta(
                    &format!(
                        "SELECT name, type, NULL FROM {sch}.sqlite_master WHERE type IN ('table','view') \
                         AND name NOT LIKE 'sqlite_%' ORDER BY name"
                    ),
                    &[],
                )
                .await?
                .rows
            }
        };
        Ok(rows
            .iter()
            .map(|r| {
                let k = s(&r[1]).to_ascii_lowercase();
                Table {
                    name: s(&r[0]),
                    kind: if k == "v" || k == "m" || k.contains("view") { TableKind::View } else { TableKind::Table },
                    row_estimate: r[2].as_i64().filter(|n| *n >= 0),
                }
            })
            .collect())
    }

    pub async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
        let (schema, name) = split_table(table);
        let rows = match self.kind() {
            DbKind::Postgres => {
                self.meta(
                    "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                     pg_get_expr(d.adbin, d.adrelid), \
                     EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary \
                             AND a.attnum = ANY(i.indkey)) \
                     FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                     WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped \
                     ORDER BY a.attnum",
                    &[Value::Text(qualified(DbKind::Postgres, table))],
                )
                .await?
                .rows
            }
            DbKind::MySql => {
                self.meta(
                    "SELECT column_name, column_type, is_nullable = 'YES', column_default, column_key = 'PRI' \
                     FROM information_schema.columns WHERE table_schema = COALESCE(?, DATABASE()) \
                     AND table_name = ? ORDER BY ordinal_position",
                    &[text(schema), Value::Text(name)],
                )
                .await?
                .rows
            }
            DbKind::Sqlite => {
                self.meta(
                    "SELECT name, type, \"notnull\" = 0, dflt_value, pk > 0 FROM pragma_table_info(?, ?) ORDER BY cid",
                    &[Value::Text(name), Value::Text(schema.unwrap_or_else(|| "main".into()))],
                )
                .await?
                .rows
            }
        };
        if rows.is_empty() && self.kind() != DbKind::Postgres {
            return Err(DbError::Sql(format!("table not found: {table}")));
        }
        Ok(rows
            .iter()
            .map(|r| ColumnInfo {
                name: s(&r[0]),
                data_type: s(&r[1]),
                nullable: r[2].as_bool().unwrap_or(true),
                default: opt_s(&r[3]),
                primary_key: r[4].as_bool().unwrap_or(false),
            })
            .collect())
    }

    pub async fn indexes(&self, table: &str) -> Result<Vec<Index>, DbError> {
        let (schema, name) = split_table(table);
        match self.kind() {
            DbKind::Postgres => Ok(self
                .meta(
                    "SELECT ic.relname::text, i.indisunique, i.indisprimary, \
                     array_to_string(ARRAY(SELECT pg_get_indexdef(i.indexrelid, k, true) \
                       FROM generate_series(1, i.indnkeyatts) k ORDER BY k), ',') \
                     FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid \
                     WHERE i.indrelid = $1::text::regclass ORDER BY 1",
                    &[Value::Text(qualified(DbKind::Postgres, table))],
                )
                .await?
                .rows
                .iter()
                .map(|r| Index {
                    name: s(&r[0]),
                    unique: r[1].as_bool().unwrap_or(false),
                    primary: r[2].as_bool().unwrap_or(false),
                    columns: csv(&r[3]),
                })
                .collect()),
            DbKind::MySql => Ok(self
                .meta(
                    "SELECT index_name, MIN(non_unique) = 0, GROUP_CONCAT(column_name ORDER BY seq_in_index) \
                     FROM information_schema.statistics WHERE table_schema = COALESCE(?, DATABASE()) \
                     AND table_name = ? GROUP BY index_name ORDER BY index_name",
                    &[text(schema), Value::Text(name)],
                )
                .await?
                .rows
                .iter()
                .map(|r| Index {
                    name: s(&r[0]),
                    unique: r[1].as_bool().unwrap_or(false),
                    primary: s(&r[0]) == "PRIMARY",
                    columns: csv(&r[2]),
                })
                .collect()),
            DbKind::Sqlite => {
                let sch = Value::Text(schema.unwrap_or_else(|| "main".into()));
                let list = self
                    .meta(
                        "SELECT name, \"unique\", origin FROM pragma_index_list(?, ?) ORDER BY name",
                        &[Value::Text(name), sch.clone()],
                    )
                    .await?;
                let mut out = Vec::new();
                for r in &list.rows {
                    let idx = s(&r[0]);
                    let cols = self
                        .meta(
                            "SELECT name FROM pragma_index_info(?, ?) ORDER BY seqno",
                            &[Value::Text(idx.clone()), sch.clone()],
                        )
                        .await?;
                    out.push(Index {
                        name: idx,
                        unique: r[1].as_bool().unwrap_or(false),
                        primary: s(&r[2]) == "pk",
                        columns: cols.rows.iter().map(|c| s(&c[0])).collect(),
                    });
                }
                Ok(out)
            }
        }
    }

    pub async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, DbError> {
        let (schema, name) = split_table(table);
        match self.kind() {
            DbKind::Postgres => {
                let cols = |side: &str, rel: &str| {
                    format!(
                        "array_to_string(ARRAY(SELECT a.attname::text FROM unnest(con.{side}) WITH ORDINALITY k(n, o) \
                         JOIN pg_attribute a ON a.attrelid = con.{rel} AND a.attnum = k.n ORDER BY k.o), ',')"
                    )
                };
                let sql = format!(
                    "SELECT con.conname::text, {}, con.confrelid::regclass::text, {}, \
                     con.confdeltype::text, con.confupdtype::text \
                     FROM pg_constraint con WHERE con.contype = 'f' AND con.conrelid = $1::text::regclass ORDER BY 1",
                    cols("conkey", "conrelid"),
                    cols("confkey", "confrelid")
                );
                let action = |c: String| {
                    Some(
                        match c.as_str() {
                            "a" => "NO ACTION",
                            "r" => "RESTRICT",
                            "c" => "CASCADE",
                            "n" => "SET NULL",
                            "d" => "SET DEFAULT",
                            _ => return None,
                        }
                        .to_string(),
                    )
                };
                Ok(self
                    .meta(&sql, &[Value::Text(qualified(DbKind::Postgres, table))])
                    .await?
                    .rows
                    .iter()
                    .map(|r| ForeignKey {
                        name: opt_s(&r[0]),
                        columns: csv(&r[1]),
                        ref_table: s(&r[2]),
                        ref_columns: csv(&r[3]),
                        on_delete: action(s(&r[4])),
                        on_update: action(s(&r[5])),
                    })
                    .collect())
            }
            DbKind::MySql => Ok(self
                .meta(
                    "SELECT k.constraint_name, GROUP_CONCAT(k.column_name ORDER BY k.ordinal_position), \
                     k.referenced_table_name, GROUP_CONCAT(k.referenced_column_name ORDER BY k.ordinal_position), \
                     r.delete_rule, r.update_rule \
                     FROM information_schema.key_column_usage k JOIN information_schema.referential_constraints r \
                     ON r.constraint_schema = k.constraint_schema AND r.constraint_name = k.constraint_name \
                     WHERE k.table_schema = COALESCE(?, DATABASE()) AND k.table_name = ? \
                     AND k.referenced_table_name IS NOT NULL \
                     GROUP BY k.constraint_name, k.referenced_table_name, r.delete_rule, r.update_rule \
                     ORDER BY k.constraint_name",
                    &[text(schema), Value::Text(name)],
                )
                .await?
                .rows
                .iter()
                .map(|r| ForeignKey {
                    name: opt_s(&r[0]),
                    columns: csv(&r[1]),
                    ref_table: s(&r[2]),
                    ref_columns: csv(&r[3]),
                    on_delete: opt_s(&r[4]),
                    on_update: opt_s(&r[5]),
                })
                .collect()),
            DbKind::Sqlite => {
                let rows = self
                    .meta(
                        "SELECT id, \"table\", \"from\", \"to\", on_update, on_delete \
                         FROM pragma_foreign_key_list(?, ?) ORDER BY id, seq",
                        &[Value::Text(name), Value::Text(schema.unwrap_or_else(|| "main".into()))],
                    )
                    .await?
                    .rows;
                let mut out: Vec<(i64, ForeignKey)> = Vec::new();
                for r in &rows {
                    let id = r[0].as_i64().unwrap_or(0);
                    if out.last().map(|(i, _)| *i) != Some(id) {
                        out.push((
                            id,
                            ForeignKey {
                                name: None,
                                columns: vec![],
                                ref_table: s(&r[1]),
                                ref_columns: vec![],
                                on_update: opt_s(&r[4]),
                                on_delete: opt_s(&r[5]),
                            },
                        ));
                    }
                    if let Some((_, fk)) = out.last_mut() {
                        fk.columns.push(s(&r[2]));
                        fk.ref_columns.push(s(&r[3]));
                    }
                }
                Ok(out.into_iter().map(|(_, f)| f).collect())
            }
        }
    }

    /// Best-effort DDL for a table or view.
    pub async fn ddl(&self, table: &str) -> Result<String, DbError> {
        let (schema, name) = split_table(table);
        match self.kind() {
            DbKind::Sqlite => {
                let sch = quote_ident(DbKind::Sqlite, schema.as_deref().unwrap_or("main"));
                let r = self
                    .meta(
                        &format!(
                            "SELECT sql FROM {sch}.sqlite_master WHERE tbl_name = ? AND sql IS NOT NULL \
                             ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 0 ELSE 1 END, name"
                        ),
                        &[Value::Text(name)],
                    )
                    .await?;
                if r.rows.is_empty() {
                    return Err(DbError::Sql(format!("table not found: {table}")));
                }
                Ok(r.rows.iter().map(|x| format!("{};", s(&x[0]))).collect::<Vec<_>>().join("\n"))
            }
            DbKind::MySql => {
                let r = self.meta(&format!("SHOW CREATE TABLE {}", qualified(DbKind::MySql, table)), &[]).await?;
                Ok(r.rows.first().map(|x| format!("{};", s(&x[1]))).unwrap_or_default())
            }
            DbKind::Postgres => {
                let q = qualified(DbKind::Postgres, table);
                let rel = self
                    .meta(
                        "SELECT c.relkind::text, pg_get_viewdef(c.oid, true) FROM pg_class c WHERE c.oid = $1::text::regclass",
                        &[Value::Text(q.clone())],
                    )
                    .await?;
                if let Some(r) = rel.rows.first()
                    && matches!(s(&r[0]).as_str(), "v" | "m")
                {
                    let kw = if s(&r[0]) == "m" { "MATERIALIZED VIEW" } else { "VIEW" };
                    return Ok(format!("CREATE {kw} {q} AS\n{}", s(&r[1])));
                }
                let cols = self.columns(table).await?;
                let cons = self
                    .meta(
                        "SELECT conname::text, pg_get_constraintdef(oid, true) FROM pg_constraint \
                         WHERE conrelid = $1::text::regclass ORDER BY contype, conname",
                        &[Value::Text(q.clone())],
                    )
                    .await?;
                let idx = self
                    .meta(
                        "SELECT pg_get_indexdef(i.indexrelid) FROM pg_index i WHERE i.indrelid = $1::text::regclass \
                         AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid) ORDER BY 1",
                        &[Value::Text(q.clone())],
                    )
                    .await?;
                let mut lines: Vec<String> = cols
                    .iter()
                    .map(|c| {
                        let mut l = format!("  {} {}", quote_ident(DbKind::Postgres, &c.name), c.data_type);
                        if !c.nullable {
                            l.push_str(" NOT NULL");
                        }
                        if let Some(d) = &c.default {
                            l.push_str(&format!(" DEFAULT {d}"));
                        }
                        l
                    })
                    .collect();
                for r in &cons.rows {
                    lines.push(format!("  CONSTRAINT {} {}", quote_ident(DbKind::Postgres, &s(&r[0])), s(&r[1])));
                }
                let mut out = format!("CREATE TABLE {q} (\n{}\n);", lines.join(",\n"));
                for r in &idx.rows {
                    out.push_str(&format!("\n{};", s(&r[0])));
                }
                Ok(out)
            }
        }
    }
}

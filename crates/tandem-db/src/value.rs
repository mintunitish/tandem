use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A single cell value, engine-independent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
    /// ISO-8601 (`2024-01-02T03:04:05[.fff][Z|+00:00]`).
    Timestamp(String),
    /// `YYYY-MM-DD`.
    Date(String),
    /// Exact decimal rendered as a string.
    Decimal(String),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) | Value::Timestamp(s) | Value::Date(s) | Value::Decimal(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(*b as i64),
            Value::Float(f) => Some(*f as i64),
            Value::Text(s) | Value::Decimal(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            Value::Text(s) => match s.to_ascii_lowercase().as_str() {
                "t" | "true" | "yes" | "y" | "1" => Some(true),
                "f" | "false" | "no" | "n" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
    /// Plain JSON (no type tags): what agents see.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Int(i) => J::from(*i),
            Value::Float(f) => serde_json::Number::from_f64(*f).map(J::Number).unwrap_or_else(|| J::String(f.to_string())),
            Value::Text(s) | Value::Timestamp(s) | Value::Date(s) | Value::Decimal(s) => J::String(s.clone()),
            Value::Bytes(b) => J::String(format!("\\x{}", hex(b))),
            Value::Json(j) => j.clone(),
        }
    }
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("NULL"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Text(s) | Value::Timestamp(s) | Value::Date(s) | Value::Decimal(s) => f.write_str(s),
            Value::Bytes(b) => write!(f, "\\x{}", hex(b)),
            Value::Json(j) => write!(f, "{j}"),
        }
    }
}

macro_rules! from_impl {
    ($($t:ty => $v:ident $(as $c:ty)?),*) => {$(
        impl From<$t> for Value { fn from(x: $t) -> Self { Value::$v((x $(as $c)?).into()) } }
    )*};
}
from_impl!(bool => Bool, i32 => Int as i64, i64 => Int, f64 => Float, String => Text, &str => Text, Vec<u8> => Bytes);

/// Result column metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub type_name: String,
}

/// A fully materialised query result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    /// More rows were available than `max_rows`.
    pub truncated: bool,
    pub elapsed_ms: u64,
    /// Rows affected by DML (`None` when the driver reported nothing).
    pub affected: Option<u64>,
}

impl QueryResult {
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Rows as plain JSON objects (`[{col: value}]`).
    pub fn rows_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.rows
                .iter()
                .map(|r| {
                    let mut m = serde_json::Map::new();
                    for (c, v) in self.columns.iter().zip(r) {
                        m.insert(c.name.clone(), v.to_json());
                    }
                    serde_json::Value::Object(m)
                })
                .collect(),
        )
    }

    /// Compact markdown table. Cells are escaped (pipes, newlines) and capped at `max_cell` chars.
    pub fn to_markdown(&self, max_cell: usize) -> String {
        let esc = |s: &str| -> String {
            let mut out: String = s.chars().take(max_cell).collect();
            if s.chars().count() > max_cell {
                out.push('…');
            }
            out.replace('\\', "\\\\").replace('|', "\\|").replace(['\n', '\r'], " ")
        };
        let mut out = String::new();
        if self.columns.is_empty() {
            match self.affected {
                Some(n) => out.push_str(&format!("({n} rows affected)\n")),
                None => out.push_str("(no columns)\n"),
            }
            return out;
        }
        out.push('|');
        for c in &self.columns {
            out.push_str(&format!(" {} |", esc(&c.name)));
        }
        out.push_str("\n|");
        for _ in &self.columns {
            out.push_str(" --- |");
        }
        out.push('\n');
        for r in &self.rows {
            out.push('|');
            for v in r {
                out.push_str(&format!(" {} |", esc(&v.to_string())));
            }
            out.push('\n');
        }
        out.push_str(&format!(
            "({} row{}{})\n",
            self.row_count,
            if self.row_count == 1 { "" } else { "s" },
            if self.truncated { ", truncated" } else { "" }
        ));
        out
    }
}

/// Options for [`crate::Db::query`].
#[derive(Clone, Debug)]
pub struct QueryOptions {
    pub max_rows: usize,
    pub timeout: Option<Duration>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self { max_rows: 1000, timeout: Some(Duration::from_secs(30)) }
    }
}

/// Items produced by [`crate::Db::query_stream`].
#[derive(Clone, Debug, PartialEq)]
pub enum StreamItem {
    Columns(Vec<Column>),
    Rows(Vec<Vec<Value>>),
    Done { row_count: usize, affected: Option<u64>, elapsed_ms: u64 },
}

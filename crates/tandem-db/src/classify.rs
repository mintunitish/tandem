//! SQL statement classification used to gate agent access.
//!
//! The classifier is deliberately conservative: anything it cannot parse or does
//! not recognise is [`SqlClass::Dangerous`]. It is one of two layers; the other is
//! engine-level read-only enforcement in [`crate::Db`].

use serde::{Deserialize, Serialize};
use sqlparser::ast::{Query, SetExpr, Statement as S};
use sqlparser::dialect::{Dialect, GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use tandem_core::RiskClass;

use crate::config::DbKind;

/// Risk class of a SQL text. Ordered: `Read < Write < Ddl < Dangerous`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlClass {
    Read,
    Write,
    Ddl,
    Dangerous,
}

impl SqlClass {
    pub fn risk(self) -> RiskClass {
        match self {
            SqlClass::Read => RiskClass::Safe,
            SqlClass::Write | SqlClass::Ddl => RiskClass::Review,
            SqlClass::Dangerous => RiskClass::Destructive,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SqlClass::Read => "read",
            SqlClass::Write => "write",
            SqlClass::Ddl => "ddl",
            SqlClass::Dangerous => "dangerous",
        }
    }
}

impl std::fmt::Display for SqlClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Detailed classification: overall class (max over statements) plus one entry per statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub class: SqlClass,
    pub statements: Vec<StatementClass>,
    /// Set when the text could not be parsed (class is then `Dangerous`).
    pub parse_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementClass {
    pub class: SqlClass,
    pub reason: String,
}

/// Classifies with a dialect-agnostic parser.
pub fn classify(sql: &str) -> SqlClass {
    classify_detailed(None, sql).class
}

/// Classifies using the dialect of `kind`.
pub fn classify_for(kind: DbKind, sql: &str) -> SqlClass {
    classify_detailed(Some(kind), sql).class
}

pub fn classify_detailed(kind: Option<DbKind>, sql: &str) -> Classification {
    let dialect: Box<dyn Dialect> = match kind {
        Some(DbKind::Postgres) => Box::new(PostgreSqlDialect {}),
        Some(DbKind::MySql) => Box::new(MySqlDialect {}),
        Some(DbKind::Sqlite) => Box::new(SQLiteDialect {}),
        None => Box::new(GenericDialect {}),
    };
    match Parser::parse_sql(dialect.as_ref(), sql) {
        Ok(stmts) => {
            let statements: Vec<StatementClass> = stmts
                .iter()
                .map(|s| {
                    let (class, reason) = statement_class(s);
                    StatementClass { class, reason: reason.to_string() }
                })
                .collect();
            let class = statements.iter().map(|s| s.class).max().unwrap_or(SqlClass::Read);
            Classification { class, statements, parse_error: None }
        }
        Err(_) if is_pragma_read(sql) => Classification {
            class: SqlClass::Read,
            statements: vec![StatementClass { class: SqlClass::Read, reason: "pragma read".into() }],
            parse_error: None,
        },
        Err(e) => Classification {
            class: SqlClass::Dangerous,
            statements: vec![],
            parse_error: Some(e.to_string()),
        },
    }
}

fn trivially_true(e: &sqlparser::ast::Expr) -> bool {
    let s = e.to_string().to_ascii_uppercase().replace(' ', "");
    let s = s.trim_matches(|c| c == '(' || c == ')');
    matches!(s, "TRUE" | "1" | "1=1" | "'1'='1'" | "NOTFALSE" | "1<>0" | "0=0")
}

fn query_class(q: &Query) -> (SqlClass, &'static str) {
    let mut worst = (SqlClass::Read, "select");
    let mut bump = |c: (SqlClass, &'static str)| {
        if c.0 > worst.0 {
            worst = c;
        }
    };
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            bump(query_class(&cte.query));
        }
    }
    bump(set_expr_class(&q.body));
    if !q.locks.is_empty() {
        bump((SqlClass::Write, "select ... for update/share takes row locks"));
    }
    worst
}

fn set_expr_class(e: &SetExpr) -> (SqlClass, &'static str) {
    match e {
        SetExpr::Select(sel) => {
            if sel.into.is_some() {
                (SqlClass::Ddl, "select into creates a table")
            } else {
                (SqlClass::Read, "select")
            }
        }
        SetExpr::Query(q) => query_class(q),
        SetExpr::SetOperation { left, right, .. } => {
            let (a, b) = (set_expr_class(left), set_expr_class(right));
            if a.0 >= b.0 { a } else { b }
        }
        SetExpr::Values(_) | SetExpr::Table(_) => (SqlClass::Read, "values"),
        SetExpr::Insert(s) | SetExpr::Update(s) | SetExpr::Delete(s) | SetExpr::Merge(s) => {
            let (c, r) = statement_class(s);
            (c.max(SqlClass::Write), r)
        }
        #[allow(unreachable_patterns)]
        _ => (SqlClass::Dangerous, "unrecognised query body"),
    }
}

fn statement_class(s: &S) -> (SqlClass, &'static str) {
    use SqlClass::*;
    match s {
        S::Query(q) => query_class(q),
        S::Explain { analyze, statement, options, .. } => {
            let analyze_opt = options
                .as_ref()
                .is_some_and(|o| format!("{o:?}").to_ascii_uppercase().contains("ANALYZE"));
            if *analyze || analyze_opt {
                // EXPLAIN ANALYZE executes the statement.
                let (c, r) = statement_class(statement);
                (c, r)
            } else {
                (Read, "explain")
            }
        }
        S::ExplainTable { .. }
        | S::ShowTables { .. }
        | S::ShowColumns { .. }
        | S::ShowVariable { .. }
        | S::ShowVariables { .. }
        | S::ShowStatus { .. }
        | S::ShowCreate { .. }
        | S::ShowDatabases { .. }
        | S::ShowSchemas { .. }
        | S::ShowViews { .. }
        | S::ShowFunctions { .. }
        | S::ShowCollation { .. }
        | S::ShowCharset(..)
        | S::ShowObjects(..)
        | S::ShowCatalogs { .. }
        | S::ShowProcessList { .. } => (Read, "show/describe"),
        S::Pragma { is_eq, .. } => {
            if *is_eq {
                (Dangerous, "pragma assignment can change engine safety settings")
            } else {
                (Read, "pragma read")
            }
        }

        S::Insert(_) => (Write, "insert"),
        S::Update(u) => match &u.selection {
            Some(w) if !trivially_true(w) => (Write, "update with where"),
            Some(_) => (Dangerous, "update with trivially true where"),
            None => (Dangerous, "update without where"),
        },
        S::Delete(d) => match &d.selection {
            Some(w) if !trivially_true(w) => (Write, "delete with where"),
            Some(_) => (Dangerous, "delete with trivially true where"),
            None => (Dangerous, "delete without where"),
        },
        S::Merge(_) => (Write, "merge"),
        S::Set(_) | S::Use(_) | S::Reset(_) | S::Discard { .. } => (Write, "session state change"),
        S::StartTransaction { .. }
        | S::Commit { .. }
        | S::Rollback { .. }
        | S::Savepoint { .. }
        | S::ReleaseSavepoint { .. } => (Write, "transaction control"),
        S::Lock(_) | S::LockTables { .. } | S::UnlockTables => (Write, "lock"),
        S::Call(_) | S::Execute { .. } | S::Prepare { .. } | S::Deallocate { .. } => {
            (Write, "procedure / prepared statement execution")
        }
        S::Declare { .. } | S::Fetch { .. } | S::Close { .. } => (Write, "cursor"),
        S::Analyze(_) | S::Vacuum(_) => (Write, "maintenance"),
        S::LISTEN { .. } | S::UNLISTEN { .. } | S::NOTIFY { .. } => (Write, "listen/notify"),

        S::CreateTable(_)
        | S::CreateView(_)
        | S::CreateIndex(_)
        | S::CreateSchema { .. }
        | S::CreateDatabase { .. }
        | S::CreateSequence { .. }
        | S::CreateType { .. }
        | S::CreateDomain(_)
        | S::CreateVirtualTable { .. }
        | S::Comment { .. } => (Ddl, "create / comment"),
        S::CreateFunction(_) | S::CreateProcedure { .. } | S::CreateTrigger(_) => {
            (Ddl, "create routine/trigger")
        }
        S::AlterTable(a) => {
            if a.operations.iter().any(|op| {
                let d = format!("{op:?}");
                d.starts_with("Drop") || d.starts_with("Truncate")
            }) {
                (Dangerous, "alter table ... drop")
            } else {
                (Ddl, "alter table")
            }
        }
        S::AlterIndex { operation, .. } => {
            if format!("{operation:?}").starts_with("Drop") {
                (Dangerous, "alter index ... drop")
            } else {
                (Ddl, "alter index")
            }
        }
        S::AlterView { .. } | S::AlterSchema(_) | S::AlterType(_) | S::RenameTable(_) => (Ddl, "alter"),

        S::Drop { .. } => (Dangerous, "drop"),
        S::Truncate(_) => (Dangerous, "truncate"),
        S::Grant(_) | S::Revoke(_) | S::Deny(_) => (Dangerous, "privilege change"),
        S::CreateRole(_) | S::AlterRole { .. } | S::CreateUser(_) | S::AlterUser(_) => {
            (Dangerous, "role/user management")
        }
        S::AttachDatabase { .. } => (Dangerous, "attach database"),
        S::Copy { .. } => (Dangerous, "copy (file/program access)"),
        S::Kill { .. } => (Dangerous, "kill"),
        _ => (Dangerous, "unrecognised statement"),
    }
}

/// `PRAGMA name` / `PRAGMA schema.name(arg)` with no assignment; sqlparser rejects bare
/// identifiers as pragma arguments.
fn is_pragma_read(sql: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)^\s*PRAGMA\s+[A-Za-z_][\w.]*\s*(\(\s*[\w"'`.]+\s*\))?\s*;?\s*$"#).expect("valid regex")
    })
    .is_match(sql)
}

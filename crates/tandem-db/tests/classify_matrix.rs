use tandem_db::{DbKind, SqlClass, classify, classify_detailed, classify_for};

use DbKind::*;
use SqlClass::*;

#[test]
fn classify_matrix() {
    // (dialect, sql, expected)
    let cases: &[(Option<DbKind>, &str, SqlClass)] = &[
        // --- reads
        (None, "SELECT 1", Read),
        (None, "select * from users where id = 1", Read),
        (None, "WITH t AS (SELECT 1 AS x) SELECT x FROM t", Read),
        (Some(Postgres), "SELECT a, count(*) FROM t GROUP BY a HAVING count(*) > 1 ORDER BY 2 DESC LIMIT 5", Read),
        (Some(Postgres), "SELECT 1 UNION ALL SELECT 2", Read),
        (Some(Postgres), "EXPLAIN SELECT * FROM t", Read),
        (Some(Postgres), "EXPLAIN DELETE FROM t", Read),
        (Some(Postgres), "SHOW search_path", Read),
        (Some(MySql), "SHOW TABLES", Read),
        (Some(MySql), "SHOW COLUMNS FROM users", Read),
        (Some(MySql), "DESCRIBE users", Read),
        (Some(MySql), "SELECT `a` FROM `t` WHERE `b` = 'x'", Read),
        (Some(Sqlite), "PRAGMA table_info(users)", Read),
        (Some(Sqlite), "EXPLAIN QUERY PLAN SELECT * FROM users", Read),
        (None, "-- just a comment\nSELECT 1 /* inline */", Read),
        (None, "/* leading */ SELECT 1; SELECT 2;", Read),
        (None, "VALUES (1), (2)", Read),
        (None, "", Read),
        // --- writes
        (None, "INSERT INTO t (a) VALUES (1)", Write),
        (Some(Postgres), "INSERT INTO t SELECT * FROM s ON CONFLICT DO NOTHING", Write),
        (None, "UPDATE t SET a = 1 WHERE id = 2", Write),
        (None, "DELETE FROM t WHERE id = 2", Write),
        (Some(MySql), "REPLACE INTO t (a) VALUES (1)", Write),
        (Some(Postgres), "SET search_path TO public", Write),
        (None, "BEGIN", Write),
        (None, "COMMIT", Write),
        (Some(Postgres), "SELECT * FROM t FOR UPDATE", Write),
        (Some(Postgres), "WITH d AS (DELETE FROM t WHERE id = 1 RETURNING *) SELECT * FROM d", Write),
        (Some(Postgres), "SELECT 1; UPDATE t SET a = 1 WHERE id = 3", Write),
        // --- ddl
        (None, "CREATE TABLE x (id INT PRIMARY KEY)", Ddl),
        (None, "CREATE INDEX ix ON t (a)", Ddl),
        (None, "CREATE VIEW v AS SELECT 1", Ddl),
        (None, "ALTER TABLE t ADD COLUMN b INT", Ddl),
        (Some(Postgres), "ALTER TABLE t RENAME COLUMN a TO b", Ddl),
        (Some(Postgres), "CREATE SCHEMA s", Ddl),
        (Some(Postgres), "SELECT * INTO newtab FROM t", Ddl),
        (Some(Postgres), "COMMENT ON TABLE t IS 'x'", Ddl),
        // --- dangerous
        (None, "DROP TABLE x", Dangerous),
        (Some(Postgres), "DROP TABLE IF EXISTS x CASCADE", Dangerous),
        (Some(Postgres), "DROP INDEX ix", Dangerous),
        (Some(MySql), "DROP DATABASE prod", Dangerous),
        (None, "TRUNCATE TABLE t", Dangerous),
        (None, "DELETE FROM t", Dangerous),
        (None, "UPDATE t SET a = 1", Dangerous),
        (None, "UPDATE t SET a = 1 WHERE 1 = 1", Dangerous),
        (None, "DELETE FROM t WHERE true", Dangerous),
        (None, "ALTER TABLE t DROP COLUMN a", Dangerous),
        (Some(Postgres), "ALTER TABLE t DROP CONSTRAINT fk", Dangerous),
        (Some(Postgres), "GRANT ALL ON t TO bob", Dangerous),
        (Some(Postgres), "REVOKE SELECT ON t FROM bob", Dangerous),
        (None, "SELECT 1; DROP TABLE x", Dangerous),
        (None, "SELECT 1; -- harmless\n DROP TABLE x", Dangerous),
        (None, "SELECT 1 /* ; */; DELETE FROM t", Dangerous),
        (Some(Postgres), "EXPLAIN ANALYZE DELETE FROM t", Dangerous),
        (Some(Postgres), "EXPLAIN (ANALYZE) DELETE FROM t WHERE id = 1", Write),
        (Some(Postgres), "COPY t TO PROGRAM 'curl evil'", Dangerous),
        (Some(Sqlite), "ATTACH DATABASE '/tmp/x.db' AS x", Dangerous),
        (Some(Sqlite), "PRAGMA query_only = OFF", Dangerous),
        (Some(Postgres), "CREATE ROLE admin SUPERUSER", Dangerous),
        (None, "this is not sql", Dangerous),
        (None, "SELECT FROM WHERE (", Dangerous),
    ];
    assert!(cases.len() >= 40, "matrix has {} cases", cases.len());
    let mut failures = vec![];
    for (kind, sql, want) in cases {
        let got = match kind {
            Some(k) => classify_for(*k, sql),
            None => classify(sql),
        };
        if got != *want {
            failures.push(format!("{kind:?} {sql:?}: want {want:?}, got {got:?} ({:?})", classify_detailed(*kind, sql)));
        }
    }
    assert!(failures.is_empty(), "classification failures:\n{}", failures.join("\n"));
}

#[test]
fn classification_is_max_over_statements_and_reports_each() {
    let c = classify_detailed(Some(Postgres), "SELECT 1; INSERT INTO t VALUES (1); DROP TABLE t");
    assert_eq!(c.class, Dangerous);
    assert_eq!(c.statements.len(), 3);
    assert_eq!(c.statements[0].class, Read);
    assert_eq!(c.statements[1].class, Write);
    let bad = classify_detailed(None, "SELEC 1");
    assert_eq!(bad.class, Dangerous);
    assert!(bad.parse_error.is_some());
    assert_eq!(Dangerous.risk(), tandem_core::RiskClass::Destructive);
    assert_eq!(Read.risk(), tandem_core::RiskClass::Safe);
}

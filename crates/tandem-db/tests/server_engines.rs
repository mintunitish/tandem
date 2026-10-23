//! Postgres / MySQL tests. Skipped unless `TANDEM_TEST_PG_URL` / `TANDEM_TEST_MYSQL_URL` are set,
//! e.g. `TANDEM_TEST_PG_URL=postgres://postgres:pw@localhost/postgres`.

use std::time::Duration;

use tandem_db::{ConnectOptions, ConnectionConfig, Db, DbError, QueryOptions, TableKind, Value};

fn env_cfg(var: &str) -> Option<(ConnectionConfig, Option<tandem_db::Secret>)> {
    let url = std::env::var(var).ok().filter(|s| !s.is_empty())?;
    Some(ConnectionConfig::from_url("test", &url).expect("valid test URL"))
}

#[tokio::test]
async fn postgres_end_to_end() {
    let Some((cfg, pw)) = env_cfg("TANDEM_TEST_PG_URL") else {
        eprintln!("skipping: TANDEM_TEST_PG_URL not set");
        return;
    };
    let schema = format!("tandem_test_{}", std::process::id());
    let rw = Db::connect(&cfg, pw.as_ref(), ConnectOptions::read_write()).await.unwrap();
    rw.execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await.unwrap();
    rw.execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
    rw.execute(&format!(
        "CREATE TABLE {schema}.users (id serial PRIMARY KEY, name text NOT NULL, active boolean DEFAULT true, \
         balance numeric(12,2), meta jsonb, created timestamptz DEFAULT now(), born date, tags text[], raw bytea)"
    ))
    .await
    .unwrap();
    rw.execute(&format!(
        "CREATE TABLE {schema}.orders (id bigserial PRIMARY KEY, user_id int NOT NULL REFERENCES {schema}.users(id) ON DELETE CASCADE, total numeric)"
    ))
    .await
    .unwrap();
    rw.execute(&format!("CREATE INDEX orders_user_idx ON {schema}.orders (user_id)")).await.unwrap();
    rw.execute(&format!("CREATE VIEW {schema}.v AS SELECT id FROM {schema}.users")).await.unwrap();
    let n = rw
        .execute(&format!(
            "INSERT INTO {schema}.users (name, balance, meta, born, tags, raw) SELECT 'u' || g, g * 1.5, \
             jsonb_build_object('g', g), date '2000-01-01' + g, ARRAY['a','b'], '\\x01ff'::bytea FROM generate_series(1, 30) g"
        ))
        .await
        .unwrap();
    assert_eq!(n, 30);

    let db = Db::connect(&cfg, pw.as_ref(), ConnectOptions::default()).await.unwrap();
    let r = db
        .query(
            &format!("SELECT id, name, active, balance, meta, born, tags, raw, created FROM {schema}.users ORDER BY id"),
            &[],
            &QueryOptions { max_rows: 5, ..Default::default() },
        )
        .await
        .unwrap();
    assert!(r.truncated);
    assert_eq!(r.rows[0][0], Value::Int(1));
    assert_eq!(r.rows[0][2], Value::Bool(true));
    assert_eq!(r.rows[0][3], Value::Decimal("1.50".into()));
    assert_eq!(r.rows[0][4], Value::Json(serde_json::json!({"g": 1})));
    assert_eq!(r.rows[0][5], Value::Date("2000-01-02".into()));
    assert_eq!(r.rows[0][6], Value::Json(serde_json::json!(["a", "b"])));
    assert_eq!(r.rows[0][7], Value::Bytes(vec![1, 0xff]));
    assert!(matches!(r.rows[0][8], Value::Timestamp(_)));
    let p = db
        .query(&format!("SELECT name FROM {schema}.users WHERE id = $1"), &[Value::Int(3)], &QueryOptions::default())
        .await
        .unwrap();
    assert_eq!(p.rows[0][0], Value::Text("u3".into()));

    // introspection
    assert!(db.schemas().await.unwrap().contains(&schema));
    let tables = db.tables(Some(&schema)).await.unwrap();
    assert_eq!(tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["orders", "users", "v"]);
    assert_eq!(tables[2].kind, TableKind::View);
    let cols = db.columns(&format!("{schema}.users")).await.unwrap();
    assert_eq!(cols.len(), 9);
    assert!(cols[0].primary_key && !cols[1].nullable);
    let idx = db.indexes(&format!("{schema}.orders")).await.unwrap();
    assert!(idx.iter().any(|i| i.name == "orders_user_idx" && i.columns == vec!["user_id"]));
    assert!(idx.iter().any(|i| i.primary));
    let fks = db.foreign_keys(&format!("{schema}.orders")).await.unwrap();
    assert_eq!(fks[0].columns, vec!["user_id"]);
    assert_eq!(fks[0].on_delete.as_deref(), Some("CASCADE"));
    let ddl = db.ddl(&format!("{schema}.orders")).await.unwrap();
    assert!(ddl.contains("FOREIGN KEY") && ddl.contains("orders_user_idx"), "{ddl}");
    assert!(db.explain(&format!("SELECT * FROM {schema}.users")).await.unwrap().row_count > 0);

    // engine-level read-only, including a session-level bypass attempt
    for sql in [
        format!("INSERT INTO {schema}.users (name) VALUES ('x')"),
        format!("DELETE FROM {schema}.users"),
        format!("DROP TABLE {schema}.orders"),
        format!("CREATE TABLE {schema}.evil (a int)"),
    ] {
        assert!(matches!(db.execute(&sql).await, Err(DbError::Sql(_))), "{sql}");
    }
    let _ = db.query("SELECT set_config('default_transaction_read_only', 'off', false)", &[], &QueryOptions::default()).await;
    assert!(db.execute(&format!("INSERT INTO {schema}.users (name) VALUES ('x')")).await.is_err());

    // timeout cancels on the server
    let e = db
        .query("SELECT pg_sleep(10)", &[], &QueryOptions { max_rows: 1, timeout: Some(Duration::from_millis(300)) })
        .await
        .unwrap_err();
    assert_eq!(e, DbError::Timeout);

    rw.execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
}

#[tokio::test]
async fn mysql_end_to_end() {
    let Some((cfg, pw)) = env_cfg("TANDEM_TEST_MYSQL_URL") else {
        eprintln!("skipping: TANDEM_TEST_MYSQL_URL not set");
        return;
    };
    let t = format!("tandem_test_{}", std::process::id());
    let rw = Db::connect(&cfg, pw.as_ref(), ConnectOptions::read_write()).await.unwrap();
    rw.execute(&format!("DROP TABLE IF EXISTS {t}_orders")).await.unwrap();
    rw.execute(&format!("DROP TABLE IF EXISTS {t}")).await.unwrap();
    rw.execute(&format!(
        "CREATE TABLE {t} (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(50) NOT NULL, active BOOLEAN DEFAULT TRUE, \
         balance DECIMAL(12,2), meta JSON, born DATE, created DATETIME, big BIGINT UNSIGNED, raw VARBINARY(8))"
    ))
    .await
    .unwrap();
    rw.execute(&format!(
        "CREATE TABLE {t}_orders (id INT PRIMARY KEY, user_id INT NOT NULL, INDEX by_user (user_id), \
         CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES {t}(id) ON DELETE CASCADE)"
    ))
    .await
    .unwrap();
    for i in 1..=20 {
        rw.query(
            &format!("INSERT INTO {t} (name, balance, meta, born, created, big, raw) VALUES (?, 1.25, '{{\"a\":1}}', '2001-02-03', '2001-02-03 04:05:06', 18446744073709551615, x'01ff')"),
            &[Value::Text(format!("u{i}"))],
            &QueryOptions::default(),
        )
        .await
        .unwrap();
    }
    let db = Db::connect(&cfg, pw.as_ref(), ConnectOptions::default()).await.unwrap();
    let r = db
        .query(&format!("SELECT id, name, active, balance, meta, born, created, big, raw FROM {t} ORDER BY id"), &[], &QueryOptions { max_rows: 3, ..Default::default() })
        .await
        .unwrap();
    assert!(r.truncated);
    assert_eq!(r.rows[0][0], Value::Int(1));
    assert_eq!(r.rows[0][1], Value::Text("u1".into()));
    assert_eq!(r.rows[0][2], Value::Bool(true));
    assert_eq!(r.rows[0][3], Value::Decimal("1.25".into()));
    assert_eq!(r.rows[0][4], Value::Json(serde_json::json!({"a": 1})));
    assert_eq!(r.rows[0][5], Value::Date("2001-02-03".into()));
    assert_eq!(r.rows[0][6], Value::Timestamp("2001-02-03T04:05:06".into()));
    assert_eq!(r.rows[0][7], Value::Decimal("18446744073709551615".into()));
    assert_eq!(r.rows[0][8], Value::Bytes(vec![1, 0xff]));

    let tables = db.tables(None).await.unwrap();
    assert!(tables.iter().any(|x| x.name == t));
    let cols = db.columns(&t).await.unwrap();
    assert_eq!(cols.len(), 9);
    assert!(cols[0].primary_key);
    let idx = db.indexes(&format!("{t}_orders")).await.unwrap();
    assert!(idx.iter().any(|i| i.name == "by_user"));
    let fks = db.foreign_keys(&format!("{t}_orders")).await.unwrap();
    assert_eq!(fks[0].ref_table, t);
    assert!(db.ddl(&t).await.unwrap().contains("CREATE TABLE"));

    for sql in [format!("INSERT INTO {t} (name) VALUES ('x')"), format!("DELETE FROM {t}")] {
        assert!(matches!(db.execute(&sql).await, Err(DbError::Sql(_))), "{sql}");
    }
    let e = db
        .query("SELECT SLEEP(10)", &[], &QueryOptions { max_rows: 1, timeout: Some(Duration::from_millis(300)) })
        .await
        .unwrap_err();
    assert_eq!(e, DbError::Timeout);

    rw.execute(&format!("DROP TABLE {t}_orders")).await.unwrap();
    rw.execute(&format!("DROP TABLE {t}")).await.unwrap();
}

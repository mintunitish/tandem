use std::time::Duration;

use tandem_db::{
    ConnectOptions, ConnectionConfig, Db, DbError, QueryOptions, StreamItem, TableKind, Value,
};

async fn setup() -> (tempfile::TempDir, ConnectionConfig, Db) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ConnectionConfig::sqlite("t", dir.path().join("test.db"));
    let db = Db::connect(&cfg, None, ConnectOptions::read_write()).await.unwrap();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT UNIQUE, active BOOLEAN DEFAULT 1, score REAL, avatar BLOB)",
    )
    .await
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, total NUMERIC, created_at TEXT)",
    )
    .await
    .unwrap();
    db.execute("CREATE INDEX orders_user_idx ON orders (user_id, created_at)").await.unwrap();
    db.execute("CREATE VIEW active_users AS SELECT id, name FROM users WHERE active = 1").await.unwrap();
    for i in 1..=50 {
        db.query(
            "INSERT INTO users (id, name, email, active, score, avatar) VALUES (?, ?, ?, ?, ?, ?)",
            &[
                Value::Int(i),
                Value::Text(format!("user {i}")),
                if i % 10 == 0 { Value::Null } else { Value::Text(format!("u{i}@x.io")) },
                Value::Bool(i % 2 == 0),
                Value::Float(i as f64 / 2.0),
                Value::Bytes(vec![i as u8, 0xff]),
            ],
            &QueryOptions::default(),
        )
        .await
        .unwrap();
    }
    let n = db.execute("INSERT INTO orders (user_id, total, created_at) VALUES (1, 9.5, '2024-01-01'), (2, 3, '2024-01-02')").await.unwrap();
    assert_eq!(n, 2);
    (dir, cfg, db)
}

#[tokio::test]
async fn introspection() {
    let (_d, _cfg, db) = setup().await;
    assert_eq!(db.schemas().await.unwrap(), vec!["main".to_string()]);

    let tables = db.tables(None).await.unwrap();
    let names: Vec<_> = tables.iter().map(|t| (t.name.as_str(), t.kind)).collect();
    assert_eq!(names, vec![("active_users", TableKind::View), ("orders", TableKind::Table), ("users", TableKind::Table)]);

    let cols = db.columns("users").await.unwrap();
    assert_eq!(cols.len(), 6);
    assert_eq!(cols[0].name, "id");
    assert!(cols[0].primary_key);
    assert!(!cols[1].nullable);
    assert_eq!(cols[3].default.as_deref(), Some("1"));
    assert_eq!(cols[3].data_type, "BOOLEAN");
    assert!(db.columns("main.users").await.unwrap().len() == 6);
    assert!(db.columns("nope").await.is_err());

    let idx = db.indexes("orders").await.unwrap();
    let ix = idx.iter().find(|i| i.name == "orders_user_idx").unwrap();
    assert_eq!(ix.columns, vec!["user_id", "created_at"]);
    assert!(!ix.unique);
    let uidx = db.indexes("users").await.unwrap();
    assert!(uidx.iter().any(|i| i.unique && i.columns == vec!["email"]));

    let fks = db.foreign_keys("orders").await.unwrap();
    assert_eq!(fks.len(), 1);
    assert_eq!(fks[0].ref_table, "users");
    assert_eq!(fks[0].columns, vec!["user_id"]);
    assert_eq!(fks[0].ref_columns, vec!["id"]);
    assert_eq!(fks[0].on_delete.as_deref(), Some("CASCADE"));

    let ddl = db.ddl("orders").await.unwrap();
    assert!(ddl.contains("CREATE TABLE orders"), "{ddl}");
    assert!(ddl.contains("CREATE INDEX orders_user_idx"), "{ddl}");
}

#[tokio::test]
async fn query_values_truncation_and_params() {
    let (_d, _cfg, db) = setup().await;
    let r = db
        .query("SELECT * FROM users ORDER BY id", &[], &QueryOptions { max_rows: 10, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(r.row_count, 10);
    assert!(r.truncated);
    assert_eq!(r.columns.len(), 6);
    assert_eq!(r.columns[1].name, "name");
    let row = &r.rows[1]; // id 2
    assert_eq!(row[0], Value::Int(2));
    assert_eq!(row[1], Value::Text("user 2".into()));
    assert_eq!(row[3], Value::Bool(true));
    assert_eq!(row[4], Value::Float(1.0));
    assert_eq!(row[5], Value::Bytes(vec![2, 0xff]));
    assert_eq!(r.rows[9][2], Value::Null);

    let all = db.query("SELECT id FROM users", &[], &QueryOptions::default()).await.unwrap();
    assert_eq!(all.row_count, 50);
    assert!(!all.truncated);

    let p = db
        .query("SELECT name FROM users WHERE id > ? AND name LIKE ?", &[Value::Int(48), Value::Text("user%".into())], &QueryOptions::default())
        .await
        .unwrap();
    assert_eq!(p.row_count, 2);

    // empty result still reports columns
    let e = db.query("SELECT id, name FROM users WHERE id < 0", &[], &QueryOptions::default()).await.unwrap();
    assert_eq!(e.row_count, 0);
    assert_eq!(e.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["id", "name"]);

    let md = r.to_markdown(50);
    assert!(md.starts_with("| id | name |"), "{md}");
    assert!(md.contains("truncated"));
    let json = r.rows_json();
    assert_eq!(json[0]["name"], "user 1");

    let affected = db.execute("UPDATE users SET score = 0 WHERE id <= 5").await.unwrap();
    assert_eq!(affected, 5);

    let plan = db.explain("SELECT * FROM orders WHERE user_id = 1").await.unwrap();
    assert!(plan.row_count >= 1);
    assert!(plan.to_markdown(200).to_lowercase().contains("orders_user_idx"), "{}", plan.to_markdown(200));
}

#[tokio::test]
async fn streaming() {
    let (_d, _cfg, db) = setup().await;
    let rx = db.query_stream("SELECT id FROM users ORDER BY id", vec![], 16);
    let mut cols = None;
    let mut batches = vec![];
    let mut done = None;
    while let Ok(item) = rx.recv().await {
        match item.unwrap() {
            StreamItem::Columns(c) => cols = Some(c),
            StreamItem::Rows(r) => batches.push(r.len()),
            StreamItem::Done { row_count, .. } => done = Some(row_count),
        }
    }
    assert_eq!(cols.unwrap()[0].name, "id");
    assert_eq!(batches, vec![16, 16, 16, 2]);
    assert_eq!(done, Some(50));
}

const SLOW: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c";

#[tokio::test]
async fn timeout_and_cancel() {
    let (_d, cfg, _db) = setup().await;
    let db = Db::connect(&cfg, None, ConnectOptions { max_connections: 2, ..ConnectOptions::default() }).await.unwrap();
    let r = db.query(SLOW, &[], &QueryOptions { max_rows: 10, timeout: Some(Duration::from_millis(200)) }).await;
    assert_eq!(r.unwrap_err(), DbError::Timeout);

    let db2 = db.clone();
    let h = tokio::spawn(async move { db2.query(SLOW, &[], &QueryOptions { max_rows: 10, timeout: None }).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    db.cancel();
    let r = tokio::time::timeout(Duration::from_secs(5), h).await.expect("cancel took effect").unwrap();
    assert_eq!(r.unwrap_err(), DbError::Cancelled);
    // Runaway queries were interrupted inside SQLite, so every pooled connection is free again:
    // run more concurrent queries than the pool size.
    let mut hs = vec![];
    for _ in 0..8 {
        let d = db.clone();
        hs.push(tokio::spawn(async move { d.query("SELECT 1", &[], &QueryOptions::default()).await }));
    }
    for h in hs {
        let r = tokio::time::timeout(Duration::from_secs(5), h).await.expect("pool not wedged").unwrap().unwrap();
        assert_eq!(r.rows[0][0], Value::Int(1));
    }
}

#[tokio::test]
async fn read_only_blocks_writes_at_engine_level() {
    let (_d, cfg, rw) = setup().await;
    rw.close().await;
    let db = Db::connect(&cfg, None, ConnectOptions::default()).await.unwrap();
    assert!(db.is_read_only());
    // Reads work.
    assert_eq!(db.query("SELECT count(*) FROM users", &[], &QueryOptions::default()).await.unwrap().rows[0][0], Value::Int(50));
    // Writes fail inside the engine (no classifier involved).
    for sql in [
        "INSERT INTO users (id, name) VALUES (999, 'x')",
        "UPDATE users SET name = 'x' WHERE id = 1",
        "DELETE FROM users",
        "DROP TABLE orders",
        "CREATE TABLE evil (x INT)",
        "PRAGMA query_only = OFF; DELETE FROM users",
    ] {
        let e = db.execute(sql).await;
        assert!(matches!(e, Err(DbError::Sql(_))), "{sql} -> {e:?}");
    }
    assert_eq!(db.query("SELECT count(*) FROM users", &[], &QueryOptions::default()).await.unwrap().rows[0][0], Value::Int(50));
    assert_eq!(db.tables(None).await.unwrap().len(), 3);
}

#[tokio::test]
async fn read_only_does_not_create_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ConnectionConfig::sqlite("t", dir.path().join("missing.db"));
    assert!(Db::connect(&cfg, None, ConnectOptions::default()).await.is_err());
    assert!(!dir.path().join("missing.db").exists());
}

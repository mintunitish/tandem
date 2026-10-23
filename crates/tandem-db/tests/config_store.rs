use tandem_db::{ConnectionConfig, ConnectionStore, DbKind, MemorySecretStore, Secret, SecretStore};

const PW: &str = "hunter2-S3cr3t!";

fn pg_cfg() -> ConnectionConfig {
    let mut c = ConnectionConfig::new("prod", DbKind::Postgres);
    c.host = Some("db.example.com".into());
    c.port = Some(5432);
    c.user = Some("app".into());
    c.database = Some("main".into());
    c.params.insert("application_name".into(), "tandem".into());
    c.params.insert("sslpassword".into(), PW.into());
    c.password_ref = Some("keychain:tandem-db/prod".into());
    c
}

#[test]
fn debug_and_display_redact_secrets() {
    let c = pg_cfg();
    for s in [format!("{c:?}"), format!("{c}"), c.redacted_url()] {
        assert!(!s.contains(PW), "leaked in {s}");
    }
    let secret = Secret::new(PW);
    assert!(!format!("{secret:?}").contains(PW));
    assert!(!format!("{secret}").contains(PW));
    let url = c.to_url(Some(&secret));
    assert!(!format!("{url:?}").contains(PW));
    assert!(!format!("{url}").contains(PW));
    assert!(url.expose().contains("hunter2-S3cr3t%21"), "{}", url.redacted());
    assert!(url.expose().starts_with("postgres://app:"));
    assert!(url.redacted().contains("app:***@db.example.com:5432/main"));
}

#[test]
fn sqlite_url() {
    let c = ConnectionConfig::sqlite("local", "/tmp/a.db");
    assert_eq!(c.to_url(None).expose(), "sqlite:///tmp/a.db");
}

#[test]
fn store_crud_keeps_passwords_out_of_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db/connections.json");
    let store = ConnectionStore::open(&path, Box::new(MemorySecretStore::default()));
    assert!(store.list().unwrap().is_empty());

    let mut c = pg_cfg();
    c.password_ref = None;
    c.params.clear();
    store.upsert(c.clone(), Some(&Secret::new(PW))).unwrap();
    store.upsert(ConnectionConfig::sqlite("local", "/tmp/x.db"), None).unwrap();

    let json = std::fs::read_to_string(&path).unwrap();
    assert!(!json.contains(PW));
    assert_eq!(store.list().unwrap().len(), 2);
    let got = store.get("prod").unwrap().unwrap();
    assert_eq!(got.password_ref.as_deref(), Some("keychain:tandem-db/prod"));
    assert_eq!(store.password(&got).unwrap().unwrap().expose(), PW);
    assert!(store.password(&store.get("local").unwrap().unwrap()).unwrap().is_none());

    // update without password keeps the reference
    let mut upd = got.clone();
    upd.password_ref = None;
    upd.port = Some(6543);
    store.upsert(upd, None).unwrap();
    let got = store.get("prod").unwrap().unwrap();
    assert_eq!(got.port, Some(6543));
    assert!(got.password_ref.is_some());

    assert!(store.remove("prod").unwrap());
    assert!(!store.remove("prod").unwrap());
    assert!(store.get("prod").unwrap().is_none());
}

#[test]
fn memory_secret_store_roundtrip() {
    let s = MemorySecretStore::default();
    s.set("a", &Secret::new("x")).unwrap();
    assert_eq!(s.get("a").unwrap().unwrap().expose(), "x");
    s.delete("a").unwrap();
    assert!(s.get("a").unwrap().is_none());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "touches the real login keychain; run manually"]
fn keychain_roundtrip() {
    let k = tandem_db::KeychainStore;
    let acct = "tandem-db-test-account";
    k.set(acct, &Secret::new("pw")).unwrap();
    assert_eq!(k.get(acct).unwrap().unwrap().expose(), "pw");
    k.delete(acct).unwrap();
    assert!(k.get(acct).unwrap().is_none());
}

#[test]
fn from_url_roundtrip() {
    let (c, pw) = ConnectionConfig::from_url("x", "postgres://app:p%40ss@db.local:6543/main?sslmode=require&application_name=t").unwrap();
    assert_eq!(c.kind, DbKind::Postgres);
    assert_eq!(c.user.as_deref(), Some("app"));
    assert_eq!(pw.unwrap().expose(), "p@ss");
    assert_eq!(c.host.as_deref(), Some("db.local"));
    assert_eq!(c.port, Some(6543));
    assert_eq!(c.database.as_deref(), Some("main"));
    assert_eq!(c.ssl, tandem_db::SslMode::Require);
    assert_eq!(c.params.get("application_name").map(String::as_str), Some("t"));
    assert!(!format!("{c:?}{c}").contains("p@ss"));
    let (m, pw) = ConnectionConfig::from_url("m", "mysql://root@localhost/test").unwrap();
    assert_eq!((m.kind, m.port, pw.is_none()), (DbKind::MySql, None, true));
    let (s, _) = ConnectionConfig::from_url("s", "sqlite:///tmp/a.db").unwrap();
    assert_eq!(s.database.as_deref(), Some("/tmp/a.db"));
    assert!(ConnectionConfig::from_url("bad", "redis://x").is_err());
}

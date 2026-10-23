//! Connection configuration, persisted at `~/.tandem/db/connections.json`.
//!
//! Passwords are never stored in the JSON file: they live in the macOS Keychain
//! (service `tandem-db`, account = connection name) and `password_ref` only records
//! where to find them. Every `Debug`/`Display` impl in this module redacts secrets.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::DbError;

/// Keychain service name used for all connection passwords.
pub const KEYCHAIN_SERVICE: &str = "tandem-db";
const REDACTED: &str = "***";

/// Database engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbKind {
    Postgres,
    MySql,
    Sqlite,
}

impl DbKind {
    pub fn scheme(self) -> &'static str {
        match self {
            DbKind::Postgres => "postgres",
            DbKind::MySql => "mysql",
            DbKind::Sqlite => "sqlite",
        }
    }
    pub fn default_port(self) -> Option<u16> {
        match self {
            DbKind::Postgres => Some(5432),
            DbKind::MySql => Some(3306),
            DbKind::Sqlite => None,
        }
    }
}

impl fmt::Display for DbKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.scheme())
    }
}

/// TLS mode. Ignored for SQLite.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyFull,
}

/// A secret string (password). `Debug`/`Display` never print the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// Explicit access to the raw value. Never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({REDACTED})")
    }
}
impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// One saved connection.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub name: String,
    pub kind: DbKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Database name; for SQLite, the file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default)]
    pub ssl: SslMode,
    /// Extra driver parameters (appended to the URL query string).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// Where the password lives, e.g. `keychain:tandem-db/<name>`. `None` = no password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
}

fn is_sensitive_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["pass", "pwd", "secret", "token", "key", "auth", "credential"]
        .iter()
        .any(|s| k.contains(s))
}

impl ConnectionConfig {
    pub fn new(name: impl Into<String>, kind: DbKind) -> Self {
        Self {
            name: name.into(),
            kind,
            host: None,
            port: None,
            user: None,
            database: None,
            ssl: SslMode::default(),
            params: BTreeMap::new(),
            password_ref: None,
        }
    }

    /// Convenience constructor for a SQLite file.
    pub fn sqlite(name: impl Into<String>, path: impl AsRef<Path>) -> Self {
        let mut c = Self::new(name, DbKind::Sqlite);
        c.database = Some(path.as_ref().to_string_lossy().into_owned());
        c
    }

    /// Parses `postgres://user:pass@host:port/db?k=v`, `mysql://...` or `sqlite://path`.
    /// The password (if any) is returned separately and never stored in the config.
    pub fn from_url(name: impl Into<String>, url: &str) -> Result<(Self, Option<Secret>), DbError> {
        let bad = || DbError::Config("invalid connection URL".into());
        let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => DbKind::Postgres,
            "mysql" | "mariadb" => DbKind::MySql,
            "sqlite" => DbKind::Sqlite,
            _ => return Err(DbError::Config(format!("unsupported scheme '{scheme}'"))),
        };
        let mut cfg = Self::new(name, kind);
        let (rest, query) = match rest.split_once('?') {
            Some((a, b)) => (a, Some(b)),
            None => (rest, None),
        };
        let mut password = None;
        if kind == DbKind::Sqlite {
            cfg.database = Some(pct_decode(rest));
        } else {
            let (auth, hostpart) = match rest.rsplit_once('@') {
                Some((a, h)) => (Some(a), h),
                None => (None, rest),
            };
            if let Some(a) = auth {
                match a.split_once(':') {
                    Some((u, p)) => {
                        cfg.user = Some(pct_decode(u));
                        password = Some(Secret(pct_decode(p)));
                    }
                    None => cfg.user = Some(pct_decode(a)),
                }
            }
            let (hostport, db) = match hostpart.split_once('/') {
                Some((h, d)) => (h, Some(d)),
                None => (hostpart, None),
            };
            let (host, port) = match hostport.rsplit_once(':') {
                Some((h, p)) if !h.is_empty() => (h, Some(p.parse::<u16>().map_err(|_| bad())?)),
                _ => (hostport, None),
            };
            if !host.is_empty() {
                cfg.host = Some(pct_decode(host));
            }
            cfg.port = port;
            cfg.database = db.filter(|d| !d.is_empty()).map(pct_decode);
        }
        for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let (k, v) = (pct_decode(k), pct_decode(v));
            match (k.as_str(), v.to_ascii_lowercase().as_str()) {
                ("sslmode" | "ssl-mode", "disable" | "disabled") => cfg.ssl = SslMode::Disable,
                ("sslmode" | "ssl-mode", "prefer" | "preferred" | "allow") => cfg.ssl = SslMode::Prefer,
                ("sslmode" | "ssl-mode", "require" | "required" | "verify-ca" | "verify_ca") => cfg.ssl = SslMode::Require,
                ("sslmode" | "ssl-mode", "verify-full" | "verify_identity") => cfg.ssl = SslMode::VerifyFull,
                _ => {
                    cfg.params.insert(k, v);
                }
            }
        }
        Ok((cfg, password))
    }

    /// Params with sensitive values replaced by `***`.
    fn redacted_params(&self) -> BTreeMap<&str, &str> {
        self.params
            .iter()
            .map(|(k, v)| (k.as_str(), if is_sensitive_key(k) { REDACTED } else { v.as_str() }))
            .collect()
    }

    /// Builds the connection URL. The result's `Debug`/`Display` are redacted;
    /// call [`SecretUrl::expose`] to get the real string for the driver.
    pub fn to_url(&self, password: Option<&Secret>) -> SecretUrl {
        let real = self.build_url(password.map(|p| p.expose()), false);
        let redacted = self.build_url(password.map(|_| REDACTED), true);
        SecretUrl { real, redacted }
    }

    /// URL suitable for logs and UI (password and sensitive params masked).
    pub fn redacted_url(&self) -> String {
        self.build_url(self.password_ref.as_ref().map(|_| REDACTED), true)
    }

    fn build_url(&self, password: Option<&str>, redact_params: bool) -> String {
        let mut url = format!("{}://", self.kind.scheme());
        if self.kind == DbKind::Sqlite {
            url.push_str(self.database.as_deref().unwrap_or(":memory:"));
        } else {
            if let Some(u) = &self.user {
                url.push_str(&pct_encode(u));
                if let Some(p) = password {
                    url.push(':');
                    if p == REDACTED {
                        url.push_str(p);
                    } else {
                        url.push_str(&pct_encode(p));
                    }
                }
                url.push('@');
            }
            url.push_str(self.host.as_deref().unwrap_or("localhost"));
            if let Some(p) = self.port {
                url.push_str(&format!(":{p}"));
            }
            if let Some(db) = &self.database {
                url.push('/');
                url.push_str(&pct_encode(db));
            }
        }
        let mut q: Vec<String> = Vec::new();
        if self.kind != DbKind::Sqlite {
            let ssl = match (self.kind, self.ssl) {
                (DbKind::Postgres, SslMode::Disable) => "sslmode=disable",
                (DbKind::Postgres, SslMode::Prefer) => "sslmode=prefer",
                (DbKind::Postgres, SslMode::Require) => "sslmode=require",
                (DbKind::Postgres, SslMode::VerifyFull) => "sslmode=verify-full",
                (_, SslMode::Disable) => "ssl-mode=DISABLED",
                (_, SslMode::Prefer) => "ssl-mode=PREFERRED",
                (_, SslMode::Require) => "ssl-mode=REQUIRED",
                (_, SslMode::VerifyFull) => "ssl-mode=VERIFY_IDENTITY",
            };
            q.push(ssl.to_string());
        }
        let params: Vec<(String, String)> = if redact_params {
            self.redacted_params().into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        } else {
            self.params.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        for (k, v) in params {
            q.push(format!("{}={}", pct_encode(&k), if v == REDACTED && redact_params { v } else { pct_encode(&v) }));
        }
        if !q.is_empty() {
            url.push('?');
            url.push_str(&q.join("&"));
        }
        url
    }
}

impl fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("database", &self.database)
            .field("ssl", &self.ssl)
            .field("params", &self.redacted_params())
            .field("password_ref", &self.password_ref.as_ref().map(|_| "<keychain>"))
            .finish()
    }
}

impl fmt::Display for ConnectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.redacted_url())
    }
}

/// A connection URL that may contain a password. Printing it is always redacted.
#[derive(Clone)]
pub struct SecretUrl {
    real: String,
    redacted: String,
}
impl SecretUrl {
    pub fn expose(&self) -> &str {
        &self.real
    }
    pub fn redacted(&self) -> &str {
        &self.redacted
    }
}
impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretUrl({})", self.redacted)
    }
}
impl fmt::Display for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted)
    }
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Secret storage
// ---------------------------------------------------------------------------

/// Where connection passwords are kept.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Secret>, DbError>;
    fn set(&self, account: &str, secret: &Secret) -> Result<(), DbError>;
    fn delete(&self, account: &str) -> Result<(), DbError>;
}

/// macOS Keychain (generic password, service `tandem-db`).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainStore;

#[cfg(target_os = "macos")]
impl SecretStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, DbError> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(|s| Some(Secret(s)))
                .map_err(|_| DbError::Secret("keychain item is not valid UTF-8".into())),
            Err(e) if e.code() == -25300 => Ok(None), // errSecItemNotFound
            Err(e) => Err(DbError::Secret(format!("keychain read failed (code {})", e.code()))),
        }
    }
    fn set(&self, account: &str, secret: &Secret) -> Result<(), DbError> {
        security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, account, secret.expose().as_bytes())
            .map_err(|e| DbError::Secret(format!("keychain write failed (code {})", e.code())))
    }
    fn delete(&self, account: &str) -> Result<(), DbError> {
        match security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(DbError::Secret(format!("keychain delete failed (code {})", e.code()))),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl SecretStore for KeychainStore {
    fn get(&self, _account: &str) -> Result<Option<Secret>, DbError> {
        Err(DbError::Secret("keychain is only supported on macOS".into()))
    }
    fn set(&self, _account: &str, _secret: &Secret) -> Result<(), DbError> {
        Err(DbError::Secret("keychain is only supported on macOS".into()))
    }
    fn delete(&self, _account: &str) -> Result<(), DbError> {
        Ok(())
    }
}

/// In-memory store for tests and headless use.
#[derive(Debug, Default)]
pub struct MemorySecretStore(std::sync::Mutex<BTreeMap<String, Secret>>);

impl SecretStore for MemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, DbError> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).get(account).cloned())
    }
    fn set(&self, account: &str, secret: &Secret) -> Result<(), DbError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(account.to_string(), secret.clone());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), DbError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(account);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ConnectionStore
// ---------------------------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    connections: Vec<ConnectionConfig>,
}

/// CRUD over `connections.json` + the secret store.
pub struct ConnectionStore {
    path: PathBuf,
    secrets: Box<dyn SecretStore>,
}

impl fmt::Debug for ConnectionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionStore").field("path", &self.path).finish_non_exhaustive()
    }
}

impl ConnectionStore {
    /// `~/.tandem/db/connections.json`.
    pub fn default_path() -> Result<PathBuf, DbError> {
        let home = dirs::home_dir().ok_or_else(|| DbError::Config("cannot determine home directory".into()))?;
        Ok(home.join(".tandem").join("db").join("connections.json"))
    }

    /// Default location (overridable with `TANDEM_DB_STORE`) backed by the Keychain.
    pub fn open_default() -> Result<Self, DbError> {
        let path = match std::env::var_os("TANDEM_DB_STORE") {
            Some(p) => PathBuf::from(p),
            None => Self::default_path()?,
        };
        Ok(Self::open(path, Box::new(KeychainStore)))
    }

    pub fn open(path: impl Into<PathBuf>, secrets: Box<dyn SecretStore>) -> Self {
        Self { path: path.into(), secrets }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<StoreFile, DbError> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) if s.trim().is_empty() => Ok(StoreFile::default()),
            Ok(s) => serde_json::from_str(&s).map_err(|e| DbError::Config(format!("{}: {e}", self.path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile::default()),
            Err(e) => Err(DbError::Io(e.to_string())),
        }
    }

    fn save(&self, file: &StoreFile) -> Result<(), DbError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| DbError::Io(e.to_string()))?;
        }
        let json = serde_json::to_string_pretty(file).map_err(|e| DbError::Config(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| DbError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| DbError::Io(e.to_string()))
    }

    pub fn list(&self) -> Result<Vec<ConnectionConfig>, DbError> {
        Ok(self.load()?.connections)
    }

    pub fn get(&self, name: &str) -> Result<Option<ConnectionConfig>, DbError> {
        Ok(self.load()?.connections.into_iter().find(|c| c.name == name))
    }

    /// Insert or replace by name. `password`: `Some` stores it in the secret store and
    /// sets `password_ref`; `None` keeps whatever password was stored before.
    pub fn upsert(&self, mut cfg: ConnectionConfig, password: Option<&Secret>) -> Result<(), DbError> {
        if cfg.name.trim().is_empty() {
            return Err(DbError::Config("connection name must not be empty".into()));
        }
        let mut file = self.load()?;
        let previous = file.connections.iter().position(|c| c.name == cfg.name);
        if let Some(p) = password {
            self.secrets.set(&cfg.name, p)?;
            cfg.password_ref = Some(format!("keychain:{KEYCHAIN_SERVICE}/{}", cfg.name));
        } else if cfg.password_ref.is_none()
            && let Some(i) = previous
        {
            cfg.password_ref = file.connections[i].password_ref.clone();
        }
        match previous {
            Some(i) => file.connections[i] = cfg,
            None => file.connections.push(cfg),
        }
        self.save(&file)
    }

    /// Removes the connection and its stored password. Returns whether it existed.
    pub fn remove(&self, name: &str) -> Result<bool, DbError> {
        let mut file = self.load()?;
        let before = file.connections.len();
        file.connections.retain(|c| c.name != name);
        let existed = file.connections.len() != before;
        if existed {
            self.save(&file)?;
            self.secrets.delete(name)?;
        }
        Ok(existed)
    }

    /// Password for a connection (only looked up when `password_ref` is set).
    pub fn password(&self, cfg: &ConnectionConfig) -> Result<Option<Secret>, DbError> {
        if cfg.password_ref.is_none() {
            return Ok(None);
        }
        self.secrets.get(&cfg.name)
    }
}

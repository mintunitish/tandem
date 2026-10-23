//! Any ACP agent: `session/update` `sessionUpdate:"usage_update"` (`size`, `used`,
//! optional `cost{amount,currency}`) and `AgentEvent::Usage`. Context + spend, not quota.

use super::{get, get_f64, raw_json, session_windows};
use crate::model::{QuotaSnapshot, Source, Unit, Window, WindowKind};
use crate::provider::{ProviderCore, QuotaProvider};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::Value;
use tandem_core::{AgentEvent, AgentId, Receiver, Usage};

/// Finds a `usage_update` object in a raw ACP line (notification, bare update, or params).
fn find_usage_update(v: &Value) -> Option<&Value> {
    [Some(v), v.get("update"), v.pointer("/params/update"), v.get("params")]
        .into_iter()
        .flatten()
        .find(|u| u.get("sessionUpdate").and_then(|s| s.as_str()) == Some("usage_update"))
}

/// `usage_update` → "context" (Tokens, limit = size) + "cost" (Usd) windows.
pub fn parse_usage_update(v: &Value, now: DateTime<Utc>) -> Option<Vec<Window>> {
    let u = find_usage_update(v)?;
    let mut out = Vec::new();
    let used = get_f64(u, &["used"]);
    let size = get_f64(u, &["size"]);
    if used.is_some() || size.is_some() {
        out.push(Window::new("context", Unit::Tokens, Source::AgentEvent, now).with_kind(WindowKind::Context).with_amounts(used, size));
    }
    if let Some(c) = get(u, &["cost"]) {
        let amount = get_f64(c, &["amount"]).or_else(|| c.as_f64());
        let currency = get(c, &["currency"]).and_then(|x| x.as_str()).unwrap_or("USD");
        if let Some(a) = amount.filter(|_| currency.eq_ignore_ascii_case("usd")) {
            out.push(Window::new("cost", Unit::Usd, Source::AgentEvent, now).with_kind(WindowKind::Usage).with_amounts(Some(a), None));
        }
    }
    Some(out)
}

pub struct AcpGenericProvider {
    core: ProviderCore,
    session: Mutex<Usage>,
}

impl AcpGenericProvider {
    pub fn new(agent: AgentId) -> Self {
        Self { core: ProviderCore::new(agent), session: Mutex::new(Usage::default()) }
    }
    pub fn core(&self) -> &ProviderCore {
        &self.core
    }
}

impl QuotaProvider for AcpGenericProvider {
    fn agent(&self) -> AgentId {
        self.core.agent()
    }
    fn snapshot(&self) -> BoxFuture<'_, QuotaSnapshot> {
        Box::pin(async move { self.core.current() })
    }
    fn subscribe(&self) -> Receiver<QuotaSnapshot> {
        self.core.subscribe()
    }
    fn ingest_event(&self, ev: &AgentEvent) {
        let now = Utc::now();
        let windows = match ev {
            AgentEvent::Usage(u) => {
                let mut s = self.session.lock();
                s.add(u);
                session_windows(&s, Source::AgentEvent, now)
            }
            AgentEvent::Raw { json, .. } => match raw_json(json).and_then(|v| parse_usage_update(&v, now)) {
                Some(w) => w,
                None => return,
            },
            _ => return,
        };
        if !windows.is_empty() {
            let mut d = self.core.delta();
            d.windows = windows;
            self.core.apply(d);
        }
    }
}

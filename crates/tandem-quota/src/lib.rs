//! tandem-quota: the Quota Pool + Router.
//!
//! Every agent reports its *own* usage signals (stream events, app-server rate limits,
//! local transcript logs). This crate normalises them into [`QuotaSnapshot`]s, merges
//! them by confidence and freshness, aggregates them in a [`QuotaPool`] and ranks agents
//! for a task with [`router::route`].
//!
//! Security: credential files are never opened. All file reads go through
//! [`safe_fs::safe_open`], which rejects the vendor credential stores.

pub mod model;
pub mod persist;
pub mod pool;
pub mod provider;
pub mod providers;
pub mod router;
pub mod safe_fs;

pub use model::*;
pub use pool::{PoolState, QuotaPool};
pub use provider::{ProviderCore, QuotaProvider};
pub use router::{estimate, route, Estimate, PhaseKey, RouteChoice, RouteRequest, TurnRecord};

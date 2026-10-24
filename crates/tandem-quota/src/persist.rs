//! On-disk locations: `$TANDEM_HOME/quota/{state.json,calibration.json}` (default `~/.tandem/quota/`).
//! `TANDEM_QUOTA_DIR` overrides the directory (tests, portable installs).

use crate::pool::PoolState;
use crate::safe_fs;
use std::path::{Path, PathBuf};

pub fn quota_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TANDEM_QUOTA_DIR") {
        return PathBuf::from(d);
    }
    tandem_core::fs::tandem_home().join("quota")
}
pub fn state_path() -> PathBuf {
    quota_dir().join("state.json")
}
pub fn calibration_path() -> PathBuf {
    quota_dir().join("calibration.json")
}

pub fn load_state(path: &Path) -> Option<PoolState> {
    let s = safe_fs::safe_read_to_string(path).ok()?;
    match serde_json::from_str(&s) {
        Ok(st) => Some(st),
        Err(e) => {
            tracing::warn!("ignoring unreadable quota state {}: {e}", path.display());
            None
        }
    }
}

pub fn save_state(path: &Path, st: &PoolState) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(st).map_err(std::io::Error::other)?;
    safe_fs::atomic_write(path, &bytes)
}

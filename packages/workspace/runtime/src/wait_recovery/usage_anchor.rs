//! Current derived snapshots alongside unchanged sealed handoff sources.

use super::WaitHandoff;
use centaeris_core::session::history::migrate_prompt_usage_anchor_snapshot;

impl WaitHandoff {
    pub(crate) fn migrate_prompt_usage_anchor(&mut self) -> Result<bool, String> {
        if self.normalized_snapshot_json.is_some() {
            self.current_snapshot_json()?;
            return Ok(false);
        }
        let source: serde_json::Value =
            serde_json::from_str(&self.snapshot_json).map_err(|error| error.to_string())?;
        if source.get("history").is_none() {
            // The earlier history upgrade retains its original sealed source.
            self.normalized_snapshot()?;
            return Ok(false);
        }
        let normalized = migrate_prompt_usage_anchor_snapshot(&self.snapshot_json)?;
        if normalized == self.snapshot_json {
            return Ok(false);
        }
        self.normalized_snapshot_json = Some(normalized);
        Ok(true)
    }

    pub(super) fn current_snapshot_json(&self) -> Result<&str, String> {
        match &self.normalized_snapshot_json {
            Some(normalized) => {
                // The receipt seals the original source. Verify that this separate
                // projection removes only its obsolete derived calibration.
                if migrate_prompt_usage_anchor_snapshot(&self.snapshot_json)? != *normalized {
                    return Err("wait recovery normalized snapshot binding mismatch".into());
                }
                Ok(normalized)
            }
            None => Ok(&self.snapshot_json),
        }
    }
}

use sha2::{Digest, Sha256};

/// One initial execution is outside this budget. Every replacement preparation
/// must first reserve a durable attempt, including preparations that fail.
pub(crate) struct ExecutionRecoverySchedule {
    pub(crate) maximum_attempts: u64,
}

impl ExecutionRecoverySchedule {
    pub(crate) fn from_env() -> Result<Self, String> {
        Ok(Self {
            maximum_attempts: crate::positive_u64_env(
                "RUNTIME_EXECUTION_RECOVERY_MAX_ATTEMPTS",
                5,
            )?,
        })
    }

    /// The anchor is a committed checkpoint (first attempt) or the previous
    /// committed reservation. Deterministic jitter survives worker restarts.
    pub(crate) fn retry_at_ms(
        &self,
        agent_run_id: &str,
        completed_attempts: u64,
        anchor_ms: i64,
    ) -> Result<i64, String> {
        if completed_attempts >= self.maximum_attempts {
            return Err("execution recovery attempt budget exhausted".to_string());
        }
        let base_ms = 1_000_i64 << completed_attempts.min(4);
        let digest = Sha256::digest(format!(
            "execution-recovery:{agent_run_id}:{completed_attempts}"
        ));
        let jitter = i64::from(u16::from_be_bytes([digest[0], digest[1]]) % 401) - 200;
        anchor_ms
            .checked_add(base_ms + base_ms * jitter / 1_000)
            .ok_or_else(|| "execution recovery retry deadline overflow".to_string())
    }
}

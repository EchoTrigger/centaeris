//! Internal diagnostics never include raw tool, provider, or helper output.
use sha2::{Digest, Sha256};

fn record(agent_run_id: &str, stage: &'static str, error: &str) -> serde_json::Value {
    let reason = [
        "execution activity changed during workspace snapshot collection",
        "snapshot manifest length is invalid",
        "snapshot manifest is not canonical",
        "snapshot file integrity mismatch",
        "snapshot collect frame has trailing bytes",
        "sandbox user processes did not quiesce",
    ]
    .into_iter()
    .find(|reason| error == *reason)
    .or_else(|| {
        [
            "snapshot collect helper failed:",
            "read snapshot collect frame failed:",
            "decode snapshot manifest failed:",
            "stage execution workspace returned ",
            "stage execution workspace failed:",
            "decode execution workspace stage failed:",
            "Docker Engine request failed:",
        ]
        .into_iter()
        .find(|prefix| error.starts_with(prefix))
    })
    .unwrap_or("unclassified");
    serde_json::json!({
        "event": "agent_run_internal_failure", "agentRunId": agent_run_id,
        "stage": stage, "reason": reason,
        "errorDigest": format!("sha256:{:x}", Sha256::digest(error.as_bytes())),
        "errorBytes": error.len(),
    })
}

pub(crate) fn report(agent_run_id: &str, stage: &'static str, error: &str) {
    eprintln!("{}", record(agent_run_id, stage, error));
}

pub(crate) fn observe<T>(
    agent_run_id: &str,
    stage: &'static str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    operation().inspect_err(|error| report(agent_run_id, stage, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_classify_without_copying_dynamic_error_content() {
        let error = "snapshot collect helper failed: SECRET_TOKEN\nprivate model text";
        let value = record("agent_run_test", "snapshot_collect", error);
        assert_eq!(value["reason"], "snapshot collect helper failed:");
        assert_eq!(value["stage"], "snapshot_collect");
        assert!(!value.to_string().contains("SECRET_TOKEN"));
        assert!(!value.to_string().contains("private model text"));
        assert_eq!(
            record("agent_run_test", "core_turn", "SECRET_TOKEN")["reason"],
            "unclassified"
        );
    }

    #[test]
    fn diagnostics_preserve_success_and_original_failure() {
        assert_eq!(observe("run", "stage", || Ok(7)), Ok(7));
        let error = "original failure with detail".to_string();
        assert_eq!(
            observe::<()>("run", "stage", || Err(error.clone())),
            Err(error)
        );
    }
}

//! Internal diagnostics never include raw tool, provider, or helper output.
use centaeris_core::tool::layer::EXECUTION_CANCELLATION_INDETERMINATE;
use sha2::{Digest, Sha256};

fn record(agent_run_id: &str, stage: &'static str, error: &str) -> serde_json::Value {
    let model_failure = crate::api_model_client::hosted_model_failure(error);
    let cancellation_failure = error.strip_prefix("fatal_execution_outcome:")
        == Some(EXECUTION_CANCELLATION_INDETERMINATE)
        || error.starts_with("execution cancellation indeterminate:");
    let reason = if cancellation_failure {
        EXECUTION_CANCELLATION_INDETERMINATE
    } else if let Some(failure) = &model_failure {
        failure.reason_type.as_str()
    } else {
        classify_error(error)
    };
    let mut value = serde_json::json!({
        "event": "agent_run_internal_failure", "agentRunId": agent_run_id,
        "stage": stage, "reason": reason,
        "errorDigest": format!("sha256:{:x}", Sha256::digest(error.as_bytes())),
        "errorBytes": error.len(),
    });
    if let Some(failure) = model_failure {
        // The validated envelope contains only schema/category, status and retry
        // counters. Never copy the surrounding provider error into diagnostics.
        value["modelFailure"] = serde_json::json!(failure);
    }
    value
}

fn classify_error(error: &str) -> &str {
    [
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
    .unwrap_or("unclassified")
}

pub(crate) fn report(agent_run_id: &str, stage: &'static str, error: &str) {
    eprintln!("{}", record(agent_run_id, stage, error));
}

pub(crate) fn cleanup_record(
    agent_run_id: &str,
    stage: &'static str,
    error: &str,
) -> serde_json::Value {
    let reason = match error {
        "execution_resident_capacity_wait" => "execution_resident_capacity_wait",
        "environment_host_busy" => "environment_host_busy",
        "cleanup_signing_key_unavailable" => "cleanup_signing_key_unavailable",
        "cleanup_container_identity_missing" => "cleanup_container_identity_missing",
        "cleanup_physical_identity_rejected" => "cleanup_physical_identity_rejected",
        "cleanup_physical_absence_unconfirmed" => "cleanup_physical_absence_unconfirmed",
        "cleanup_physical_release_unconfirmed" => "cleanup_physical_release_unconfirmed",
        "environment_host_transport_unavailable" => "environment_host_transport_unavailable",
        _ => "unclassified",
    };
    let mut diagnostic = record(agent_run_id, stage, error);
    diagnostic["reason"] = reason.into();
    // Cleanup only classifies exact local codes. Provider metadata and dynamic
    // text must not become a second interpretation of an unknown cleanup error.
    if let Some(fields) = diagnostic.as_object_mut() {
        fields.remove("modelFailure");
    }
    diagnostic
}

pub(crate) fn report_cleanup(agent_run_id: &str, stage: &'static str, error: &str) {
    eprintln!("{}", cleanup_record(agent_run_id, stage, error));
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
    fn cleanup_diagnostics_classify_only_exact_known_waits_at_each_boundary() {
        for stage in [
            "teardown_signing_key",
            "teardown_supplement_queue",
            "teardown_terminal_subagents",
            "teardown_host_context",
            "teardown_cleanup_authority",
        ] {
            for (error, reason) in [
                (
                    "execution_resident_capacity_wait",
                    "execution_resident_capacity_wait",
                ),
                ("environment_host_busy", "environment_host_busy"),
                (
                    "cleanup_signing_key_unavailable",
                    "cleanup_signing_key_unavailable",
                ),
                (
                    "cleanup_container_identity_missing",
                    "cleanup_container_identity_missing",
                ),
                (
                    "cleanup_physical_identity_rejected",
                    "cleanup_physical_identity_rejected",
                ),
                (
                    "cleanup_physical_absence_unconfirmed",
                    "cleanup_physical_absence_unconfirmed",
                ),
                (
                    "cleanup_physical_release_unconfirmed",
                    "cleanup_physical_release_unconfirmed",
                ),
                (
                    "environment_host_transport_unavailable",
                    "environment_host_transport_unavailable",
                ),
                (
                    "execution_resident_capacity_wait: PRIVATE_TOKEN",
                    "unclassified",
                ),
                ("private tool content", "unclassified"),
            ] {
                let diagnostic = cleanup_record("run-a", stage, error);
                assert_eq!(diagnostic["stage"], stage);
                assert_eq!(diagnostic["reason"], reason);
                assert_eq!(diagnostic["errorBytes"], error.len());
                assert_eq!(
                    diagnostic["errorDigest"],
                    format!("sha256:{:x}", Sha256::digest(error.as_bytes()))
                );
                assert_eq!(diagnostic.as_object().unwrap().len(), 6);
                assert!(!diagnostic.to_string().contains("PRIVATE_TOKEN"));
                assert!(!diagnostic.to_string().contains("private tool content"));
                let contextual = format!("{error}: PRIVATE_TOKEN");
                let contextual = cleanup_record("run-a", stage, &contextual);
                assert_eq!(contextual["reason"], "unclassified");
                assert!(!contextual.to_string().contains("PRIVATE_TOKEN"));
            }
        }
    }

    #[test]
    fn unknown_cleanup_errors_do_not_copy_model_metadata_or_dynamic_classification() {
        let error = "execution cancellation indeterminate: PRIVATE_TOKEN";
        let diagnostic = cleanup_record("run-a", "teardown_cleanup_authority", error);
        assert_eq!(diagnostic["reason"], "unclassified");
        assert_eq!(diagnostic.as_object().unwrap().len(), 6);
        let failure = serde_json::json!({"schema":"hosted.model.failure.v1", "reasonType":"provider_unavailable", "httpStatus":503, "providerAttempts":6, "retryExhausted":true});
        let error = format!("hosted_model_error: {failure}");
        let diagnostic = cleanup_record("run-a", "teardown_cleanup_authority", &error);
        assert_eq!(diagnostic["reason"], "unclassified");
        assert!(diagnostic.get("modelFailure").is_none());
    }

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

    #[test]
    fn diagnostics_recognize_the_durable_cancellation_failure() {
        let error = "fatal_execution_outcome:execution_cancellation_indeterminate";
        let value = record("run", "core_turn", error);
        assert_eq!(value["reason"], "execution_cancellation_indeterminate");
        assert_eq!(value["errorBytes"], 60);
        assert_eq!(
            value["errorDigest"],
            "sha256:3ea336e486e5880a897fa9ce96e04903f7b4e42ccfad1b48fa7ad3ddf04b11ea"
        );
    }

    #[test]
    fn diagnostics_keep_cancellation_boundaries_without_raw_details() {
        for stage in [
            "execution_command_spawn",
            "execution_command_cancel",
            "execution_command_deadline",
            "execution_command_poll",
            "execution_command_output",
        ] {
            let value = record(
                "run",
                stage,
                "execution cancellation indeterminate: SECRET_TOKEN private tool output",
            );
            assert_eq!(value["reason"], "execution_cancellation_indeterminate");
            assert_eq!(value["stage"], stage);
            assert!(!value.to_string().contains("SECRET_TOKEN"));
            assert!(!value.to_string().contains("private tool output"));
        }
    }

    #[test]
    fn diagnostics_retain_validated_hosted_model_failure_metadata() {
        let failure = serde_json::json!({
            "schema": "hosted.model.failure.v1",
            "reasonType": "provider_unavailable",
            "httpStatus": 503,
            "providerAttempts": 6,
            "retryExhausted": true,
        });
        let error =
            format!("model_client_error(kind=provider_unavailable): hosted_model_error: {failure}");
        let value = record("run", "core_turn", &error);
        assert_eq!(value["reason"], "provider_unavailable");
        assert_eq!(value["modelFailure"], failure);

        let mut invalid = failure;
        invalid["rawProviderOutput"] = "SECRET_TOKEN private model text".into();
        let error = format!("hosted_model_error: {invalid}");
        let value = record("run", "core_turn", &error);
        assert_eq!(value["reason"], "unclassified");
        assert!(value.get("modelFailure").is_none());
        assert!(!value.to_string().contains("SECRET_TOKEN"));
        assert!(!value.to_string().contains("private model text"));
    }
}

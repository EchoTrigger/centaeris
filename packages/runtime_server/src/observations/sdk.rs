//! Strict numeric SDK diagnostics; provider messages and request payloads are discarded.
use serde::Deserialize;
use serde_json::Value;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SdkTiming {
    schema: String,
    stage: String,
    value: f64,
    outcome: String,
    request_id: Value,
    operation: Value,
}
fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"_.:-".contains(&value))
}
fn parse(line: &[u8]) -> Option<Value> {
    if line.len() > 2048 {
        return None;
    }
    let timing: SdkTiming = serde_json::from_slice(line).ok()?;
    if !timing.value.is_finite()
        || timing.value < 0.0
        || timing.value > u64::MAX as f64
        || !matches!(timing.outcome.as_str(), "ok" | "error")
    {
        return None;
    }
    let operation = if timing.schema == "runtime.e2b.power_timing.v1"
        && matches!(timing.stage.as_str(), "sdkImport" | "sdkClientSetup")
        && timing.request_id.is_null()
        && timing.operation.is_null()
    {
        "startup"
    } else {
        if timing.schema != "runtime.e2b.sdk_timing.v1"
            || !timing.request_id.as_str().is_some_and(identity)
        {
            return None;
        }
        let operation = timing.operation.as_str()?;
        let valid = match timing.stage.as_str() {
            "sdkCreate" | "sdkCreateVerifyGet" => operation == "create",
            "sdkCommandStart" => operation == "runCommand",
            "sdkCommandWait" => matches!(operation, "waitCommand" | "recoverCommand"),
            "sdkFileUpload" => operation == "uploadFile",
            "sdkFileDownload" => operation == "downloadFile",
            "sdkHandle" => matches!(
                operation,
                "create"
                    | "runCommand"
                    | "waitCommand"
                    | "recoverCommand"
                    | "uploadFile"
                    | "downloadFile"
            ),
            _ => false,
        };
        if !valid {
            return None;
        }
        operation
    };
    Some(
        serde_json::json!({"source":"sdk","phase":"e2bSdk","stage":timing.stage,"operation":operation,"value":timing.value,"outcome":timing.outcome}),
    )
}
pub(crate) fn forward_sdk_line(line: &[u8], publish: bool) -> bool {
    if !super::enabled() {
        return false;
    }
    let Some(event) = parse(line) else {
        return false;
    };
    if publish {
        // The SDK lane can outlive its creator Run. Until a diagnostic request
        // has a verified per-call binding, its spawn Job cannot own this record.
        let _unassigned = (event["operation"] != "startup").then(|| super::Context::enter(b"{}"));
        super::emit_sdk(event);
    }
    true
}
pub(crate) fn dropped_records(count: u64) {
    if count > 0 {
        super::emit(
            serde_json::json!({"phase":"executionObservationLoss","metric":"sdkDroppedRecords","value":count}),
        );
    }
}

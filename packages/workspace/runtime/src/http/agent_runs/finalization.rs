//! Advance an ordinary per-Run Job from its committed terminal to platform release.
use crate::agent_run_execution::Failure;
use crate::HttpServerState;
use crate::{
    http_response, json_error_response, AGENT_RUN_LIFECYCLE_JOB_KIND, MAX_HTTP_BODY_BYTES,
};
use axum::{
    body::Body,
    http::{Request, Response},
};
use bytes::Bytes;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use std::{env, io::Read, time::Duration};

const REQUEST_SCHEMA: &str = "runtime.agent_run.finalize.v1";
const RESOLVE_SCHEMA: &str = "runtime.agent_run_lifecycle.finalize.v1";
const RESOLVED_SCHEMA: &str = "runtime.agent_run_lifecycle.resolved.v1";

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TerminalState {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FinalizeRequest {
    pub(crate) schema: String,
    pub(crate) job_id: String,
    pub(crate) lease_owner: String,
    pub(crate) agent_run_id: String,
    pub(crate) authorization_digest: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub(crate) terminal_state: Option<TerminalState>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub(crate) agent_run_cleanup: Option<Box<crate::contract::AgentRunCleanup>>,
}

enum FinalizeIntent {
    Resolve(TerminalState),
    Cleanup(Box<crate::contract::AgentRunCleanup>),
}

#[derive(Deserialize)]
#[serde(
    tag = "disposition",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum CleanupResolution {
    Terminal {
        schema: String,
        terminal_state: TerminalState,
        agent_run_cleanup: Box<crate::contract::AgentRunCleanup>,
    },
    Released {
        schema: String,
        terminal_state: TerminalState,
        release_receipt_id: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseReceipt {
    schema: String,
    agent_run_id: String,
    release_receipt_id: String,
}

pub(crate) fn valid_identity(value: &str) -> bool {
    (1..=160).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}

impl FinalizeRequest {
    fn valid(&self) -> bool {
        self.schema == REQUEST_SCHEMA
            && valid_identity(&self.agent_run_id)
            && (16..=160).contains(&self.lease_owner.len())
            && !self.lease_owner.chars().any(char::is_control)
            && valid_identity(&self.job_id)
            && self.job_id == format!("{AGENT_RUN_LIFECYCLE_JOB_KIND}:{}", self.agent_run_id)
            && crate::agent_run_authorization::validate_sha256(
                "authorizationDigest",
                &self.authorization_digest,
            )
            .is_ok()
    }

    fn take_intent(&mut self) -> Option<FinalizeIntent> {
        match (self.terminal_state, self.agent_run_cleanup.take()) {
            (Some(terminal), None) => Some(FinalizeIntent::Resolve(terminal)),
            (None, Some(cleanup)) => Some(FinalizeIntent::Cleanup(cleanup)),
            _ => None,
        }
    }
}

pub(crate) fn advance(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let request: FinalizeRequest = match serde_json::from_slice(request.body()) {
        Ok(request) => request,
        Err(_) => return json_error_response(400, "agent_run_finalize_request_invalid"),
    };
    let run = request.agent_run_id.clone();
    match finish(state, request) {
        Ok(receipt) => released(&run, &receipt),
        Err(error) => error.response(),
    }
}

pub(crate) fn finish(
    state: HttpServerState,
    mut request: FinalizeRequest,
) -> Result<String, Failure> {
    if !request.valid() {
        return Err(Failure::rejected(400, "agent_run_finalize_request_invalid"));
    }
    let Some(intent) = request.take_intent() else {
        return Err(Failure::rejected(400, "agent_run_finalize_request_invalid"));
    };
    if let Err(error) = state
        .job_store
        .require_execute_lease(&request.job_id, &request.lease_owner)
    {
        return if error == "ordinary execution lease rejected" {
            Err(PlatformFailure::LeaseLost.failure())
        } else {
            Err(Failure::rejected(
                503,
                "agent_run_finalize_lease_unavailable",
            ))
        };
    }
    let platform = match Platform::new() {
        Ok(platform) => platform,
        Err(error) => return Err(error.failure()),
    };
    let cleanup = match intent {
        FinalizeIntent::Cleanup(cleanup) => cleanup,
        FinalizeIntent::Resolve(expected_terminal) => {
            let resolution = match platform.resolve(&request, expected_terminal) {
                Ok(resolution) => resolution,
                Err(error) => return Err(error.failure()),
            };
            match resolution {
                CleanupResolution::Terminal {
                    schema,
                    terminal_state,
                    agent_run_cleanup,
                } if schema == RESOLVED_SCHEMA && terminal_state == expected_terminal => {
                    agent_run_cleanup
                }
                CleanupResolution::Released {
                    schema,
                    terminal_state,
                    release_receipt_id,
                } if schema == RESOLVED_SCHEMA
                    && terminal_state == expected_terminal
                    && valid_identity(&release_receipt_id) =>
                {
                    return Ok(release_receipt_id);
                }
                _ => return Err(PlatformFailure::Invalid.failure()),
            }
        }
    };
    let environment = &cleanup.environment_authorization.payload;
    if cleanup.agent_run_id != request.agent_run_id
        || cleanup.authorization_digest != request.authorization_digest
        || environment.execution_mode != "perRun"
    {
        return Err(Failure::rejected(
            409,
            "agent_run_finalize_binding_mismatch",
        ));
    }
    let Some(controller) = &environment.controller else {
        return Err(Failure::rejected(
            409,
            "agent_run_finalize_binding_mismatch",
        ));
    };
    let epoch = controller.controller_epoch;
    let receipt = crate::agent_run_cleanup::teardown(
        state,
        request.job_id.clone(),
        request.lease_owner.clone(),
        *cleanup,
    )?;
    platform
        .release(&request, epoch, &receipt)
        .map_err(|error| error.failure())
}

fn released(run: &str, receipt: &str) -> Result<Response<Body>, String> {
    Ok(http_response(
        200,
        "application/json",
        serde_json::to_vec(&json!({
            "schema": "runtime.agent_run.finalize.result.v1",
            "agentRunId": run,
            "releaseReceiptId": receipt,
        }))
        .map_err(|error| error.to_string())?,
    ))
}

#[derive(Debug)]
pub(crate) enum PlatformFailure {
    Unavailable,
    Invalid,
    ControllerBusy,
    ControllerOwned,
    LeaseLost,
}

impl PlatformFailure {
    pub(crate) fn failure(self) -> Failure {
        let (status, code) = match self {
            Self::Unavailable => (503, "agent_run_finalize_platform_unavailable"),
            Self::Invalid => (409, "agent_run_finalize_platform_rejected"),
            Self::ControllerBusy => (409, "environment_controller_busy"),
            Self::ControllerOwned => (409, "environment_controller_owned"),
            Self::LeaseLost => (409, "agent_run_lifecycle_lease_lost"),
        };
        Failure::rejected(status, code)
    }
}

#[derive(Clone)]
pub(crate) struct Platform {
    api_url: String,
    token: String,
    client: reqwest::blocking::Client,
}

impl Platform {
    pub(crate) fn new() -> Result<Self, PlatformFailure> {
        Ok(Self {
            api_url: env::var("API_INTERNAL_URL").map_err(|_| PlatformFailure::Unavailable)?,
            token: env::var("INTERNAL_API_TOKEN").map_err(|_| PlatformFailure::Unavailable)?,
            client: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| PlatformFailure::Unavailable)?,
        })
    }

    fn resolve(
        &self,
        request: &FinalizeRequest,
        terminal: TerminalState,
    ) -> Result<CleanupResolution, PlatformFailure> {
        self.post(
            "/internal/agent-run-lifecycle/finalize",
            json!({
                "schema": RESOLVE_SCHEMA,
                "jobId": request.job_id,
                "leaseOwner": request.lease_owner,
                "agentRunId": request.agent_run_id,
                "authorizationDigest": request.authorization_digest,
                "terminalState": terminal,
            }),
        )
    }

    pub(crate) fn release(
        &self,
        request: &FinalizeRequest,
        epoch: i64,
        cleanup_receipt: &str,
    ) -> Result<String, PlatformFailure> {
        let receipt: ReleaseReceipt = self.post(
            "/internal/agent-run-lifecycle/release",
            json!({
                "schema": "runtime.agent_run_lifecycle.release.v1",
                "jobId": request.job_id,
                "leaseOwner": request.lease_owner,
                "agentRunId": request.agent_run_id,
                "authorizationDigest": request.authorization_digest,
                "controllerEpoch": epoch,
                "cleanupReceiptId": cleanup_receipt,
            }),
        )?;
        if receipt.schema != "runtime.agent_run_lifecycle.released.v1"
            || receipt.agent_run_id != request.agent_run_id
            || !valid_identity(&receipt.release_receipt_id)
        {
            return Err(PlatformFailure::Invalid);
        }
        Ok(receipt.release_receipt_id)
    }

    pub(crate) fn post<T: DeserializeOwned>(
        &self,
        route: &str,
        body: serde_json::Value,
    ) -> Result<T, PlatformFailure> {
        let response = self
            .client
            .post(format!("{}{route}", self.api_url.trim_end_matches('/')))
            .header("X-Internal-Token", &self.token)
            .json(&body)
            .send()
            .map_err(|_| PlatformFailure::Unavailable)?;
        let status = response.status();
        if status.is_server_error() {
            return Err(PlatformFailure::Unavailable);
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_HTTP_BODY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PlatformFailure::Unavailable)?;
        if bytes.len() > MAX_HTTP_BODY_BYTES {
            return Err(PlatformFailure::Invalid);
        }
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|_| PlatformFailure::Invalid);
        }
        let failure = if status.as_u16() == 409 {
            let body = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
            match body
                .as_ref()
                .and_then(|body| body.get("error"))
                .and_then(|code| code.as_str())
            {
                Some("environment_controller_busy") => PlatformFailure::ControllerBusy,
                Some("environment_controller_owned") => PlatformFailure::ControllerOwned,
                Some("environment_controller_lease_rejected") => PlatformFailure::LeaseLost,
                _ => PlatformFailure::Invalid,
            }
        } else {
            PlatformFailure::Invalid
        };
        Err(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_requires_exact_terminal_intent_and_an_explicit_cleanup_field() {
        let mut body = json!({
            "schema": REQUEST_SCHEMA,
            "jobId": "agent_run.lifecycle:agent_run_fixture",
            "leaseOwner": "worker:finalization-fixture",
            "agentRunId": "agent_run_fixture",
            "authorizationDigest": format!("sha256:{}", "a".repeat(64)),
            "terminalState": "completed",
            "agentRunCleanup": null,
        });
        let mut request = serde_json::from_value::<FinalizeRequest>(body.clone()).unwrap();
        assert!(request.valid());
        assert!(matches!(
            request.take_intent(),
            Some(FinalizeIntent::Resolve(TerminalState::Completed))
        ));

        let mut no_intent = body.clone();
        no_intent["terminalState"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<FinalizeRequest>(no_intent)
            .unwrap()
            .take_intent()
            .is_none());

        body["jobId"] = json!("agent_run.lifecycle:another_run");
        assert!(!serde_json::from_value::<FinalizeRequest>(body.clone())
            .unwrap()
            .valid());

        body.as_object_mut().unwrap().remove("agentRunCleanup");
        assert!(serde_json::from_value::<FinalizeRequest>(body).is_err());
    }

    #[test]
    fn finalize_accepts_release_receipt_and_rejects_execution_resolution() {
        let body = json!({
            "schema": RESOLVED_SCHEMA,
            "disposition": "released",
            "terminalState": "cancelled",
            "releaseReceiptId": "release_fixture",
        });
        assert!(matches!(
            serde_json::from_value::<CleanupResolution>(body).unwrap(),
            CleanupResolution::Released {
                terminal_state: TerminalState::Cancelled,
                ..
            }
        ));
        assert!(serde_json::from_value::<CleanupResolution>(json!({
            "schema": RESOLVED_SCHEMA,
            "disposition": "ready",
            "agentRunStart": {},
        }))
        .is_err());
    }
}

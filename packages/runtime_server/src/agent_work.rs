//! API owns private coordination identity and current resource authorization.
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
};
use centaeris_core::tool::{DynamicToolContract, DynamicToolRegistry, ToolContract};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;

pub(crate) const PROVIDER_ID: &str = "workspace.agent_work";
const CONTRACT: &str = include_str!("../../api/app_core/contracts/dispatch_work.json");
const QUERY_CONTRACT: &str = include_str!("../../api/app_core/contracts/get_work_request.json");
const CONFIRM_CONTRACT: &str =
    include_str!("../../api/app_core/contracts/confirm_work_return.json");

pub(crate) fn work_api_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "work API client failed".into())
}

pub(crate) fn work_tool_contract() -> DynamicToolContract {
    serde_json::from_str(CONTRACT).expect("first-party work contract")
}

pub(crate) fn work_query_tool_contract() -> DynamicToolContract {
    serde_json::from_str(QUERY_CONTRACT).expect("first-party work query contract")
}

pub(crate) fn work_confirmation_tool_contract() -> DynamicToolContract {
    serde_json::from_str(CONFIRM_CONTRACT).expect("first-party work confirmation contract")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkConfirmationArgs {
    pub(crate) notice_id: String,
    pub(crate) attempt_id: String,
}

impl WorkConfirmationArgs {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if [&self.notice_id, &self.attempt_id].iter().any(|id| {
            id.trim().is_empty()
                || id.chars().count() > 160
                || id.contains('/')
                || id.contains('\\')
        }) {
            return Err(
                "confirm_work_return requires exact opaque notice and attempt identities".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn digest(&self) -> Result<String, String> {
        let mut hash = Sha256::new();
        hash.update(b"workspace.agent_work.return_confirm.input.v1\0");
        hash.update(
            serde_json::to_vec(&serde_json::to_value(self).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
        Ok(format!("sha256:{:x}", hash.finalize()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkConfirmationRecord {
    pub(crate) schema: String,
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
    pub(crate) agent_run_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) authorization_digest: String,
    pub(crate) input_digest: String,
    pub(crate) notice_id: String,
    pub(crate) attempt_id: String,
    pub(crate) notice_identity: serde_json::Value,
}

fn valid_notice_identity(
    identity: &serde_json::Value,
    auth: &WorkspaceAgentRunAuthorization,
) -> bool {
    let Some(fields) = identity.as_object() else {
        return false;
    };
    let names = [
        "agentId",
        "coordinationSessionId",
        "userId",
        "workspaceId",
        "sourceAgentRunId",
        "sourceTurnId",
        "sourceToolCallId",
        "sourceEventId",
        "operationId",
        "workSessionId",
        "workAgentRunId",
        "factKind",
        "factRef",
        "factDigest",
    ];
    fields.len() == names.len()
        && names.iter().all(|name| fields.contains_key(*name))
        && names.iter().filter(|name| **name != "userId").all(|name| {
            fields[*name]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
        })
        && identity["agentId"].as_str() == Some(auth.agent_id.as_str())
        && identity["coordinationSessionId"].as_str() == Some(auth.session_id.as_str())
        && identity["workspaceId"].as_str() == Some(auth.workspace_id.as_str())
        && identity["userId"]
            .as_i64()
            .is_some_and(|id| id.to_string() == auth.user_id)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkQueryArgs {
    source_agent_run_id: String,
    source_tool_call_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkQuerySource {
    source_agent_run_id: String,
    source_turn_id: String,
    source_tool_call_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    source_event_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    projects_to_agent_run_stream: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkQueryOperation {
    operation_id: String,
    command: String,
    status: String,
    session_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    agent_run_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    turn_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkQueryResource {
    available: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    agent_run_status: Option<String>,
    returns: Vec<WorkReturnQueryRecord>,
    #[serde(deserialize_with = "Option::deserialize")]
    output: Option<WorkOutput>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkOutput {
    schema: String,
    source: String,
    session_id: String,
    agent_run_id: String,
    event_id: String,
    through_sequence: u64,
    body: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkReturnQueryAttempt {
    attempt_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    agent_run_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkReturnConfirmationSource {
    event_id: String,
    call_event_id: String,
    sequence: u64,
    agent_run_id: String,
    turn_id: String,
    tool_call_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkReturnQueryRecord {
    notice: serde_json::Value,
    #[serde(deserialize_with = "Option::deserialize")]
    attempt: Option<WorkReturnQueryAttempt>,
    handled: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    confirmation: Option<WorkReturnConfirmationSource>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
enum WorkQueryStatus {
    NotRecorded,
    Pending,
    Admitted,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkQueryRecord {
    schema: String,
    agent_id: String,
    session_id: String,
    agent_run_id: String,
    tool_call_id: String,
    authorization_digest: String,
    source_agent_run_id: String,
    source_tool_call_id: String,
    status: WorkQueryStatus,
    #[serde(deserialize_with = "Option::deserialize")]
    source: Option<WorkQuerySource>,
    #[serde(deserialize_with = "Option::deserialize")]
    operation: Option<WorkQueryOperation>,
    #[serde(deserialize_with = "Option::deserialize")]
    work: Option<WorkQueryResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkArgs {
    objective: String,
    session_refs: Vec<String>,
    file_refs: Vec<String>,
}

impl WorkArgs {
    fn validate(&self) -> Result<(), String> {
        if self.objective.trim().is_empty() || self.objective.len() > 65_536 {
            return Err(
                "dispatch_work.objective must contain text and fit 65536 UTF-8 bytes".into(),
            );
        }
        for refs in [&self.session_refs, &self.file_refs] {
            if refs.len() > 8
                || refs
                    .iter()
                    .any(|item| item.trim().is_empty() || item.contains('/') || item.contains('\\'))
            {
                return Err(
                    "dispatch_work refs must contain at most eight opaque resource identities"
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn digest(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|_| "encode work input failed")?;
        // These model arguments contain only strings and arrays; Value sorts
        // object keys, matching the API's canonical input digest.
        let bytes = serde_json::to_vec(&value).map_err(|_| "encode work input failed")?;
        let mut hash = Sha256::new();
        hash.update(b"workspace.agent_work.input.v1\0");
        hash.update(bytes);
        Ok(format!("sha256:{:x}", hash.finalize()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValidationRecord {
    schema: String,
    agent_id: String,
    session_id: String,
    agent_run_id: String,
    tool_call_id: String,
    authorization_digest: String,
    input_digest: String,
}

pub(crate) struct WorkspaceAgentWorkProvider {
    http: reqwest::Client,
    api_url: String,
    internal_token: String,
    agent_id: String,
    session_id: String,
    agent_run_id: String,
    authorization_digest: String,
    authorization: WorkspaceAgentRunAuthorization,
    contract: ToolContract,
    query_contract: ToolContract,
    confirmation_contract: ToolContract,
}

impl WorkspaceAgentWorkProvider {
    pub(crate) fn new(
        api_url: String,
        internal_token: String,
        authorization: &WorkspaceAgentRunAuthorization,
        authorization_digest: String,
    ) -> Result<Self, String> {
        let http = work_api_client()?;
        let registry = DynamicToolRegistry::from_contracts(vec![
            work_tool_contract(),
            work_query_tool_contract(),
            work_confirmation_tool_contract(),
        ])?;
        let contract = registry
            .find_contract("dispatch_work")
            .ok_or("work contract missing")?;
        Ok(Self {
            http,
            api_url,
            internal_token,
            agent_id: authorization.agent_id.clone(),
            session_id: authorization.session_id.clone(),
            agent_run_id: authorization.agent_run_id.clone(),
            authorization_digest,
            authorization: authorization.clone(),
            contract,
            query_contract: registry
                .find_contract("get_work_request")
                .ok_or("work query contract missing")?,
            confirmation_contract: registry
                .find_contract("confirm_work_return")
                .ok_or("work confirmation contract missing")?,
        })
    }

    async fn query(
        &self,
        request: DynamicToolProviderRequest,
    ) -> Result<DynamicToolProviderResponse, String> {
        if request.contract != self.query_contract || request.tool_call_id.trim().is_empty() {
            return Err("workspace work query identity mismatch".into());
        }
        let args: WorkQueryArgs = serde_json::from_str(&request.args_json)
            .map_err(|_| "get_work_request input is invalid")?;
        if [&args.source_agent_run_id, &args.source_tool_call_id]
            .iter()
            .any(|id| {
                id.trim().is_empty()
                    || id.chars().count() > 160
                    || id.contains('/')
                    || id.contains('\\')
            })
        {
            return Err("get_work_request requires opaque source identities".into());
        }
        let response = self.http.post(format!("{}/internal/agent-work/query", self.api_url.trim_end_matches('/')))
            .header("X-Internal-Token", &self.internal_token)
            .json(&json!({"schema": "workspace.agent_work.query.v1", "agentRunId": self.agent_run_id,
                "authorizationDigest": self.authorization_digest, "coordinationSessionId": self.session_id,
                "toolCallId": request.tool_call_id, "sourceAgentRunId": args.source_agent_run_id,
                "sourceToolCallId": args.source_tool_call_id}))
            .send().await.map_err(|_| "work query authority unavailable")?;
        if !response.status().is_success() {
            return Err(format!(
                "work query authority rejected: {}",
                response.status().as_u16()
            ));
        }
        let record: WorkQueryRecord = response
            .json()
            .await
            .map_err(|_| "work query response invalid")?;
        if record.schema != "workspace.agent_work.query_result.v1"
            || record.agent_id != self.agent_id
            || record.session_id != self.session_id
            || record.agent_run_id != self.agent_run_id
            || record.tool_call_id != request.tool_call_id
            || record.authorization_digest != self.authorization_digest
            || record.source_agent_run_id != args.source_agent_run_id
            || record.source_tool_call_id != args.source_tool_call_id
            || record.source.as_ref().is_some_and(|source| {
                source.source_agent_run_id != args.source_agent_run_id
                    || source.source_tool_call_id != args.source_tool_call_id
                    || source.source_turn_id.trim().is_empty()
                    || source
                        .source_event_id
                        .as_ref()
                        .is_some_and(|id| id.trim().is_empty())
            })
        {
            return Err("work query response identity mismatch".into());
        }
        let committed = record
            .source
            .as_ref()
            .is_some_and(|source| source.source_event_id.is_some());
        let coherent = match record.status {
            WorkQueryStatus::NotRecorded => {
                !committed && record.operation.is_none() && record.work.is_none()
            }
            WorkQueryStatus::Pending => {
                committed && record.operation.is_none() && record.work.is_none()
            }
            WorkQueryStatus::Admitted => {
                committed
                    && record.work.as_ref().is_some_and(|work| {
                        if work.available {
                            work.agent_run_status
                                .as_ref()
                                .is_some_and(|s| !s.trim().is_empty())
                        } else {
                            work.agent_run_status.is_none()
                        }
                    })
                    && record.operation.as_ref().is_some_and(|op| {
                        op.command == "submitMessage"
                            && op.status == "accepted"
                            && !op.operation_id.trim().is_empty()
                            && !op.session_id.trim().is_empty()
                            && op
                                .agent_run_id
                                .as_ref()
                                .is_some_and(|id| !id.trim().is_empty())
                            && op.turn_id.as_ref().is_some_and(|id| !id.trim().is_empty())
                    })
            }
        };
        if !coherent {
            return Err("work query response state mismatch".into());
        }
        if let Some(work) = &record.work {
            if work.output.as_ref().is_some_and(|output| {
                !work.available
                    || output.schema != "workspace.agent_work.output.v1"
                    || output.source != "untrustedWorkOutput"
                    || output.event_id.trim().is_empty()
                    || output.through_sequence == 0
                    || output.body.trim().is_empty()
                    || record.operation.as_ref().is_none_or(|op| {
                        output.session_id != op.session_id
                            || Some(output.agent_run_id.as_str()) != op.agent_run_id.as_deref()
                    })
            }) {
                return Err("work output identity mismatch".into());
            }
            for view in &work.returns {
                let identity = &view.notice["identity"];
                if !work.available
                    || !valid_notice_identity(identity, &self.authorization)
                    || view.notice["schema"] != "workspace.agent_work.return_notice.v1"
                    || view.notice["source"] != "workspace.agent_work.return"
                    || view.notice["noticeId"]
                        .as_str()
                        .is_none_or(|id| id.trim().is_empty())
                    || identity["sourceAgentRunId"].as_str()
                        != Some(args.source_agent_run_id.as_str())
                    || identity["sourceToolCallId"].as_str()
                        != Some(args.source_tool_call_id.as_str())
                    || record.operation.as_ref().is_none_or(|op| {
                        identity["workAgentRunId"].as_str() != op.agent_run_id.as_deref()
                            || identity["workSessionId"].as_str() != Some(op.session_id.as_str())
                    })
                    || view.handled != view.confirmation.is_some()
                    || view
                        .attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.attempt_id.trim().is_empty())
                    || view.confirmation.as_ref().is_some_and(|source| {
                        source.sequence == 0
                            || [
                                &source.event_id,
                                &source.call_event_id,
                                &source.agent_run_id,
                                &source.turn_id,
                                &source.tool_call_id,
                            ]
                            .iter()
                            .any(|id| id.trim().is_empty())
                            || view.attempt.as_ref().is_none_or(|attempt| {
                                attempt.agent_run_id.as_deref()
                                    != Some(source.agent_run_id.as_str())
                            })
                    })
                {
                    return Err("work return query identity mismatch".into());
                }
            }
        }
        let details = serde_json::to_value(record).map_err(|_| "encode work query failed")?;
        Ok(DynamicToolProviderResponse {
            content: serde_json::to_string(&details).map_err(|_| "encode work query failed")?,
            details,
            is_error: false,
            facts: vec![],
            transition_reason: None,
        })
    }

    async fn confirm(
        &self,
        request: DynamicToolProviderRequest,
    ) -> Result<DynamicToolProviderResponse, String> {
        if request.contract != self.confirmation_contract || request.tool_call_id.trim().is_empty()
        {
            return Err("workspace work confirmation identity mismatch".into());
        }
        let args: WorkConfirmationArgs = serde_json::from_str(&request.args_json)
            .map_err(|_| "confirm_work_return input is invalid")?;
        args.validate()?;
        let response = self.http.post(format!("{}/internal/agent-work/returns/confirm/validate", self.api_url.trim_end_matches('/')))
            .header("X-Internal-Token", &self.internal_token)
            .json(&json!({"schema":"workspace.agent_work.return_confirm.validate.v1", "agentRunId":self.agent_run_id,
                "authorizationDigest":self.authorization_digest, "coordinationSessionId":self.session_id,
                "toolCallId":request.tool_call_id, "noticeId":args.notice_id, "attemptId":args.attempt_id}))
            .send().await.map_err(|_| "work confirmation authority unavailable")?;
        if !response.status().is_success() {
            return Err(format!(
                "work confirmation authority rejected: {}",
                response.status().as_u16()
            ));
        }
        let record: WorkConfirmationRecord = response
            .json()
            .await
            .map_err(|_| "work confirmation response invalid")?;
        if record.schema != "workspace.agent_work.return_confirm.validated.v1"
            || record.agent_id != self.agent_id
            || record.session_id != self.session_id
            || record.agent_run_id != self.agent_run_id
            || record.tool_call_id != request.tool_call_id
            || record.authorization_digest != self.authorization_digest
            || record.input_digest != args.digest()?
            || record.notice_id != args.notice_id
            || record.attempt_id != args.attempt_id
            || !valid_notice_identity(&record.notice_identity, &self.authorization)
        {
            return Err("work confirmation response identity mismatch".into());
        }
        let details =
            serde_json::to_value(record).map_err(|_| "encode work confirmation failed")?;
        // This receipt only validates intent. Handled is projected after Core's
        // matching canonical call/result passes the fenced Session commit.
        Ok(DynamicToolProviderResponse {
            content: serde_json::to_string(&details)
                .map_err(|_| "encode work confirmation failed")?,
            details,
            is_error: false,
            facts: vec![],
            transition_reason: None,
        })
    }
}

impl DynamicToolProvider for WorkspaceAgentWorkProvider {
    fn provider_id(&self) -> &str {
        PROVIDER_ID
    }

    fn execute<'a>(
        &'a self,
        request: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            if request.tool_name == "get_work_request" {
                return self.query(request).await;
            }
            if request.tool_name == "confirm_work_return" {
                return self.confirm(request).await;
            }
            if request.tool_name != "dispatch_work"
                || request.contract != self.contract
                || request.tool_call_id.trim().is_empty()
            {
                return Err("workspace work tool identity mismatch".into());
            }
            let args: WorkArgs = serde_json::from_str(&request.args_json)
                .map_err(|_| "dispatch_work input is invalid")?;
            args.validate()?;
            let input_digest = args.digest()?;
            let response = self.http.post(format!("{}/internal/agent-work/validate", self.api_url.trim_end_matches('/')))
                .header("X-Internal-Token", &self.internal_token)
                .json(&json!({"schema": "workspace.agent_work.validate.v1", "agentRunId": self.agent_run_id,
                    "authorizationDigest": self.authorization_digest, "coordinationSessionId": self.session_id,
                    "toolCallId": request.tool_call_id, "objective": args.objective,
                    "sessionRefs": args.session_refs, "fileRefs": args.file_refs}))
                .send().await.map_err(|_| "work authority unavailable")?;
            if !response.status().is_success() {
                return Err(format!(
                    "work authority rejected: {}",
                    response.status().as_u16()
                ));
            }
            let validated: ValidationRecord = response
                .json()
                .await
                .map_err(|_| "work authority response invalid")?;
            if validated.schema != "workspace.agent_work.validated.v1"
                || validated.agent_id != self.agent_id
                || validated.session_id != self.session_id
                || validated.agent_run_id != self.agent_run_id
                || validated.tool_call_id != request.tool_call_id
                || validated.authorization_digest != self.authorization_digest
                || validated.input_digest != input_digest
            {
                return Err("work authority response identity mismatch".into());
            }
            // Validation has no outgoing write. Core's next durable receipt safe
            // point commits the call's success before the next model request.
            Ok(DynamicToolProviderResponse {
                content: "Work request validated; awaiting Session commit, not admitted. Continue working; finish with Final without repeating this request.".into(),
                details: serde_json::to_value(validated).map_err(|_| "encode work validation failed")?,
                is_error: false, facts: vec![], transition_reason: None,
            })
        })
    }
}

#[cfg(test)]
#[path = "agent_work_tests.rs"]
mod tests;

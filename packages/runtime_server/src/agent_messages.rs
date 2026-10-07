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

pub(crate) const PROVIDER_ID: &str = "workspace.agent_messages";
const CONTRACT: &str = include_str!("../../api/app_core/contracts/send_message.json");

pub(crate) fn message_tool_contract() -> DynamicToolContract {
    serde_json::from_str(CONTRACT).expect("first-party message contract")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageArgs {
    body: String,
    session_refs: Vec<String>,
    file_refs: Vec<String>,
}

impl MessageArgs {
    fn validate(&self) -> Result<(), String> {
        if self.body.trim().is_empty() || self.body.len() > 65_536 {
            return Err("send_message.body must contain text and fit 65536 UTF-8 bytes".into());
        }
        for refs in [&self.session_refs, &self.file_refs] {
            if refs.len() > 8
                || refs
                    .iter()
                    .any(|item| item.trim().is_empty() || item.contains('/') || item.contains('\\'))
            {
                return Err(
                    "send_message refs must contain at most eight opaque resource identities"
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn digest(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|_| "encode message input failed")?;
        // These model arguments contain only strings and arrays; Value sorts
        // object keys, matching the API's canonical input digest.
        let bytes = serde_json::to_vec(&value).map_err(|_| "encode message input failed")?;
        let mut hash = Sha256::new();
        hash.update(b"workspace.agent_message.input.v1\0");
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

pub(crate) struct WorkspaceAgentMessageProvider {
    http: reqwest::Client,
    api_url: String,
    internal_token: String,
    agent_id: String,
    session_id: String,
    agent_run_id: String,
    authorization_digest: String,
    contract: ToolContract,
}

impl WorkspaceAgentMessageProvider {
    pub(crate) fn new(
        api_url: String,
        internal_token: String,
        authorization: &WorkspaceAgentRunAuthorization,
        authorization_digest: String,
    ) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "message API client failed")?;
        let contract = DynamicToolRegistry::from_contracts(vec![message_tool_contract()])?
            .find_contract("send_message")
            .ok_or("message contract missing")?;
        Ok(Self {
            http,
            api_url,
            internal_token,
            agent_id: authorization.agent_id.clone(),
            session_id: authorization.session_id.clone(),
            agent_run_id: authorization.agent_run_id.clone(),
            authorization_digest,
            contract,
        })
    }
}

impl DynamicToolProvider for WorkspaceAgentMessageProvider {
    fn provider_id(&self) -> &str {
        PROVIDER_ID
    }

    fn execute<'a>(
        &'a self,
        request: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            if request.tool_name != "send_message"
                || request.contract != self.contract
                || request.tool_call_id.trim().is_empty()
            {
                return Err("workspace message tool identity mismatch".into());
            }
            let args: MessageArgs = serde_json::from_str(&request.args_json)
                .map_err(|_| "send_message input is invalid")?;
            args.validate()?;
            let input_digest = args.digest()?;
            let response = self.http.post(format!("{}/internal/agent-messages/validate", self.api_url.trim_end_matches('/')))
                .header("X-Internal-Token", &self.internal_token)
                .json(&json!({"schema": "workspace.agent_message.validate.v1", "agentRunId": self.agent_run_id,
                    "authorizationDigest": self.authorization_digest, "coordinationSessionId": self.session_id,
                    "toolCallId": request.tool_call_id, "body": args.body,
                    "sessionRefs": args.session_refs, "fileRefs": args.file_refs}))
                .send().await.map_err(|_| "message authority unavailable")?;
            if !response.status().is_success() {
                return Err(format!(
                    "message authority rejected: {}",
                    response.status().as_u16()
                ));
            }
            let validated: ValidationRecord = response
                .json()
                .await
                .map_err(|_| "message authority response invalid")?;
            if validated.schema != "workspace.agent_message.validated.v1"
                || validated.agent_id != self.agent_id
                || validated.session_id != self.session_id
                || validated.agent_run_id != self.agent_run_id
                || validated.tool_call_id != request.tool_call_id
                || validated.authorization_digest != self.authorization_digest
                || validated.input_digest != input_digest
            {
                return Err("message authority response identity mismatch".into());
            }
            // Validation has no outgoing write. Core's next durable receipt safe
            // point commits the call's success before the next model request.
            Ok(DynamicToolProviderResponse {
                content: "Message accepted for Session commit. Continue working; finish with Final without repeating this message.".into(),
                details: serde_json::to_value(validated).map_err(|_| "encode message validation failed")?,
                is_error: false, facts: vec![], transition_reason: None,
            })
        })
    }
}

#[cfg(test)]
#[path = "agent_message_tests.rs"]
mod tests;

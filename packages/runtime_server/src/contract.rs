use centaeris_core::session::host_event_input::HostEventInput;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;

pub const AGENT_RUN_START_SCHEMA: &str = "workspace.agent_run.start.v1";
pub const AGENT_RUN_STEP_SCHEMA: &str = "runtime.agent_run.step.v1";
pub const AGENT_RUN_CANCEL_SCHEMA: &str = "runtime.agent_run.cancel.v1";
pub const AGENT_RUN_SUPPLEMENT_SCHEMA: &str = "runtime.agent_run.supplement.v1";
pub const AGENT_RUN_TEARDOWN_SCHEMA: &str = "runtime.agent_run.teardown.v1";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunTeardownEnvelope {
    pub schema: String,
    pub job_id: String,
    pub lease_owner: String,
    pub agent_run_cleanup: Box<AgentRunCleanup>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunCleanup {
    pub schema: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub authorization_signature: String,
    pub authorization: WorkspaceAgentRunAuthorization,
    pub environment_authorization: crate::environment_authorization::EnvironmentAuthorization,
}

impl AgentRunCleanup {
    pub fn validate(self, key: &[u8]) -> Result<Self, String> {
        if self.schema != "workspace.agent_run.cleanup.v1"
            || self
                .environment_authorization
                .payload
                .controller
                .as_ref()
                .is_none_or(|controller| controller.controller_purpose != "cleanup")
        {
            return Err("agent_run_cleanup_authority_invalid".into());
        }
        self.authorization
            .checked_binding(&self.agent_run_id)?
            .verify(
                &self.authorization_digest,
                key,
                &self.authorization_signature,
            )?;
        self.environment_authorization.verify(
            &self.authorization,
            &self.authorization_digest,
            key,
        )?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunStart {
    pub schema: String,
    pub agent_run_id: String,
    pub turn_id: String,
    pub prompt: String,
    pub initial_input: AgentRunStartInitialInput,
    pub agent_instructions: String,
    pub model_context_tokens: u32,
    pub model_max_output_tokens: u32,
    pub authorization_digest: String,
    pub authorization_signature: String,
    pub authorization: WorkspaceAgentRunAuthorization,
    pub environment_authorization: crate::environment_authorization::EnvironmentAuthorization,
    pub tail_action: AgentRunTailAction,
    pub coordination_session_id: Option<String>,
    pub native_coordination_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum AgentRunStartInitialInput {
    UserMessage {},
    UserInput {
        #[serde(rename = "inputId")]
        input_id: String,
        message: String,
        #[serde(rename = "attachmentRefs")]
        attachment_refs: Vec<String>,
    },
    HostEvent {
        #[serde(rename = "attemptId")]
        attempt_id: String,
        #[serde(rename = "noticeId")]
        notice_id: String,
        input: HostEventInput,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostEventInputRequest {
    pub schema: String,
    pub session_id: String,
    pub input_id: String,
    pub source: String,
    pub content: String,
}

impl HostEventInputRequest {
    pub fn create(self) -> Result<HostEventInput, String> {
        if self.schema != "runtime.host_event_input.create.v1" {
            return Err("host_event_input_schema_mismatch".into());
        }
        HostEventInput::new(&self.session_id, self.input_id, self.source, self.content)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum AgentRunTailAction {
    Append,
    RewriteLastUser {
        #[serde(rename = "targetMessageId")]
        target_message_id: String,
        #[serde(rename = "expectedTailMessageId")]
        expected_tail_message_id: String,
    },
}

impl AgentRunStart {
    pub fn validate(self, signing_key: &[u8]) -> Result<Self, String> {
        if self.schema != AGENT_RUN_START_SCHEMA
            || self
                .environment_authorization
                .payload
                .controller
                .as_ref()
                .is_none_or(|c| c.controller_purpose != "execute")
        {
            return Err("schema_or_controller_invalid".into());
        }
        for (name, value) in [
            ("agentRunId", self.agent_run_id.as_str()),
            ("turnId", self.turn_id.as_str()),
            ("authorizationDigest", self.authorization_digest.as_str()),
            (
                "authorizationSignature",
                self.authorization_signature.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{name} is required"));
            }
        }
        if self.turn_id == self.agent_run_id {
            return Err("turnId must differ from agentRunId".to_string());
        }
        validate_agent_instructions(self.agent_instructions.as_str())?;
        let authorization = self
            .authorization
            .checked_binding(self.agent_run_id.as_str())?;
        if self.model_context_tokens == 0
            || self.model_max_output_tokens == 0
            || self.model_max_output_tokens >= self.model_context_tokens
        {
            return Err("model token limits are invalid".to_string());
        }
        authorization.verify(
            &self.authorization_digest,
            signing_key,
            &self.authorization_signature,
        )?;
        self.environment_authorization.verify(
            &self.authorization,
            &self.authorization_digest,
            signing_key,
        )?;
        if self
            .coordination_session_id
            .as_ref()
            .is_some_and(|session_id| session_id != &self.authorization.session_id)
        {
            return Err("coordinationSessionId must match the authorized Session".to_string());
        }
        if self
            .native_coordination_session_id
            .as_ref()
            .is_some_and(|session_id| {
                Some(session_id) != self.coordination_session_id.as_ref()
                    || session_id != &self.authorization.session_id
            })
        {
            return Err(
                "nativeCoordinationSessionId must match the coordination and authorized Session"
                    .into(),
            );
        }
        if let AgentRunTailAction::RewriteLastUser {
            target_message_id,
            expected_tail_message_id,
        } = &self.tail_action
        {
            if target_message_id.trim().is_empty() || expected_tail_message_id.trim().is_empty() {
                return Err("rewrite tail message identities are required".to_string());
            }
        }
        if let AgentRunStartInitialInput::UserInput {
            input_id,
            message,
            attachment_refs,
        } = &self.initial_input
        {
            centaeris_core::session::supplement::validate_turn_supplement_id(input_id)
                .map_err(|error| error.to_string())?;
            let attachments = initial_input_attachments(&self)?;
            centaeris_core::session::turn_input::attachments::validate_user_input(
                message,
                &attachments,
            )?;
            if message != &self.prompt
                || self.coordination_session_id.is_none()
                || !matches!(self.tail_action, AgentRunTailAction::Append)
                || attachment_refs != &self.authorization.message_asset_refs
            {
                return Err("agent_input_initial_binding_rejected".into());
            }
        }
        if self.prompt.trim().is_empty()
            && !matches!(&self.initial_input,
            AgentRunStartInitialInput::UserInput { attachment_refs, .. } if !attachment_refs.is_empty())
        {
            return Err("prompt is required".into());
        }
        if let AgentRunStartInitialInput::HostEvent {
            attempt_id,
            notice_id,
            input,
        } = &self.initial_input
        {
            input.validate(&self.authorization.session_id)?;
            if attempt_id != &input.input_id
                || notice_id.trim().is_empty()
                || notice_id.len() > 96
                || notice_id.chars().any(char::is_control)
                || self.native_coordination_session_id.is_none()
                || !matches!(self.tail_action, AgentRunTailAction::Append)
                || !self.authorization.message_asset_refs.is_empty()
            {
                return Err("host_event_admission_binding_invalid".into());
            }
        }
        Ok(self)
    }
}

pub(crate) fn initial_input_attachments(
    start: &AgentRunStart,
) -> Result<Vec<centaeris_core::session::turn_input::attachments::UserInputAttachment>, String> {
    use centaeris_core::session::turn_input::attachments::UserInputAttachment;
    let AgentRunStartInitialInput::UserInput {
        attachment_refs, ..
    } = &start.initial_input
    else {
        return Ok(Vec::new());
    };
    if attachment_refs.len() > 50 || attachment_refs.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("agent_input_initial_attachment_refs_invalid".into());
    }
    attachment_refs
        .iter()
        .map(|input_ref| {
            let declared = start
                .authorization
                .asset_refs
                .iter()
                .find(|item| item.input_ref == *input_ref)
                .ok_or_else(|| "agent_input_initial_attachment_not_authorized".to_string())?;
            Ok(UserInputAttachment {
                input_ref: declared.input_ref.clone(),
                display_name: declared.display_name.clone(),
                content_type: declared.content_type.clone(),
            })
        })
        .collect()
}

fn validate_agent_instructions(value: &str) -> Result<(), String> {
    if value != value.trim()
        || value.chars().count() > 16_000
        || value.nfc().ne(value.chars())
        || value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err("agentInstructions is invalid".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunStepRequest {
    pub schema: String,
    pub job_id: String,
    pub lease_owner: String,
    pub agent_run_start: AgentRunStart,
}

impl AgentRunStepRequest {
    pub fn validate(self, signing_key: &[u8]) -> Result<Self, String> {
        if self.schema != AGENT_RUN_STEP_SCHEMA {
            return Err("agent_run_step_schema_mismatch".into());
        }
        if self.job_id.trim().is_empty()
            || self.lease_owner.len() < 16
            || self.lease_owner.len() > 160
            || self.lease_owner.chars().any(char::is_control)
        {
            return Err("agent_run_step_identity_invalid".to_string());
        }
        let mut request = self;
        request.agent_run_start = request.agent_run_start.validate(signing_key)?;
        Ok(request)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunControl {
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub authorization_signature: String,
    pub authorization: WorkspaceAgentRunAuthorization,
}

impl AgentRunControl {
    fn validate(&self, signing_key: &[u8]) -> Result<(), String> {
        self.authorization
            .checked_binding(&self.agent_run_id)?
            .verify(
                &self.authorization_digest,
                signing_key,
                &self.authorization_signature,
            )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunCancelRequest {
    pub schema: String,
    pub agent_run_control: AgentRunControl,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunSupplementRequest {
    pub schema: String,
    pub supplement_id: String,
    pub job_id: String,
    pub message: String,
    pub agent_run_control: AgentRunControl,
}

impl AgentRunSupplementRequest {
    pub fn validate(self, signing_key: &[u8]) -> Result<Self, String> {
        if self.schema != AGENT_RUN_SUPPLEMENT_SCHEMA {
            return Err("agent_run_supplement_schema_mismatch".to_string());
        }
        centaeris_core::session::supplement::validate_turn_supplement_id(
            self.supplement_id.as_str(),
        )
        .map_err(|error| error.to_string())?;
        centaeris_core::session::supplement::validate_turn_supplement_message(
            self.message.as_str(),
        )
        .map_err(|error| error.to_string())?;
        if self.job_id.trim().is_empty() {
            return Err("agent_run_supplement_job_id_required".to_string());
        }
        self.agent_run_control.validate(signing_key)?;
        let request = self;
        if request.job_id
            != centaeris_core::session::reliability::agent_run_lifecycle_job_id(
                request.agent_run_control.agent_run_id.as_str(),
            )?
        {
            return Err("agent_run_supplement_job_id_mismatch".to_string());
        }
        Ok(request)
    }
}

impl AgentRunCancelRequest {
    pub fn validate(self, signing_key: &[u8]) -> Result<Self, String> {
        if self.schema != AGENT_RUN_CANCEL_SCHEMA {
            return Err("agent_run_cancel_schema_mismatch".to_string());
        }
        self.agent_run_control.validate(signing_key)?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_authorization() -> serde_json::Value {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
        ))
        .unwrap();
        value["assetRefs"] = serde_json::json!([]);
        value["messageAssetRefs"] = serde_json::json!([]);
        value["thinkingMode"] = serde_json::Value::Null;
        value["sessionWorkspace"] = serde_json::json!({"generation":0,"snapshotSha256":"","snapshotSizeBytes":0,"expandedSizeBytes":0,"fileCount":0});
        value["pluginActivation"] = serde_json::to_value(
            centaeris_core::extension::build_plugin_activation_snapshot(&[]).unwrap(),
        )
        .unwrap();
        value
    }

    #[test]
    fn current_start_and_step_share_one_controller_contract_for_both_lifecycle_policies() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let key = b"test-run-authorization-signing-key";
        for mode in ["perRun", "retained"] {
            let mut start = valid_agent_run_start();
            start.schema = "workspace.agent_run.start.v1".into();
            start.environment_authorization.payload.execution_mode = mode.into();
            let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
            mac.update(b"workspace:environment-authorization:v1\0");
            mac.update(
                start
                    .environment_authorization
                    .payload
                    .digest()
                    .unwrap()
                    .as_bytes(),
            );
            start.environment_authorization.signature =
                format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
            start
                .clone()
                .validate(key)
                .expect("one current controller-bearing start");
            AgentRunStepRequest {
                schema: "runtime.agent_run.step.v1".into(),
                job_id: "job".into(),
                lease_owner: "owner-valid-00000001".into(),
                agent_run_start: start,
            }
            .validate(key)
            .unwrap();
        }
    }

    fn valid_agent_run_start_value() -> serde_json::Value {
        let authorization =
            serde_json::from_value::<WorkspaceAgentRunAuthorization>(valid_authorization())
                .expect("authorization");
        let digest = authorization.digest().expect("digest");
        let signature = authorization
            .signature(b"test-run-authorization-signing-key")
            .expect("signature");
        serde_json::json!({
            "schema": AGENT_RUN_START_SCHEMA,
            "agentRunId": "agent_run_1",
            "turnId": "turn_1",
            "prompt": "hello",
            "initialInput": {"type":"userMessage"},
            "agentInstructions": "Work precisely.",
            "modelContextTokens": 200000,
            "modelMaxOutputTokens": 32768,
            "authorizationDigest": digest,
            "authorizationSignature": signature,
            "environmentAuthorization": crate::environment_authorization::EnvironmentAuthorization::for_test(&authorization, b"test-run-authorization-signing-key"),
            "authorization": authorization,
            "tailAction": {"type": "append"}
        })
    }

    fn valid_agent_run_start() -> AgentRunStart {
        serde_json::from_value(valid_agent_run_start_value()).expect("run start")
    }

    fn valid_agent_run_control() -> AgentRunControl {
        let start = valid_agent_run_start();
        AgentRunControl {
            agent_run_id: start.agent_run_id,
            authorization_digest: start.authorization_digest,
            authorization_signature: start.authorization_signature,
            authorization: start.authorization,
        }
    }

    #[test]
    fn semantic_run_control_uses_the_signed_run_without_environment_execution_authority() {
        let start = valid_agent_run_start_value();
        let control = serde_json::json!({"agentRunId":start["agentRunId"],"authorizationDigest":start["authorizationDigest"],
            "authorizationSignature":start["authorizationSignature"],"authorization":start["authorization"]});
        let key = b"test-run-authorization-signing-key";
        serde_json::from_value::<AgentRunCancelRequest>(
            serde_json::json!({"schema":AGENT_RUN_CANCEL_SCHEMA,"agentRunControl":control}),
        )
        .unwrap()
        .validate(key)
        .unwrap();
        serde_json::from_value::<AgentRunSupplementRequest>(serde_json::json!({"schema":AGENT_RUN_SUPPLEMENT_SCHEMA,
            "agentRunControl":control,"supplementId":"supplement-1","jobId":"agent_run.lifecycle:agent_run_1","message":"continue"}))
            .unwrap().validate(key).unwrap();
        let mut changed = control.clone();
        changed["authorization"]["workspaceId"] = "other-workspace".into();
        let request = serde_json::from_value::<AgentRunCancelRequest>(
            serde_json::json!({"schema":AGENT_RUN_CANCEL_SCHEMA,"agentRunControl":changed}),
        )
        .unwrap();
        assert!(request.validate(key).is_err());
        let mut changed = control;
        changed["agentRunId"] = "other-run".into();
        let request = serde_json::from_value::<AgentRunCancelRequest>(
            serde_json::json!({"schema":AGENT_RUN_CANCEL_SCHEMA,"agentRunControl":changed}),
        )
        .unwrap();
        assert!(request.validate(key).is_err());
    }

    fn retained_start(purpose: &str) -> AgentRunStart {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut start = valid_agent_run_start();
        start.schema = AGENT_RUN_START_SCHEMA.into();
        let binding = &mut start.environment_authorization.payload;
        binding.schema = "workspace.environment_authorization.v1".into();
        binding.execution_mode = "retained".into();
        binding.controller = Some(crate::environment_authorization::ControllerBinding {
            controller_id: start.agent_run_id.clone(),
            controller_epoch: 1,
            controller_lease_digest: format!("sha256:{}", "a".repeat(64)),
            controller_purpose: purpose.into(),
        });
        let mut mac =
            Hmac::<Sha256>::new_from_slice(b"test-run-authorization-signing-key").unwrap();
        mac.update(b"workspace:environment-authorization:v1\0");
        mac.update(binding.digest().unwrap().as_bytes());
        start.environment_authorization.signature =
            format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
        start
    }

    #[test]
    fn cleanup_uses_only_original_grant_and_cleanup_authority() {
        let key = b"test-run-authorization-signing-key";
        let start = retained_start("cleanup");
        let mut value = serde_json::json!({
            "schema": "workspace.agent_run.cleanup.v1",
            "agentRunId": start.agent_run_id,
            "authorization": start.authorization,
            "authorizationDigest": start.authorization_digest,
            "authorizationSignature": start.authorization_signature,
            "environmentAuthorization": start.environment_authorization,
        });
        let cleanup: AgentRunCleanup = serde_json::from_value(value.clone()).unwrap();
        assert!(cleanup.validate(key).is_ok());
        value["prompt"] = serde_json::json!("must not rebuild model input to clean up");
        assert!(serde_json::from_value::<AgentRunCleanup>(value.clone()).is_err());
        value.as_object_mut().unwrap().remove("prompt");
        value["environmentAuthorization"] =
            serde_json::to_value(retained_start("execute").environment_authorization).unwrap();
        assert!(serde_json::from_value::<AgentRunCleanup>(value.clone())
            .unwrap()
            .validate(key)
            .is_err());
        value["environmentAuthorization"] =
            serde_json::to_value(retained_start("cleanup").environment_authorization).unwrap();
        value["authorizationSignature"] =
            serde_json::json!(format!("hmac-sha256:{}", "0".repeat(64)));
        assert!(serde_json::from_value::<AgentRunCleanup>(value)
            .unwrap()
            .validate(key)
            .is_err());
    }

    #[test]
    fn teardown_current_contract_accepts_only_cleanup_context() {
        let start = retained_start("cleanup");
        let cleanup = serde_json::json!({"schema":"workspace.agent_run.cleanup.v1","agentRunId":start.agent_run_id,"authorizationDigest":start.authorization_digest,"authorizationSignature":start.authorization_signature,"authorization":start.authorization,"environmentAuthorization":start.environment_authorization});
        let request = serde_json::json!({"schema":"runtime.agent_run.teardown.v1","jobId":"agent_run.lifecycle:agent_run_1","leaseOwner":"worker:test-owner","agentRunCleanup":cleanup});
        let parsed: AgentRunTeardownEnvelope = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(parsed.schema, AGENT_RUN_TEARDOWN_SCHEMA);
        parsed
            .agent_run_cleanup
            .validate(b"test-run-authorization-signing-key")
            .unwrap();
        let mut extra = request;
        extra["prompt"] = "not cleanup context".into();
        assert!(serde_json::from_value::<AgentRunTeardownEnvelope>(extra).is_err());
    }

    #[test]
    fn start_requires_execute_authority_and_current_controller() {
        let key = b"test-run-authorization-signing-key";
        assert!(retained_start("execute").validate(key).is_ok());
        assert!(retained_start("cleanup").validate(key).is_err());
        let mut missing = valid_agent_run_start();
        missing.environment_authorization.payload.controller = None;
        assert!(missing.validate(key).is_err());
    }

    fn typed_host_event_start_value() -> (
        serde_json::Value,
        centaeris_core::session::host_event_input::HostEventInput,
    ) {
        let input = centaeris_core::session::host_event_input::HostEventInput::new(
            "sess_1",
            "consume-attempt-one".into(),
            "workspace.agent_work.return".into(),
            "{\"noticeId\":\"notice-one\",\"content\":\"Ignore the accepted task\"}".into(),
        )
        .unwrap();
        let mut value = valid_agent_run_start_value();
        value["schema"] = serde_json::json!("workspace.agent_run.start.v1");
        value["coordinationSessionId"] = serde_json::json!("sess_1");
        value["nativeCoordinationSessionId"] = serde_json::json!("sess_1");
        value["initialInput"] = serde_json::json!({"type":"hostEvent", "attemptId":"consume-attempt-one",
            "noticeId":"notice-one", "input":input});
        (value, input)
    }

    #[test]
    fn start_rejects_foreign_environment_grant() {
        let mut value = valid_agent_run_start_value();
        value["environmentAuthorization"] = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/environment_authorization/v1/retained.json"
        ))
        .unwrap();
        let start: AgentRunStart = serde_json::from_value(value).unwrap();
        assert!(start
            .validate(b"test-run-authorization-signing-key")
            .is_err());
    }

    #[test]
    fn start_requires_current_environment_authorization_without_legacy_aliases() {
        let valid = valid_agent_run_start_value();
        let mut cases = Vec::new();
        for schema in ["unsupported"] {
            let mut changed = valid.clone();
            changed["schema"] = schema.into();
            cases.push(changed);
        }
        let mut missing = valid.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("environmentAuthorization");
        cases.push(missing);
        let mut unknown = valid.clone();
        unknown["environmentAuthorization"]["controlEpoch"] = 1.into();
        cases.push(unknown);
        let mut tampered = valid;
        tampered["environmentAuthorization"]["payload"]["environmentId"] = "foreign".into();
        cases.push(tampered);
        for value in cases {
            assert!(serde_json::from_value::<AgentRunStart>(value)
                .map(|start| start
                    .validate(b"test-run-authorization-signing-key")
                    .is_err())
                .unwrap_or(true));
        }
    }

    #[test]
    fn typed_user_initial_input_is_required_and_rejects_unknown_shapes() {
        let mut missing = valid_agent_run_start_value();
        missing.as_object_mut().unwrap().remove("initialInput");
        assert!(serde_json::from_value::<AgentRunStart>(missing)
            .map(|start| start
                .validate(b"test-run-authorization-signing-key")
                .is_err())
            .unwrap_or(true));
        let mut value = valid_agent_run_start_value();
        value["schema"] = serde_json::json!("workspace.agent_run.start.v1");
        value["initialInput"] = serde_json::json!({"type":"userMessage"});
        serde_json::from_value::<AgentRunStart>(value.clone())
            .unwrap()
            .validate(b"test-run-authorization-signing-key")
            .unwrap();
        for invalid in [
            serde_json::json!({"type":"toolContinuation"}),
            serde_json::json!({"type":"userMessage","message":"shadow objective"}),
        ] {
            value["initialInput"] = invalid;
            assert!(serde_json::from_value::<AgentRunStart>(value.clone()).is_err());
        }
    }

    #[test]
    fn typed_host_initial_input_uses_core_identity_and_canonical_record_origin() {
        let (value, input) = typed_host_event_start_value();
        let start = serde_json::from_value::<AgentRunStart>(value)
            .unwrap()
            .validate(b"test-run-authorization-signing-key")
            .unwrap();
        let mut ledger =
            centaeris_core::session::AgentRunSessionState::new("sess_1", "agent_run_1").unwrap();
        let records = crate::started_session_records(&start, &mut ledger, 10).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.event.event_type)
                .collect::<Vec<_>>(),
            vec![
                centaeris_core::session::SessionRecordType::AgentRunStarted,
                centaeris_core::session::SessionRecordType::HostEventInput
            ]
        );
        assert_eq!(
            records[0].event.payload,
            serde_json::json!({"userObjective":"hello"})
        );
        assert_eq!(
            records[1].event.payload,
            serde_json::to_value(&input).unwrap()
        );
        assert_eq!(
            records[1].event.agent_run_id.as_deref(),
            Some("agent_run_1")
        );
        let snapshot = centaeris_core::session::restore_runtime_snapshot_from_session_records(
            "sess_1",
            &records
                .into_iter()
                .map(|record| record.event)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            centaeris_core::session::host_event_input::host_event_origin(
                "sess_1",
                &snapshot.messages[0]
            )
            .unwrap(),
            Some(input)
        );
    }

    #[test]
    fn typed_host_initial_input_rejects_unknown_fields_foreign_ids_and_user_tail_actions() {
        let (valid, _input) = typed_host_event_start_value();
        let mut cases = Vec::new();
        let mut changed = valid.clone();
        changed["initialInput"]["input"]["messageId"] =
            serde_json::json!("message:agent_run_1:user");
        cases.push(changed);
        let mut changed = valid.clone();
        changed["initialInput"]["input"]["source_authority"] = serde_json::json!(true);
        cases.push(changed);
        let mut changed = valid.clone();
        changed["initialInput"]["attemptId"] = serde_json::json!("other-attempt");
        cases.push(changed);
        let mut changed = valid.clone();
        changed["tailAction"] = serde_json::json!({"type":"rewriteLastUser", "targetMessageId":"old",
            "expectedTailMessageId":"old"});
        cases.push(changed);
        let mut changed = valid;
        changed
            .as_object_mut()
            .unwrap()
            .remove("nativeCoordinationSessionId");
        cases.push(changed);
        for value in cases {
            assert!(serde_json::from_value::<AgentRunStart>(value)
                .map(|start| start
                    .validate(b"test-run-authorization-signing-key")
                    .is_err())
                .unwrap_or(true));
        }
    }

    #[test]
    fn agent_message_context_is_optional_and_bound_to_authorized_session() {
        let mut value = valid_agent_run_start_value();
        assert!(serde_json::from_value::<AgentRunStart>(value.clone()).is_ok());
        value["coordinationSessionId"] = serde_json::json!("sess_1");
        serde_json::from_value::<AgentRunStart>(value.clone())
            .expect("hosted coordination identity")
            .validate(b"test-run-authorization-signing-key")
            .expect("same authorized Session");
        value["coordinationSessionId"] = serde_json::json!("other-session");
        assert!(serde_json::from_value::<AgentRunStart>(value)
            .unwrap()
            .validate(b"test-run-authorization-signing-key")
            .is_err());
    }

    #[test]
    fn native_work_context_requires_the_same_coordination_and_authorized_session() {
        let mut value = valid_agent_run_start_value();
        value["nativeCoordinationSessionId"] = serde_json::json!("sess_1");
        assert!(serde_json::from_value::<AgentRunStart>(value.clone())
            .unwrap()
            .validate(b"test-run-authorization-signing-key")
            .is_err());
        value["coordinationSessionId"] = serde_json::json!("sess_1");
        assert!(serde_json::from_value::<AgentRunStart>(value.clone())
            .unwrap()
            .validate(b"test-run-authorization-signing-key")
            .is_ok());
        value["nativeCoordinationSessionId"] = serde_json::json!("foreign-session");
        assert!(serde_json::from_value::<AgentRunStart>(value)
            .unwrap()
            .validate(b"test-run-authorization-signing-key")
            .is_err());
    }

    #[test]
    fn authorization_error_precedence_is_stable() {
        let mut start = valid_agent_run_start();
        start.agent_run_id = "other_run".into();
        start.model_context_tokens = 0;
        assert!(start
            .validate(b"key")
            .unwrap_err()
            .contains("agentRunId mismatch"));
        let mut start = valid_agent_run_start();
        start.model_context_tokens = 0;
        start.authorization_digest = "wrong".into();
        assert_eq!(
            start.validate(b"key").unwrap_err(),
            "model token limits are invalid"
        );
        let mut start = valid_agent_run_start();
        start.authorization_digest = "wrong".into();
        start.authorization_signature = "wrong".into();
        assert!(start
            .validate(b"key")
            .unwrap_err()
            .contains("digest mismatch"));
    }

    #[test]
    fn agent_run_start_requires_authorization() {
        let payload = serde_json::json!({
            "schema": AGENT_RUN_START_SCHEMA,
            "agentRunId": "agent_run_1",
            "turnId": "turn_1",
            "prompt": "hello",
            "initialInput": {"type":"userMessage"},
            "agentInstructions": "",
            "modelContextTokens": 200000,
            "modelMaxOutputTokens": 32768
        });
        let error = serde_json::from_value::<AgentRunStart>(payload)
            .expect_err("missing authorization must fail");
        assert!(error.to_string().contains("authorizationDigest"));
    }

    #[test]
    fn agent_run_start_accepts_authorization_and_digest() {
        valid_agent_run_start()
            .validate(b"test-run-authorization-signing-key")
            .expect("validate run start");

        let mut payload = valid_agent_run_start_value();
        payload["tailAction"] = serde_json::json!({
            "type": "rewriteLastUser",
            "targetMessageId": "message:old:user",
            "expectedTailMessageId": "message:old:assistant"
        });
        let rewrite =
            serde_json::from_value::<AgentRunStart>(payload).expect("rewrite AgentRun start");
        assert!(matches!(
            rewrite.tail_action,
            AgentRunTailAction::RewriteLastUser { .. }
        ));
    }

    #[test]
    fn agent_run_start_rejects_non_canonical_agent_instructions() {
        let mut payload = valid_agent_run_start_value();
        payload["agentInstructions"] = serde_json::json!(" trailing ");
        let error = serde_json::from_value::<AgentRunStart>(payload)
            .expect("decode run start")
            .validate(b"test-run-authorization-signing-key")
            .expect_err("non-canonical Agent instructions must fail");
        assert_eq!(error, "agentInstructions is invalid");
    }

    #[test]
    fn agent_run_supplement_contract_is_strict_and_signed() {
        AgentRunSupplementRequest {
            schema: AGENT_RUN_SUPPLEMENT_SCHEMA.to_string(),
            supplement_id: "supplement-1".to_string(),
            job_id: "agent_run.lifecycle:agent_run_1".to_string(),
            message: "check the cancellation edge".to_string(),
            agent_run_control: valid_agent_run_control(),
        }
        .validate(b"test-run-authorization-signing-key")
        .expect("validate supplement request");

        let error = AgentRunSupplementRequest {
            schema: AGENT_RUN_SUPPLEMENT_SCHEMA.to_string(),
            supplement_id: "supplement-1".to_string(),
            job_id: "agent_run.lifecycle:banana".to_string(),
            message: "banana".to_string(),
            agent_run_control: valid_agent_run_control(),
        }
        .validate(b"test-run-authorization-signing-key")
        .expect_err("wrong lifecycle job identity must fail");
        assert_eq!(error, "agent_run_supplement_job_id_mismatch");

        let error = serde_json::from_value::<AgentRunSupplementRequest>(serde_json::json!({
            "schema": AGENT_RUN_SUPPLEMENT_SCHEMA,
            "supplementId": "supplement-1",
            "jobId": "agent_run.lifecycle:agent_run_1",
            "message": "banana",
            "agentRunControl": valid_agent_run_control(),
            "unexpected": true
        }))
        .expect_err("unknown supplement fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }
}

//! A create operation returns provider evidence, never readiness or execution authority.
use crate::agent_run_authorization::{
    canonical_json, EnvironmentProfile, ExecutionNetwork, WorkspaceAgentRunAuthorization,
};
use crate::e2b_adapter::{AdapterFailure, AdapterOutcome, AdapterProcess, AdapterResponse};
use crate::postgres_store::controller_authority::ControllerAuthorityRequest;
use crate::postgres_store::provider_operations::{
    FreshProviderCreationDispatchPermit, ProviderCreationDispatchState, ProviderCreationIdentity,
    ProviderCreationOperation, ProviderCreationRequest,
};
use crate::postgres_store::PostgresRuntimeStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::Duration;

pub trait E2bCreationAdapter: Send + Sync {
    fn create(
        &self,
        request_id: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<AdapterResponse, AdapterFailure>;
}

impl E2bCreationAdapter for AdapterProcess {
    fn create(
        &self,
        request_id: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        self.call(request_id, "create", payload, timeout)
    }
}

pub struct E2bCreationRequest<'a> {
    pub authorization: &'a WorkspaceAgentRunAuthorization,
    pub execution_id: &'a str,
    pub attempt_id: &'a str,
    pub idempotency_key: &'a str,
    pub configured_provider_config_digest: &'a str,
    pub transport_timeout: Duration,
}

#[derive(Debug)]
pub enum E2bCreationError {
    Rejected(&'static str),
    Store(String),
    CandidateNotRecorded {
        identity: Box<ProviderCreationIdentity>,
        sandbox_id: String,
    },
}

pub struct E2bCreationDriver<'a, A: E2bCreationAdapter + ?Sized> {
    store: &'a PostgresRuntimeStore,
    adapter: &'a A,
}

impl<'a, A: E2bCreationAdapter + ?Sized> E2bCreationDriver<'a, A> {
    pub fn new(store: &'a PostgresRuntimeStore, adapter: &'a A) -> Self {
        Self { store, adapter }
    }

    pub fn create_or_reopen(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        request: &E2bCreationRequest<'_>,
    ) -> Result<ProviderCreationOperation, E2bCreationError> {
        let planned = PlannedCreation::new(authority, request)?;
        if let Some(recorded) = self
            .store
            .provider_creation_operation(request.attempt_id)
            .map_err(E2bCreationError::Store)?
        {
            planned.require_identity(&recorded)?;
            if recorded.dispatch_state != ProviderCreationDispatchState::Prepared {
                return Ok(recorded);
            }
        }
        let operation = self
            .store
            .prepare_provider_creation(
                authority,
                &ProviderCreationRequest {
                    execution_id: request.execution_id,
                    attempt_id: request.attempt_id,
                    idempotency_key: request.idempotency_key,
                    request_digest: &planned.identity.request_digest,
                    configured_provider_config_digest: request.configured_provider_config_digest,
                },
            )
            .map_err(E2bCreationError::Store)?;
        planned.require_identity(&operation)?;
        if operation.dispatch_state != ProviderCreationDispatchState::Prepared {
            return Ok(operation);
        }
        // A local rejection proves only this still-unsent branch. Once the CAS
        // commits, every transport outcome retains the dispatch responsibility.
        if request.transport_timeout.is_zero()
            || request.transport_timeout > Duration::from_secs(60)
        {
            self.store
                .close_provider_creation_before_dispatch(authority, &operation.identity)
                .map_err(E2bCreationError::Store)?;
            return Err(E2bCreationError::Rejected(
                "e2b_creation_transport_timeout_invalid",
            ));
        }
        let permit = self
            .store
            .dispatch_provider_creation(authority, &operation.identity)
            .map_err(E2bCreationError::Store)?;
        self.send_create(permit, planned, request.transport_timeout)
    }

    pub fn record_late_candidate(
        &self,
        identity: &ProviderCreationIdentity,
        sandbox_id: &str,
    ) -> Result<ProviderCreationOperation, E2bCreationError> {
        self.store
            .record_provider_creation_observation(identity, sandbox_id)
            .map_err(|_| E2bCreationError::CandidateNotRecorded {
                identity: Box::new(identity.clone()),
                sandbox_id: sandbox_id.into(),
            })
    }

    fn send_create(
        &self,
        permit: FreshProviderCreationDispatchPermit,
        planned: PlannedCreation,
        timeout: Duration,
    ) -> Result<ProviderCreationOperation, E2bCreationError> {
        let operation = permit.consume();
        planned.require_identity(&operation)?;
        let response =
            match self
                .adapter
                .create(&operation.identity.attempt_id, planned.payload, timeout)
            {
                Ok(response) => response,
                // A transport-local rejection cannot reverse an already committed
                // ledger CAS. Neither branch retries or releases responsibility.
                Err(_) => return Ok(operation),
            };
        if response.request_id != operation.identity.attempt_id {
            return Err(E2bCreationError::Rejected(
                "e2b_creation_response_identity_rejected",
            ));
        }
        if response.outcome == AdapterOutcome::NotDispatched {
            return Ok(operation);
        }
        let Some(result) = response.result else {
            return if response.outcome == AdapterOutcome::Unknown {
                Ok(operation)
            } else {
                Err(E2bCreationError::Rejected("e2b_creation_response_invalid"))
            };
        };
        let candidate: CreateCandidate = serde_json::from_value(result)
            .map_err(|_| E2bCreationError::Rejected("e2b_creation_response_invalid"))?;
        if candidate.attempt_id != operation.identity.attempt_id {
            return Err(E2bCreationError::Rejected(
                "e2b_creation_response_identity_rejected",
            ));
        }
        let metadata_contradiction = candidate.metadata.as_ref().is_some_and(|metadata| {
            planned
                .metadata
                .iter()
                .any(|(key, value)| metadata.get(key) != Some(value))
        });
        let Some(sandbox_id) = candidate.sandbox_id else {
            return if response.outcome == AdapterOutcome::Unknown {
                Ok(operation)
            } else {
                Err(E2bCreationError::Rejected("e2b_creation_response_invalid"))
            };
        };
        let recorded = self.record_late_candidate(&operation.identity, &sandbox_id)?;
        if metadata_contradiction {
            // Keep the locator for investigation even when the inspection
            // contradicts our request. Candidate evidence authorizes no action.
            return Err(E2bCreationError::Rejected(
                "e2b_creation_response_identity_rejected",
            ));
        }
        Ok(recorded)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatePayload<'a> {
    attempt_id: &'a str,
    idempotency_key: &'a str,
    template_ref: TemplateReference<'a>,
    metadata: BTreeMap<String, String>,
    timeout_seconds: u32,
    allow_internet_access: bool,
    on_timeout: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemplateReference<'a> {
    template_id: &'a str,
    build_id: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateCandidate {
    attempt_id: String,
    sandbox_id: Option<String>,
    #[serde(rename = "templateId")]
    _template_id: Option<String>,
    metadata: Option<BTreeMap<String, String>>,
    #[serde(rename = "state")]
    _state: Option<String>,
}

struct PlannedCreation {
    identity: ProviderCreationIdentity,
    payload: Value,
    metadata: BTreeMap<String, String>,
}

impl PlannedCreation {
    fn new(
        authority: &ControllerAuthorityRequest<'_>,
        request: &E2bCreationRequest<'_>,
    ) -> Result<Self, E2bCreationError> {
        let authorization_digest = authority
            .environment_authorization
            .payload
            .agent_run_authorization_digest
            .clone();
        request
            .authorization
            .checked_binding(&authority.environment_authorization.payload.agent_run_id)
            .and_then(|validated| validated.verify_digest_binding(&authorization_digest))
            .map_err(|_| E2bCreationError::Rejected("e2b_creation_authorization_invalid"))?;
        authority
            .environment_authorization
            .verify(
                request.authorization,
                &authorization_digest,
                authority.signing_key,
            )
            .map_err(|_| E2bCreationError::Rejected("e2b_creation_authorization_invalid"))?;
        let EnvironmentProfile::E2b(profile) = &request.authorization.environment_profile else {
            return Err(E2bCreationError::Rejected(
                "e2b_creation_requires_e2b_profile",
            ));
        };
        if profile.lifetime != "perRun"
            || profile.provider_config_digest != request.configured_provider_config_digest
            || request.authorization.workspace_id != authority.workspace_id
        {
            return Err(E2bCreationError::Rejected("e2b_creation_policy_rejected"));
        }
        // Pinned provider OpenAPI defines the header independently of whether
        // its optional server-side idempotency feature is enabled.
        if request.idempotency_key.is_empty()
            || request.idempotency_key.len() > 255
            || !request
                .idempotency_key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(E2bCreationError::Rejected(
                "e2b_creation_idempotency_key_invalid",
            ));
        }
        for (value, maximum) in [(request.attempt_id, 128), (request.execution_id, 256)] {
            if value.is_empty()
                || value.len() > maximum
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
            {
                return Err(E2bCreationError::Rejected("e2b_creation_identity_invalid"));
            }
        }
        let mut identity = ProviderCreationIdentity {
            attempt_id: request.attempt_id.into(),
            execution_id: request.execution_id.into(),
            instance_id: crate::postgres_store::execution_instances::instance_id(
                request.execution_id,
            ),
            environment_id: authority
                .environment_authorization
                .payload
                .environment_id
                .clone(),
            session_id: request.authorization.session_id.clone(),
            workspace_id: request.authorization.workspace_id.clone(),
            agent_run_id: request.authorization.agent_run_id.clone(),
            authorization_digest,
            profile_digest: request.authorization.environment_profile_digest.clone(),
            provider_config_digest: request.configured_provider_config_digest.into(),
            request_digest: String::new(),
            idempotency_key: request.idempotency_key.into(),
            processing_job_id: authority.lease.job_id.clone(),
        };
        let metadata = BTreeMap::from([
            ("centaerisAttemptId".into(), identity.attempt_id.clone()),
            ("environmentId".into(), identity.environment_id.clone()),
            ("instanceId".into(), identity.instance_id.clone()),
            ("executionId".into(), identity.execution_id.clone()),
            ("agentRunId".into(), identity.agent_run_id.clone()),
            ("sessionId".into(), identity.session_id.clone()),
            ("workspaceId".into(), identity.workspace_id.clone()),
            ("profileDigest".into(), identity.profile_digest.clone()),
            (
                "authorizationDigest".into(),
                identity.authorization_digest.clone(),
            ),
            ("processingJobId".into(), identity.processing_job_id.clone()),
        ]);
        if metadata
            .values()
            .any(|value| value.is_empty() || value.len() > 4096 || value.contains('\0'))
        {
            return Err(E2bCreationError::Rejected("e2b_creation_metadata_invalid"));
        }
        let payload = serde_json::to_value(CreatePayload {
            attempt_id: request.attempt_id,
            idempotency_key: request.idempotency_key,
            template_ref: TemplateReference {
                template_id: &profile.template_ref.template_id,
                build_id: &profile.template_ref.build_id,
            },
            metadata: metadata.clone(),
            timeout_seconds: profile.timeout_seconds,
            allow_internet_access: profile.network == ExecutionNetwork::PublicInternet,
            on_timeout: "kill",
        })
        .map_err(|_| E2bCreationError::Rejected("e2b_creation_payload_invalid"))?;
        let canonical = canonical_json(payload.clone())
            .map_err(|_| E2bCreationError::Rejected("e2b_creation_payload_invalid"))?;
        // These fixed fields stay well below the transport's 1 MiB frame cap.
        if canonical.len() > 64 * 1024 {
            return Err(E2bCreationError::Rejected("e2b_creation_payload_too_large"));
        }
        identity.request_digest = format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()));
        Ok(Self {
            identity,
            payload,
            metadata,
        })
    }

    fn require_identity(
        &self,
        operation: &ProviderCreationOperation,
    ) -> Result<(), E2bCreationError> {
        if self.identity != operation.identity {
            return Err(E2bCreationError::Rejected(
                "e2b_creation_loaded_identity_rejected",
            ));
        }
        Ok(())
    }
}

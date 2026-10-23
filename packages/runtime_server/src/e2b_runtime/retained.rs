//! Retained provider identity is Environment-owned; execution authority stays per Run.
use super::*;
use crate::environment_authorization::EnvironmentAuthorization;
use crate::postgres_store::provider_operations::cleanup::ProviderRunCleanupReceipt;
use crate::postgres_store::provider_operations::retained::{
    RetainedProviderQuiescence, RetainedProviderRunUse,
};
use centaeris_core::session::RuntimeJobLeaseFence;
use uuid::Uuid;

struct OwnedRunAuthority {
    authorization: EnvironmentAuthorization,
    signing_key: Vec<u8>,
    workspace_id: String,
    lease: RuntimeJobLeaseFence,
}
impl OwnedRunAuthority {
    fn capture(authority: &ControllerAuthorityRequest<'_>) -> Self {
        Self {
            authorization: authority.environment_authorization.clone(),
            signing_key: authority.signing_key.to_vec(),
            workspace_id: authority.workspace_id.into(),
            lease: authority.lease.clone(),
        }
    }
    fn request(&self) -> ControllerAuthorityRequest<'_> {
        ControllerAuthorityRequest {
            environment_authorization: &self.authorization,
            signing_key: &self.signing_key,
            workspace_id: &self.workspace_id,
            lease: &self.lease,
            purpose: ControllerPurpose::Execute,
        }
    }
}

pub(super) struct RetainedRunAuthority {
    store: PostgresRuntimeStore,
    pub(super) use_binding: RetainedProviderRunUse,
    configured_digest: String,
    current: RwLock<Arc<OwnedRunAuthority>>,
}
impl RetainedRunAuthority {
    fn new(
        store: PostgresRuntimeStore,
        authority: &ControllerAuthorityRequest<'_>,
        value: RetainedProviderRunUse,
        configured_digest: &str,
    ) -> Result<Self, E2bRuntimeError> {
        let result = Self {
            store,
            use_binding: value,
            configured_digest: configured_digest.into(),
            current: RwLock::new(Arc::new(OwnedRunAuthority::capture(authority))),
        };
        result.validate(authority)?;
        Ok(result)
    }
    fn validate(&self, authority: &ControllerAuthorityRequest<'_>) -> Result<(), E2bRuntimeError> {
        if authority.purpose != ControllerPurpose::Execute
            || authority.environment_authorization.payload.agent_run_id
                != self.use_binding.agent_run_id
            || authority
                .environment_authorization
                .payload
                .agent_run_authorization_digest
                != self.use_binding.authorization_digest
        {
            return Err(E2bRuntimeError::Rejected(
                "e2b_retained_consumer_binding_rejected",
            ));
        }
        self.store
            .with_retained_provider_execution_dispatch(
                authority,
                &self.use_binding,
                &self.configured_digest,
                |_| Ok::<_, E2bHostError>(()),
            )
            .map_err(|_| E2bRuntimeError::Rejected("e2b_retained_authority_rejected"))
    }
    pub(super) fn replace(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
    ) -> Result<(), E2bRuntimeError> {
        // This value belongs to one immutable consumer. It never upgrades A to B.
        self.validate(authority)?;
        *self
            .current
            .write()
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_authority_unavailable"))? =
            Arc::new(OwnedRunAuthority::capture(authority));
        Ok(())
    }
}
impl E2bDispatchAuthority for RetainedRunAuthority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        let current = self
            .current
            .read()
            .map_err(|_| E2bHostError::NotDispatched("e2b_runtime_authority_unavailable"))?
            .clone();
        match self.store.with_retained_provider_execution_dispatch(
            &current.request(),
            &self.use_binding,
            &self.configured_digest,
            |target| {
                if target != &self.use_binding.target {
                    return Err(E2bHostError::NotDispatched("e2b_retained_target_rejected"));
                }
                operation()
            },
        ) {
            Ok(value) => Ok(value),
            Err(ProviderExecutionDispatchError::Callback(error)) => Err(error),
            Err(ProviderExecutionDispatchError::AfterDispatch(_)) => {
                Err(E2bHostError::Unknown("e2b_dispatch_outcome_unconfirmed"))
            }
            Err(ProviderExecutionDispatchError::Rejected(_)) => Err(E2bHostError::NotDispatched(
                "e2b_dispatch_authority_rejected",
            )),
        }
    }
}

/// A recovery-only host can run the fixed quiescence check inside cleanup pins.
/// It never receives an Execute authority or a usable ordinary SDK lane.
struct CleanupOnlyAuthority;
impl E2bDispatchAuthority for CleanupOnlyAuthority {
    fn dispatch(
        &self,
        _: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        Err(E2bHostError::NotDispatched("e2b_retained_cleanup_only"))
    }
}

impl E2bRuntime {
    pub(super) fn finish_retained_creation(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        authorization: &WorkspaceAgentRunAuthorization,
        execution_id: &str,
        operation: &ProviderCreationOperation,
        ordinary: Arc<SpoolingAdapterProcess>,
        control: Arc<SpoolingAdapterProcess>,
    ) -> Result<E2bPrepareOutcome, E2bRuntimeError> {
        let value = self
            .store
            .bind_retained_provider_run_use(
                authority,
                execution_id,
                &operation.identity.attempt_id,
                &self.config.provider_config_digest,
            )
            .map_err(|_| E2bRuntimeError::Unknown("e2b_retained_consumer_unconfirmed"))?;
        let dispatch = Arc::new(RetainedRunAuthority::new(
            self.store.clone(),
            authority,
            value.clone(),
            &self.config.provider_config_digest,
        )?);
        let binding = binding_for(authorization, &value.target)?;
        let transport = Arc::new(E2bSdkTransport::new(ordinary, control));
        let runner = Arc::new(
            E2bExecutionHostRunner::new(
                self.config.profile.clone(),
                binding,
                transport,
                dispatch.clone(),
            )
            .map_err(|_| E2bRuntimeError::Unknown("e2b_retained_host_unconfirmed"))?,
        );
        Ok(E2bPrepareOutcome::Prepared(Arc::new(E2bPreparedHost {
            runner,
            authority: PreparedAuthority::Retained(dispatch),
            logical_execution_id: execution_id.into(),
            workspace_reused_live: false,
        })))
    }

    pub(super) fn prepare_retained(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        authorization: &WorkspaceAgentRunAuthorization,
        execution_id: &str,
        previous: Option<&E2bPreparedHost>,
    ) -> Result<E2bPrepareOutcome, E2bRuntimeError> {
        self.attempt_key(authority, authorization, execution_id)?;
        let environment_id = &authority.environment_authorization.payload.environment_id;
        let Some(operation) = self
            .store
            .retained_provider_environment_creation(environment_id)
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_ledger_unavailable"))?
        else {
            return self.prepare_new(authority, authorization, execution_id);
        };
        if operation.dispatch_state == ProviderCreationDispatchState::Prepared
            && operation.identity.agent_run_id == authorization.agent_run_id
            && operation.identity.execution_id == execution_id
        {
            return self.prepare_new(authority, authorization, execution_id);
        }
        if operation.dispatch_state != ProviderCreationDispatchState::Dispatched
            || operation.reported_sandbox_id.is_none()
        {
            return Ok(pending_existing(operation));
        }
        // A verified create does not prove the creator's initial workspace was
        // prepared. Only the original in-memory context can continue that path.
        // A successor can attach after the prior consumer positively closed;
        // an open current consumer cannot discard its lost command streams.
        if previous.is_none()
            && (operation.identity.agent_run_id == authorization.agent_run_id
                || self
                    .store
                    .retained_provider_run_use(&authorization.agent_run_id)
                    .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_ledger_unavailable"))?
                    .is_some())
        {
            return Ok(pending_existing(operation));
        }
        let value = self
            .store
            .bind_retained_provider_run_use(
                authority,
                execution_id,
                &operation.identity.attempt_id,
                &self.config.provider_config_digest,
            )
            .map_err(|_| E2bRuntimeError::Rejected("e2b_retained_consumer_rejected"))?;
        let dispatch = Arc::new(RetainedRunAuthority::new(
            self.store.clone(),
            authority,
            value.clone(),
            &self.config.provider_config_digest,
        )?);
        let binding = binding_for(authorization, &value.target)?;
        let runner = if let Some(previous) = previous {
            if previous.runner.binding() != &binding || previous.retained_use().is_none() {
                return Err(E2bRuntimeError::Rejected(
                    "e2b_retained_context_binding_rejected",
                ));
            }
            previous
                .runner
                .successor(dispatch.clone())
                .map_err(|_| E2bRuntimeError::Unknown("e2b_retained_prior_effects_unconfirmed"))?
        } else {
            let ordinary = self
                .launcher
                .launch(&self.config, E2bAdapterLane::Ordinary)?;
            let control = self
                .launcher
                .launch(&self.config, E2bAdapterLane::Control)?;
            require_capabilities(
                &ordinary,
                E2bAdapterLane::Ordinary,
                self.config.request_timeout,
            )?;
            require_capabilities_for_profile(
                &control,
                E2bAdapterLane::Control,
                self.config.request_timeout,
                &self.config.profile,
            )?;
            // Read-only attach never resumes, extends TTL, creates or replays work.
            self.store.with_retained_provider_execution_dispatch(authority, &value,
                &self.config.provider_config_digest, |_| {
                    let response = ordinary.call(&Uuid::new_v4().to_string(), "attach",
                        json!({"sandboxId":binding.sandbox_id,"expectedTemplateId":binding.template_id,
                            "expectedMetadata":binding.metadata,"workspaceRoot":"/mnt/data","workspaceUser":"agent"}),
                        self.config.request_timeout)
                        .map_err(|_| E2bHostError::Unknown("e2b_retained_attach_unconfirmed"))?;
                    if response.outcome != AdapterOutcome::Succeeded {
                        return Err(E2bHostError::Unknown("e2b_retained_attach_unconfirmed"));
                    }
                    let receipt: AttachReceipt = serde_json::from_value(response.result
                        .ok_or(E2bHostError::Unknown("e2b_retained_attach_unconfirmed"))?)
                        .map_err(|_| E2bHostError::Unknown("e2b_retained_attach_unconfirmed"))?;
                    if receipt.sandbox_id != binding.sandbox_id || receipt.template_id != binding.template_id
                        || receipt.metadata != binding.metadata || receipt.state != "running" {
                        return Err(E2bHostError::Unknown("e2b_retained_attach_unconfirmed"));
                    }
                    Ok(())
                }).map_err(|_| E2bRuntimeError::Unknown("e2b_retained_attach_unconfirmed"))?;
            E2bExecutionHostRunner::new(
                self.config.profile.clone(),
                binding,
                Arc::new(E2bSdkTransport::new(ordinary, control)),
                dispatch.clone(),
            )
            .map_err(|_| E2bRuntimeError::Unknown("e2b_retained_host_unconfirmed"))?
        };
        Ok(E2bPrepareOutcome::Prepared(Arc::new(E2bPreparedHost {
            runner: Arc::new(runner),
            authority: PreparedAuthority::Retained(dispatch),
            logical_execution_id: execution_id.into(),
            workspace_reused_live: true,
        })))
    }

    pub(crate) fn cleanup_retained(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        host: Option<&E2bPreparedHost>,
    ) -> Result<ProviderRunCleanupReceipt, E2bRuntimeError> {
        let run_id = &authority.environment_authorization.payload.agent_run_id;
        let mut value = self
            .store
            .retained_provider_run_use(run_id)
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_ledger_unavailable"))?;
        let mut recovered_runner = None;
        if value.is_none() {
            if let Some(operation) = self
                .store
                .retained_provider_environment_creation(
                    &authority.environment_authorization.payload.environment_id,
                )
                .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_ledger_unavailable"))?
            {
                if operation.dispatch_state == ProviderCreationDispatchState::Dispatched {
                    // Only the original creator can recover its own missing use.
                    // A locator is evidence to inspect, not proof of absent effects.
                    if operation.identity.agent_run_id != *run_id
                        || operation.reported_sandbox_id.is_none()
                    {
                        return Err(E2bRuntimeError::Unknown(
                            "e2b_retained_cleanup_target_unconfirmed",
                        ));
                    }
                    let mut lane = cleanup::CleanupLane::new(self);
                    let target = self
                        .store
                        .verify_retained_provider_cleanup_target(
                            authority,
                            &operation.identity,
                            &self.config.provider_config_digest,
                            |_, sandbox| lane.inspect(sandbox),
                        )
                        .map_err(|_| {
                            E2bRuntimeError::Unknown("e2b_retained_cleanup_target_unconfirmed")
                        })?;
                    let EnvironmentProfile::E2b(profile) = &self.config.profile else {
                        return Err(E2bRuntimeError::Rejected("e2b_runtime_profile_unsupported"));
                    };
                    let control = self
                        .launcher
                        .launch(&self.config, E2bAdapterLane::Control)?;
                    require_capabilities(
                        &control,
                        E2bAdapterLane::Control,
                        self.config.request_timeout,
                    )?;
                    // Both transport fields deliberately refer to the control-only
                    // child. Its SDK lane rejects ordinary execution independently
                    // of the deny-all authority held by this temporary host.
                    let runner = E2bExecutionHostRunner::new(
                        self.config.profile.clone(),
                        E2bSandboxBinding {
                            sandbox_id: target.sandbox_id.clone(),
                            template_id: profile.template_ref.template_id.clone(),
                            metadata: expected_metadata(&target.identity),
                        },
                        Arc::new(E2bSdkTransport::new(control.clone(), control)),
                        Arc::new(CleanupOnlyAuthority),
                    )
                    .map_err(|_| {
                        E2bRuntimeError::Rejected("e2b_retained_cleanup_binding_rejected")
                    })?;
                    value = Some(
                        self.store
                            .recover_retained_provider_run_use_for_cleanup(
                                authority,
                                &operation.identity.execution_id,
                                &operation.identity.attempt_id,
                                &self.config.provider_config_digest,
                            )
                            .map_err(|_| {
                                E2bRuntimeError::Unknown("e2b_retained_cleanup_use_unconfirmed")
                            })?,
                    );
                    recovered_runner = Some(runner);
                }
            }
        }
        if let Some(value) = value {
            let epoch = authority
                .environment_authorization
                .payload
                .controller
                .as_ref()
                .ok_or(E2bRuntimeError::Rejected(
                    "e2b_retained_cleanup_authority_rejected",
                ))?
                .controller_epoch;
            return self
                .store
                .confirm_retained_provider_run_cleanup(
                    authority,
                    &value,
                    &self.config.provider_config_digest,
                    |checked| {
                        let runner = if let Some(runner) = recovered_runner.as_ref() {
                            runner
                        } else {
                            // Existing open uses retain their original command
                            // evidence. Recovery never reconstructs them empty.
                            let host = host.ok_or("e2b_retained_original_context_unavailable")?;
                            if host.retained_use() != Some(checked) {
                                return Err("e2b_retained_cleanup_context_rejected".into());
                            }
                            host.runner.as_ref()
                        };
                        runner
                            .confirm_run_quiescence_under_cleanup_pins(
                                &checked.target.identity.instance_id,
                                &checked.target.sandbox_id,
                            )
                            .map_err(|_| "e2b_retained_quiescence_unconfirmed")?;
                        Ok(RetainedProviderQuiescence {
                            agent_run_id: checked.agent_run_id.clone(),
                            execution_id: checked.execution_id.clone(),
                            instance_id: checked.target.identity.instance_id.clone(),
                            sandbox_id: checked.target.sandbox_id.clone(),
                            controller_epoch: epoch,
                            quiescent: true,
                        })
                    },
                )
                .map(ProviderRunCleanupReceipt::RetainedProvider)
                .map_err(|_| E2bRuntimeError::Unknown("e2b_retained_run_cleanup_unconfirmed"));
        }
        // Only a proven unsent creation or a genuinely unused Run reaches these
        // existing receipts. A reused consumer must never impersonate NoEffects.
        match self.cleanup(authority)? {
            E2bCleanupOutcome::Complete { operation_count: 0 } => self
                .store
                .confirm_provider_run_no_effects(authority)
                .map(Into::into)
                .map_err(|_| E2bRuntimeError::Unknown("e2b_retained_no_effects_unconfirmed")),
            E2bCleanupOutcome::Complete { .. } => self
                .store
                .confirm_provider_run_cleanup(authority)
                .map(Into::into)
                .map_err(|_| E2bRuntimeError::Unknown("e2b_retained_unsent_cleanup_unconfirmed")),
            E2bCleanupOutcome::Pending { .. } => {
                Err(E2bRuntimeError::Unknown("e2b_retained_cleanup_unconfirmed"))
            }
        }
    }
}

fn binding_for(
    authorization: &WorkspaceAgentRunAuthorization,
    target: &ProviderVerifiedTarget,
) -> Result<E2bSandboxBinding, E2bRuntimeError> {
    let EnvironmentProfile::E2b(profile) = &authorization.environment_profile else {
        return Err(E2bRuntimeError::Rejected("e2b_runtime_profile_unsupported"));
    };
    Ok(E2bSandboxBinding {
        sandbox_id: target.sandbox_id.clone(),
        template_id: profile.template_ref.template_id.clone(),
        metadata: expected_metadata(&target.identity),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachReceipt {
    sandbox_id: String,
    template_id: String,
    metadata: std::collections::BTreeMap<String, String>,
    state: String,
}

//! Independently signed provisioning owns no synthetic AgentRun or Job.
use super::*;
use crate::environment_authorization::provision::ProvisionAuthorization;
use crate::postgres_store::provider_operations::provision::{
    ProviderProvisionCompletion, ProviderProvisionPreparation,
};

pub(crate) enum E2bProvisionOutcome {
    Completed(Box<ProviderProvisionCompletion>),
    Pending(Box<ProviderCreationOperation>),
}

fn provision_attempt_key(
    config: &E2bRuntimeConfig,
    auth: &ProvisionAuthorization,
    signing_key: &[u8],
) -> Result<(String, E2bAttemptKey), E2bRuntimeError> {
    auth.verify_signature(signing_key)
        .map_err(|_| E2bRuntimeError::Rejected("e2b_provision_authorization_rejected"))?;
    let binding = &auth.payload;
    if binding.environment_profile != config.profile
        || !matches!(&config.profile,EnvironmentProfile::E2b(profile) if profile.lifetime=="retained")
    {
        return Err(E2bRuntimeError::Rejected("e2b_provision_profile_rejected"));
    }
    let authorization_digest = auth
        .digest()
        .map_err(|_| E2bRuntimeError::Rejected("e2b_provision_authorization_rejected"))?;
    let execution_id = format!("provision:{}", binding.operation_id);
    let key = make_attempt_key(
        &binding.environment_id,
        &authorization_digest,
        &binding.environment_profile_digest,
        &config.provider_config_digest,
        &execution_id,
    );
    Ok((execution_id, key))
}

impl E2bRuntime {
    pub(crate) fn provision(
        &self,
        auth: &ProvisionAuthorization,
        signing_key: &[u8],
    ) -> Result<E2bProvisionOutcome, E2bRuntimeError> {
        let (execution_id, key) = provision_attempt_key(&self.config, auth, signing_key)?;
        if let Some(receipt) = self
            .store
            .provider_provision_completion(&auth.payload.operation_id)
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_provision_completion_unavailable"))?
        {
            // Historical completion is a read-only fact, not current readiness or
            // permission to perform another physical operation.
            if !completion_matches(auth, &self.config, &receipt, &key, &execution_id) {
                return Err(E2bRuntimeError::Rejected(
                    "e2b_provision_completion_binding_rejected",
                ));
            }
            return Ok(E2bProvisionOutcome::Completed(Box::new(receipt)));
        }
        let request = crate::e2b_creation::E2bProvisionCreationRequest {
            authorization: auth,
            signing_key,
            execution_id: &execution_id,
            attempt_id: &key.attempt_id,
            idempotency_key: &key.idempotency_key,
            configured_provider_config_digest: &self.config.provider_config_digest,
            transport_timeout: self.config.request_timeout,
        };
        if self
            .store
            .provider_creation_operation(&key.attempt_id)
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_provision_ledger_unavailable"))?
            .is_some_and(|op| op.dispatch_state != ProviderCreationDispatchState::Prepared)
        {
            let operation = E2bCreationDriver::new(&self.store, &NeverCreateAdapter)
                .create_provision_or_reopen(&request)
                .map_err(|_| E2bRuntimeError::Unknown("e2b_provision_creation_unconfirmed"))?;
            // Loaded dispatch responsibility is never a fresh create or helper permit.
            return Ok(E2bProvisionOutcome::Pending(Box::new(operation)));
        }
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
        let adapter = RecordingCreationAdapter {
            ordinary: &ordinary,
            sent: AtomicBool::new(false),
            succeeded: AtomicBool::new(false),
        };
        let operation = E2bCreationDriver::new(&self.store, &adapter)
            .with_running_limit(self.config.running_limit)
            .create_provision_or_reopen(&request)
            .map_err(|_| E2bRuntimeError::Unknown("e2b_provision_creation_unconfirmed"))?;
        if !adapter.sent.load(Ordering::Acquire)
            || !adapter.succeeded.load(Ordering::Acquire)
            || operation.dispatch_state != ProviderCreationDispatchState::Dispatched
            || operation.reported_sandbox_id.is_none()
        {
            return Ok(E2bProvisionOutcome::Pending(Box::new(operation)));
        }
        let target = self
            .store
            .with_provider_provision_execution_dispatch(
                auth,
                signing_key,
                &operation.identity,
                &self.config.provider_config_digest,
                |target| Ok::<_, E2bHostError>(target.clone()),
            )
            .map_err(|_| E2bRuntimeError::Unknown("e2b_provision_target_unconfirmed"))?;
        let EnvironmentProfile::E2b(profile) = &self.config.profile else {
            unreachable!("validated profile")
        };
        let binding = E2bSandboxBinding {
            sandbox_id: target.sandbox_id.clone(),
            template_id: profile.template_ref.template_id.clone(),
            metadata: expected_metadata(&target.identity),
        };
        let authority = Arc::new(ProvisionDispatchAuthority {
            store: self.store.clone(),
            authorization: auth.clone(),
            signing_key: signing_key.to_vec(),
            target: target.clone(),
            configured_digest: self.config.provider_config_digest.clone(),
        });
        let runner = E2bExecutionHostRunner::new(
            self.config.profile.clone(),
            binding,
            Arc::new(E2bSdkTransport::new(
                ordinary,
                control,
                self.config.profile.clone(),
            )),
            authority,
        )
        .map_err(|_| E2bRuntimeError::Unknown("e2b_provision_host_unconfirmed"))?;
        runner
            .prepare_workspace()
            .map_err(|_| E2bRuntimeError::Unknown("e2b_provision_preparation_unconfirmed"))?;
        let helper = if self.config.profile.task_image() {
            "taskImage"
        } else if self.config.profile.managed_files() {
            "prepare-managed-workspace"
        } else {
            "prepare-workspace"
        };
        let preparation=crate::agent_run_authorization::canonical_json(json!({
            "schema":"runtime.providerWorkspacePreparation.v1",
            "creationAttemptId":target.identity.attempt_id,"instanceId":target.identity.instance_id,
            "sandboxId":target.sandbox_id,"verificationDigest":target.verification_digest,
            "authorizationDigest":target.identity.authorization_digest,"profileDigest":target.identity.profile_digest,
            "helper":helper,
        })).map_err(|_|E2bRuntimeError::Unknown("e2b_provision_preparation_encoding_unconfirmed"))?;
        let preparation = ProviderProvisionPreparation {
            instance_id: target.identity.instance_id.clone(),
            sandbox_id: target.sandbox_id.clone(),
            preparation_digest: format!("sha256:{:x}", Sha256::digest(preparation.as_bytes())),
        };
        let receipt = self
            .store
            .complete_provider_provision(auth, signing_key, &target, &preparation)
            .map_err(|_| E2bRuntimeError::Unknown("e2b_provision_completion_unconfirmed"))?;
        Ok(E2bProvisionOutcome::Completed(Box::new(receipt)))
    }
}

// Owned current authority is private; a local runner cannot manufacture a fence.
struct ProvisionDispatchAuthority {
    store: PostgresRuntimeStore,
    authorization: ProvisionAuthorization,
    signing_key: Vec<u8>,
    target: ProviderVerifiedTarget,
    configured_digest: String,
}
impl E2bDispatchAuthority for ProvisionDispatchAuthority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        match self.store.with_provider_provision_execution_dispatch(
            &self.authorization,
            &self.signing_key,
            &self.target.identity,
            &self.configured_digest,
            |target| {
                if target != &self.target {
                    return Err(E2bHostError::NotDispatched("e2b_provision_target_rejected"));
                }
                operation()
            },
        ) {
            Ok(value) => Ok(value),
            Err(ProviderExecutionDispatchError::Rejected(_)) => Err(E2bHostError::NotDispatched(
                "e2b_provision_authority_rejected",
            )),
            Err(ProviderExecutionDispatchError::Callback(error)) => Err(error),
            Err(ProviderExecutionDispatchError::AfterDispatch(_)) => {
                Err(E2bHostError::Unknown("e2b_provision_dispatch_unconfirmed"))
            }
        }
    }
}
fn completion_matches(
    auth: &ProvisionAuthorization,
    config: &E2bRuntimeConfig,
    value: &ProviderProvisionCompletion,
    key: &E2bAttemptKey,
    execution_id: &str,
) -> bool {
    let b = &auth.payload;
    value.operation_id == b.operation_id
        && value.creation_attempt_id == key.attempt_id
        && value.environment_id == b.environment_id
        && value.session_id == b.session_id
        && value.workspace_id == b.workspace_id
        && value.instance_id
            == crate::postgres_store::execution_instances::instance_id(execution_id)
        && value.profile_digest == b.environment_profile_digest
        && value.provider_config_digest == config.provider_config_digest
        && value.authorization_digest == auth.digest().unwrap_or_default()
        && value.controller_epoch == b.controller_epoch
        && value.controller_lease_digest == b.controller_lease_digest
        && value.request_digest == b.request_digest
        && value.recorded_at_ms >= 0
        && !value.sandbox_id.is_empty()
        && value.sandbox_id.len() <= 256
        && value
            .sandbox_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        && value
            .preparation_digest
            .strip_prefix("sha256:")
            .is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};

    fn authorization(profile: EnvironmentProfile) -> ProvisionAuthorization {
        let vector:serde_json::Value=serde_json::from_str(include_str!("../../../../tests/workspace/fixtures/environment_provision_authorization/v1/valid.json")).unwrap();
        let mut auth = ProvisionAuthorization {
            payload: serde_json::from_value(vector["payload"].clone()).unwrap(),
            signature: vector["signature"].as_str().unwrap().into(),
        };
        auth.payload.environment_profile = profile;
        auth.payload.environment_profile_digest =
            auth.payload.environment_profile.digest().unwrap();
        let encoded = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(auth.payload.request()).unwrap(),
        )
        .unwrap();
        auth.payload.request_digest = format!("sha256:{:x}", Sha256::digest(encoded.as_bytes()));
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-key").unwrap();
        mac.update(b"workspace:environment-provision-authorization:v1\0");
        mac.update(auth.digest().unwrap().as_bytes());
        auth.signature = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
        auth
    }

    #[test]
    fn provision_identity_binds_the_real_operation_and_frozen_profile() {
        let root = tempfile::tempdir().unwrap();
        let python = root.path().join("python-fixture");
        std::fs::write(&python, b"fixture").unwrap();
        let package = root.path().join("package");
        std::fs::create_dir_all(package.join("centaeris_e2b_adapter")).unwrap();
        for name in ["__init__.py", "__main__.py"] {
            std::fs::write(package.join("centaeris_e2b_adapter").join(name), b"").unwrap();
        }
        let mut profile: EnvironmentProfile = serde_json::from_str(include_str!(
            "../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
        let EnvironmentProfile::E2b(ref mut p) = profile else {
            unreachable!()
        };
        p.lifetime = "retained".into();
        p.user = "agent".into();
        p.workspace_root = "/mnt/data".into();
        let configured_digest = p.provider_config_digest.clone();
        let config = E2bRuntimeConfig::new(
            profile.clone(),
            configured_digest,
            python,
            package,
            Duration::from_secs(20),
        )
        .unwrap();
        let auth = authorization(profile);
        let (execution, key) = provision_attempt_key(&config, &auth, b"test-key").unwrap();
        assert_eq!(
            execution,
            format!("provision:{}", auth.payload.operation_id)
        );
        assert_eq!(
            key,
            provision_attempt_key(&config, &auth, b"test-key")
                .unwrap()
                .1
        );
        assert!(provision_attempt_key(&config, &auth, b"wrong-key").is_err());
        let mut changed = auth.clone();
        changed.payload.operation_id = "another".into();
        assert!(provision_attempt_key(&config, &changed, b"test-key").is_err());
    }
}

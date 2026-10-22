//! Current Execute controller pins for every E2B process or helper start.
use super::{E2bDispatchAuthority, E2bDispatchResult, E2bHostError};
use crate::environment_authorization::EnvironmentAuthorization;
use crate::postgres_store::controller_authority::{ControllerAuthorityRequest, ControllerPurpose};
use crate::postgres_store::provider_operations::cleanup::ProviderExecutionDispatchError;
use crate::postgres_store::provider_operations::ProviderCreationIdentity;
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::session::RuntimeJobLeaseFence;

/// Owned authorization material is deliberately not printable through Debug.
pub(crate) struct PostgresE2bDispatchAuthority {
    store: PostgresRuntimeStore,
    environment_authorization: EnvironmentAuthorization,
    signing_key: Vec<u8>,
    workspace_id: String,
    lease: RuntimeJobLeaseFence,
    identity: ProviderCreationIdentity,
    configured_digest: String,
    expected_sandbox_id: String,
}

impl PostgresE2bDispatchAuthority {
    pub(crate) fn new(
        store: PostgresRuntimeStore,
        authority: &ControllerAuthorityRequest<'_>,
        identity: ProviderCreationIdentity,
        configured_digest: &str,
        expected_sandbox_id: &str,
    ) -> Result<Self, E2bHostError> {
        if authority.purpose != ControllerPurpose::Execute
            || configured_digest != identity.provider_config_digest
            || expected_sandbox_id.is_empty()
        {
            return Err(E2bHostError::NotDispatched(
                "e2b_dispatch_authority_binding_rejected",
            ));
        }
        Ok(Self {
            store,
            environment_authorization: authority.environment_authorization.clone(),
            signing_key: authority.signing_key.to_vec(),
            workspace_id: authority.workspace_id.into(),
            lease: authority.lease.clone(),
            identity,
            configured_digest: configured_digest.into(),
            expected_sandbox_id: expected_sandbox_id.into(),
        })
    }
}

impl E2bDispatchAuthority for PostgresE2bDispatchAuthority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        let authority = ControllerAuthorityRequest {
            environment_authorization: &self.environment_authorization,
            signing_key: &self.signing_key,
            workspace_id: &self.workspace_id,
            lease: &self.lease,
            purpose: ControllerPurpose::Execute,
        };
        let result = self.store.with_provider_execution_dispatch(
            &authority,
            &self.identity,
            &self.configured_digest,
            |target| {
                if target.identity != self.identity || target.sandbox_id != self.expected_sandbox_id
                {
                    return Err(E2bHostError::NotDispatched(
                        "e2b_dispatch_authority_target_rejected",
                    ));
                }
                operation()
            },
        );
        match result {
            Ok(value) => Ok(value),
            Err(ProviderExecutionDispatchError::Rejected(_)) => Err(E2bHostError::NotDispatched(
                "e2b_dispatch_authority_rejected",
            )),
            Err(ProviderExecutionDispatchError::Callback(error)) => Err(error),
            Err(ProviderExecutionDispatchError::AfterDispatch(_)) => {
                Err(E2bHostError::Unknown("e2b_dispatch_outcome_unconfirmed"))
            }
        }
    }
}

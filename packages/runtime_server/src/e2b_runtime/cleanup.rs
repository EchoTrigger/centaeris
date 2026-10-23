//! Provider cleanup uses the existing per-operation authority and settlement facts.
use super::*;
use crate::observations::power::{PowerMetric, PowerObservation};
use crate::postgres_store::provider_operations::cleanup::{
    ProviderRemovalEvidence, ProviderSandboxObservation,
};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum E2bCleanupPendingStage {
    TargetUnverified,
    RemovalUnconfirmed,
    SettlementUnconfirmed,
}

#[derive(Debug)]
pub(crate) enum E2bCleanupOutcome {
    /// This is orchestration progress, not a cleanup/release receipt. The caller
    /// must still ask the Store to confirm the corresponding proof atomically.
    Complete { operation_count: u32 },
    Pending {
        identity: Box<ProviderCreationIdentity>,
        stage: E2bCleanupPendingStage,
    },
}

impl E2bRuntime {
    pub(crate) fn cleanup(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
    ) -> Result<E2bCleanupOutcome, E2bRuntimeError> {
        // This short read proves the caller's current cleanup scope. It grants
        // no physical action: each operation below rechecks its own authority.
        let operations = self
            .store
            .provider_creation_operations_for_cleanup(
                authority,
                &self.config.provider_config_digest,
            )
            .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_cleanup_authority_rejected"))?;
        let operation_count = u32::try_from(operations.len()).map_err(|_| {
            E2bRuntimeError::Unavailable("e2b_runtime_cleanup_operation_count_invalid")
        })?;
        let mut lane = CleanupLane::new(self);
        for operation in operations {
            match operation.dispatch_state {
                ProviderCreationDispatchState::Prepared
                | ProviderCreationDispatchState::NotDispatched => {
                    if self
                        .store
                        .settle_provider_creation_not_dispatched(authority, &operation.identity)
                        .is_err()
                    {
                        return Ok(pending(
                            operation,
                            E2bCleanupPendingStage::SettlementUnconfirmed,
                        ));
                    }
                }
                ProviderCreationDispatchState::Dispatched => {
                    // The Store keeps the existing slow cleanup budget and
                    // authority pins through these bounded callbacks. Replaying
                    // a durable target or settlement never invokes the callback.
                    if self
                        .store
                        .verify_provider_cleanup_target(
                            authority,
                            &operation.identity,
                            &self.config.provider_config_digest,
                            |_, sandbox_id| lane.inspect(sandbox_id),
                        )
                        .is_err()
                    {
                        return Ok(pending(operation, E2bCleanupPendingStage::TargetUnverified));
                    }
                    if self
                        .store
                        .remove_provider_creation(
                            authority,
                            &operation.identity,
                            &self.config.provider_config_digest,
                            |target| lane.remove(&target.sandbox_id),
                        )
                        .is_err()
                    {
                        return Ok(pending(
                            operation,
                            E2bCleanupPendingStage::RemovalUnconfirmed,
                        ));
                    }
                }
            }
        }
        Ok(E2bCleanupOutcome::Complete { operation_count })
    }
}

fn pending(
    operation: ProviderCreationOperation,
    stage: E2bCleanupPendingStage,
) -> E2bCleanupOutcome {
    E2bCleanupOutcome::Pending {
        identity: Box::new(operation.identity),
        stage,
    }
}

/// The child is allocated only when an authorized callback needs provider IO.
/// It is independent of ordinary/control SDK streams and owns no VM lifetime.
pub(super) struct CleanupLane<'a> {
    runtime: &'a E2bRuntime,
    process: Option<Arc<SpoolingAdapterProcess>>,
    observation: Option<PowerObservation>,
}

impl CleanupLane<'_> {
    pub(super) fn new(runtime: &E2bRuntime) -> CleanupLane<'_> {
        CleanupLane {
            runtime,
            process: None,
            observation: None,
        }
    }

    pub(super) fn new_observed(
        runtime: &E2bRuntime,
        observation: PowerObservation,
    ) -> CleanupLane<'_> {
        CleanupLane {
            runtime,
            process: None,
            observation: Some(observation),
        }
    }

    fn call(&mut self, operation: &str, sandbox_id: &str) -> Result<Value, String> {
        self.call_payload(operation, json!({"sandboxId":sandbox_id}))
    }

    pub(super) fn call_payload(
        &mut self,
        operation: &str,
        payload: Value,
    ) -> Result<Value, String> {
        let observation = self.observation.clone();
        if self.process.is_none() {
            let launch = || match &observation {
                Some(value) => self.runtime.launcher.launch_observed(
                    &self.runtime.config,
                    E2bAdapterLane::Cleanup,
                    value.clone(),
                ),
                None => self
                    .runtime
                    .launcher
                    .launch(&self.runtime.config, E2bAdapterLane::Cleanup),
            };
            let process = match &observation {
                Some(value) => value.timed(PowerMetric::AdapterSpawn, launch),
                None => launch(),
            }
            .map_err(|_| "e2b_cleanup_adapter_unavailable".to_string())?;
            let handshake = || {
                require_capabilities(
                    &process,
                    E2bAdapterLane::Cleanup,
                    self.runtime.config.request_timeout,
                )
            };
            match &observation {
                Some(value) => value.timed(PowerMetric::AdapterHandshake, handshake),
                None => handshake(),
            }
            .map_err(|_| "e2b_cleanup_adapter_unavailable".to_string())?;
            self.process = Some(process);
        }
        let call = || {
            let response = self
                .process
                .as_ref()
                .ok_or("e2b_cleanup_adapter_unavailable")?
                .call(
                    &Uuid::new_v4().to_string(),
                    operation,
                    payload,
                    self.runtime.config.request_timeout,
                )
                .map_err(|_| "e2b_cleanup_provider_result_unknown".to_string())?;
            if response.outcome != AdapterOutcome::Succeeded {
                return Err("e2b_cleanup_provider_result_unknown".into());
            }
            response
                .result
                .ok_or("e2b_cleanup_provider_result_invalid".into())
        };
        match &observation {
            Some(value) => value.timed(PowerMetric::AdapterRoundTrip, call),
            None => call(),
        }
    }

    pub(super) fn inspect(
        &mut self,
        sandbox_id: &str,
    ) -> Result<ProviderSandboxObservation, String> {
        let receipt: InspectReceipt = serde_json::from_value(self.call("inspect", sandbox_id)?)
            .map_err(|_| "e2b_cleanup_inspection_invalid")?;
        if receipt.sandbox_id != sandbox_id
            || receipt.template_id.is_empty()
            || receipt.state.is_empty()
        {
            return Err("e2b_cleanup_inspection_identity_rejected".into());
        }
        Ok(ProviderSandboxObservation {
            sandbox_id: receipt.sandbox_id,
            template_id: receipt.template_id,
            metadata: receipt.metadata,
        })
    }

    pub(super) fn remove(&mut self, sandbox_id: &str) -> Result<ProviderRemovalEvidence, String> {
        let receipt: DeleteReceipt = serde_json::from_value(self.call("kill", sandbox_id)?)
            .map_err(|_| "e2b_cleanup_deletion_invalid")?;
        if receipt.sandbox_id != sandbox_id || !receipt.delete_acknowledged {
            return Err("e2b_cleanup_deletion_identity_rejected".into());
        }
        // A completed bridge call acknowledges the exact deletion request. The
        // separate SDK result is false specifically for provider not-found.
        // Unknown transport/provider errors never reach either evidence branch.
        if receipt.provider_reported_killed {
            Ok(ProviderRemovalEvidence::Deleted {
                sandbox_id: receipt.sandbox_id,
            })
        } else {
            Ok(ProviderRemovalEvidence::NotFound {
                sandbox_id: receipt.sandbox_id,
            })
        }
    }
}

impl Drop for CleanupLane<'_> {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            match &self.observation {
                Some(observation) => {
                    let _: Result<(), std::convert::Infallible> =
                        observation.timed(PowerMetric::AdapterClose, || {
                            drop(process);
                            Ok(())
                        });
                }
                None => drop(process),
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InspectReceipt {
    sandbox_id: String,
    template_id: String,
    metadata: BTreeMap<String, String>,
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteReceipt {
    sandbox_id: String,
    delete_acknowledged: bool,
    provider_reported_killed: bool,
}

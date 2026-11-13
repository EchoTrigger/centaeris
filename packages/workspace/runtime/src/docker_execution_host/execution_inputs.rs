//! Host delivery of independently authorized immutable inputs.
use super::*;
use crate::material_execution::VerifiedMaterialInputFrame;
use crate::product_inputs::ResolvedInput;
use hosted_execution::host_supervisor::TrustedHelperKind;
use std::collections::BTreeSet;

fn material_input_consumer(input: &ResolvedInput) -> String {
    let mut digest = Sha256::new();
    for field in [
        &input.input_ref,
        &input.source_version,
        &input.sha256,
        &input.virtual_path,
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest.update(input.size_bytes.to_be_bytes());
    format!("material-input:{:x}", digest.finalize())
}

fn pending_revocation(
    pending: &crate::material_execution::PendingInput,
) -> Result<Option<ExecutionInputState>, String> {
    pending.resolved_input.validate()?;
    let revocation = match pending.state.as_str() {
        "active" => None,
        "asset_removed" | "access_revoked" | "stale_generation" | "asset_unavailable" => {
            Some(ExecutionInputState::AccessRevoked)
        }
        _ => return Err("material reconciliation binding invalid".into()),
    };
    // Confirmed inputs have no outstanding admission reservations. Once their
    // authority expires, their physical original still needs revocation.
    if (revocation.is_none() && pending.reservations.is_empty())
        || pending.reservations.iter().any(|reservation| {
            reservation.admission_id <= 0
                || reservation.origin_run_id.is_empty()
                || reservation.call_id.as_ref().is_some_and(|id| id.is_empty())
        })
    {
        return Err("material reconciliation binding invalid".into());
    }
    Ok(revocation)
}

impl DockerExecutionHostRunner {
    pub(crate) fn reconcile_material_inputs(
        &self,
        pending: &[crate::material_execution::PendingInput],
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let lease = self
            .resident_lease
            .as_ref()
            .ok_or("material reconciliation lease unavailable")?;
        let _activity = self.recovery_activity.begin_dispatch_activity()?;
        let _inputs = self
            .recovery_activity
            .input_operations()
            .lock()
            .map_err(|_| "sandbox input operation lock poisoned")?;
        let mut settlements = Vec::new();
        for pending in pending {
            let input = &pending.resolved_input;
            let revocation = pending_revocation(pending)?;
            let consumer = material_input_consumer(input);
            let actual = self.inspect_material_input(input, &consumer)?;
            let present = actual.as_ref().is_some_and(|value| value.state.is_none());
            if let Some(state) = revocation.filter(|_| present) {
                self.recovery_activity.invalidate_before_dispatch()?;
                self.revoke_input_for_consumer(
                    actual.as_ref().ok_or("material inventory missing")?,
                    state,
                    Some(&consumer),
                )
                .map_err(|error| error.to_string())?;
            }
            settlements.push(serde_json::json!({"inputRef":input.input_ref,"sourceVersion":input.source_version,
                "sha256":input.sha256,"state":if revocation.is_none() && present {"present"} else {"absent"},
                "admissionIds":pending.reservations.iter().map(|value| value.admission_id).collect::<Vec<_>>()}));
        }
        let response = self.api_client.post(format!("{}/internal/agent-runs/materials/reconcile", self.api_url.trim_end_matches('/')))
            .header("X-Internal-Token", &self.api_token).timeout(Duration::from_secs(30))
            .json(&serde_json::json!({"schema":"workspace.material.reconcile.v1","agentRunId":self.agent_run_id,
                "authorizationDigest":self.authorization_digest,"jobId":lease.job_id,"leaseOwner":lease.lease_owner,
                "inputs":settlements})).send().map_err(|_| "material reconciliation unavailable")?;
        if !response.status().is_success() {
            return Err("material reconciliation rejected".into());
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Settled {
            schema: String,
            settled_input_refs: Vec<String>,
        }
        let mut bytes = Vec::new();
        response
            .take(HELPER_JSON_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "material reconciliation response failed")?;
        if bytes.len() > HELPER_JSON_LIMIT {
            return Err("material reconciliation response oversized".into());
        }
        let settled: Settled = serde_json::from_slice(&bytes)
            .map_err(|_| "material reconciliation response invalid")?;
        let expected = pending
            .iter()
            .map(|value| value.resolved_input.input_ref.as_str())
            .collect::<BTreeSet<_>>();
        if settled.schema != "workspace.material.reconcile.result.v1"
            || settled.settled_input_refs.len() != expected.len()
            || settled
                .settled_input_refs
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != expected
        {
            return Err("material reconciliation result mismatch".into());
        }
        for input in pending {
            self.confirm_material_input_outputs(&input.resolved_input);
        }
        Ok(())
    }

    pub(crate) fn confirm_material_input_outputs(&self, input: &ResolvedInput) {
        let consumer = material_input_consumer(input);
        self.consume_output_group(&consumer, &format!("material-settlement:{consumer}"));
    }

    pub(crate) fn execution_input_path(
        &self,
        input: &ResolvedInput,
    ) -> Result<Option<String>, String> {
        let _activity = self.recovery_activity.begin_dispatch_activity()?;
        let _inputs = self
            .recovery_activity
            .input_operations()
            .lock()
            .map_err(|_| "sandbox input operation lock poisoned")?;
        match self
            .inspect_material_input(input, &material_input_consumer(input))?
            .as_ref()
        {
            None => Ok(None),
            Some(existing)
                if existing.state.is_none()
                    && existing.virtual_path == input.virtual_path
                    && existing.source_version == input.source_version
                    && existing.size_bytes == input.size_bytes
                    && existing.sha256 == input.sha256 =>
            {
                Ok(Some(format!(
                    "{}/{}",
                    self.workspace_root().trim_end_matches('/'),
                    input.virtual_path
                )))
            }
            Some(_) => Err("execution input identity conflict".into()),
        }
    }

    pub(crate) fn materialized_input_bytes(&self) -> Result<u64, String> {
        self.materialized_inputs
            .lock()
            .map_err(|_| "sandbox input registry lock poisoned".to_string())?
            .values()
            .filter(|input| input.state.is_none())
            .try_fold(0_u64, |total, input| {
                total
                    .checked_add(input.size_bytes)
                    .ok_or_else(|| "sandbox input size overflow".to_string())
            })
    }

    pub(super) fn reload_input_inventory(&self) -> Result<(), String> {
        let output = self.helper_inner("input-inventory", None, &[], HELPER_JSON_LIMIT)?;
        let inventory: SandboxInputInventory =
            serde_json::from_slice(&output).map_err(|_| "sandbox input inventory invalid")?;
        if inventory.schema != SANDBOX_INPUT_INVENTORY_SCHEMA {
            return Err("sandbox input inventory schema mismatch".into());
        }
        *self
            .materialized_inputs
            .lock()
            .map_err(|_| "sandbox input registry lock poisoned")? = inventory
            .inputs
            .into_iter()
            .map(|input| (input.input_ref.clone(), input))
            .collect();
        Ok(())
    }

    fn inspect_material_input(
        &self,
        input: &ResolvedInput,
        consumer: &str,
    ) -> Result<Option<SandboxMaterializedInput>, String> {
        let expected = SandboxMaterializedInput {
            input_ref: input.input_ref.clone(),
            virtual_path: input.virtual_path.clone(),
            size_bytes: input.size_bytes,
            sha256: input.sha256.clone(),
            source_version: input.source_version.clone(),
            state: None,
        };
        let body = serde_json::to_vec(&serde_json::json!({"inputs":[expected]}))
            .map_err(|_| "material inspection invalid")?;
        self.recovery_activity.invalidate_before_dispatch()?;
        let output = self.helper_for_consumer(
            "input-inventory",
            None,
            &body,
            HELPER_JSON_LIMIT,
            Some(consumer),
        )?;
        let mut inventory: SandboxInputInventory =
            serde_json::from_slice(&output).map_err(|_| "material inventory invalid")?;
        if inventory.schema != SANDBOX_INPUT_INVENTORY_SCHEMA || inventory.inputs.len() > 1 {
            return Err("material inventory schema mismatch".into());
        }
        let actual = inventory.inputs.pop();
        if actual.as_ref().is_some_and(|value| {
            value.input_ref != expected.input_ref
                || value.virtual_path != expected.virtual_path
                || value.sha256 != expected.sha256
                || value.source_version != expected.source_version
                || value.size_bytes != expected.size_bytes
        }) {
            return Err("material inventory identity mismatch".into());
        }
        let mut registry = self
            .materialized_inputs
            .lock()
            .map_err(|_| "sandbox input registry lock poisoned")?;
        if let Some(value) = &actual {
            registry.insert(value.input_ref.clone(), value.clone());
        } else {
            registry.remove(&input.input_ref);
        }
        Ok(actual)
    }

    pub(crate) fn materialize_execution_input(
        &self,
        frame: VerifiedMaterialInputFrame,
    ) -> Result<String, String> {
        let (input, projected, mut file, length, digest) = frame.into_parts();
        // Enter dispatch before the input writer lock: a checkpoint never waits
        // for an input writer that is itself waiting for snapshot admission.
        let _activity = self.recovery_activity.begin_dispatch_activity()?;
        let _inputs = self
            .recovery_activity
            .input_operations()
            .lock()
            .map_err(|_| "sandbox input operation lock poisoned")?;
        let consumer = material_input_consumer(&input);
        self.inspect_material_input(&input, &consumer)?;
        let total = {
            let registry = self
                .materialized_inputs
                .lock()
                .map_err(|_| "sandbox input registry lock poisoned")?;
            if let Some(existing) = registry.get(&input.input_ref) {
                if existing != &projected {
                    return Err("execution input identity conflict".into());
                }
            }
            if registry.values().any(|existing| {
                existing.input_ref != input.input_ref && existing.virtual_path == input.virtual_path
            }) {
                return Err("execution input path conflict".into());
            }
            registry
                .values()
                .filter(|existing| {
                    existing.state.is_none() && existing.input_ref != input.input_ref
                })
                .try_fold(input.size_bytes, |total, existing| {
                    total
                        .checked_add(existing.size_bytes)
                        .ok_or("sandbox input size overflow")
                })?
        };
        if total > self.resources.workspace_bytes / 2 {
            return Err("execution inputs exceed half of workspaceBytes".into());
        }
        self.require_workspace_capacity(0, total)?;
        self.recovery_activity.invalidate_before_dispatch()?;
        let (_, _, exit) = self.supervised_helper_file_for_consumer(
            TrustedHelperKind::MaterializeInput,
            &mut file,
            length,
            &digest,
            Some(&consumer),
        )?;
        if exit != Some(0) {
            return Err("environment_helper_failed".into());
        }
        self.materialized_inputs
            .lock()
            .map_err(|_| "sandbox input registry lock poisoned")?
            .insert(projected.input_ref.clone(), projected);
        Ok(format!(
            "{}/{}",
            self.workspace_root().trim_end_matches('/'),
            input.virtual_path
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material_execution::PendingInput;

    fn pending(state: &str) -> PendingInput {
        serde_json::from_value(serde_json::json!({
            "state":state,"reservations":[],"resolvedInput":{
                "schema":"runtime.resolved_input.v1","inputRef":"library-original",
                "objectRef":"library-file","ownerKind":"userLibraryObject",
                "virtualPath":"inputs/library-original/file.zip","displayName":"file.zip",
                "contentType":"application/zip","sizeBytes":3,"sourceVersion":"1",
                "sha256":format!("sha256:{:x}",Sha256::digest(b"zip")),
                "evidenceKind":"userProvided","citationAllowed":false
            }
        }))
        .unwrap()
    }

    #[test]
    fn material_output_consumers_separate_versions_and_stable_execution_paths() {
        let first = pending("access_revoked").resolved_input;
        let consumer = material_input_consumer(&first);
        assert_eq!(consumer, material_input_consumer(&first.clone()));
        let mut other = first.clone();
        other.source_version = "2".into();
        assert_ne!(consumer, material_input_consumer(&other));
        other = first;
        other.input_ref = "other-original".into();
        assert_ne!(consumer, material_input_consumer(&other));
    }

    #[test]
    fn confirmed_invalid_input_without_reservations_requires_physical_revocation() {
        for state in [
            "asset_removed",
            "access_revoked",
            "stale_generation",
            "asset_unavailable",
        ] {
            assert_eq!(
                pending_revocation(&pending(state)).unwrap(),
                Some(ExecutionInputState::AccessRevoked),
                "state {state}"
            );
        }
    }

    #[test]
    fn active_or_unknown_pending_without_reservations_remains_rejected() {
        assert!(pending_revocation(&pending("active")).is_err());
        assert!(pending_revocation(&pending("unknown")).is_err());
        let mut invalid_identity = pending("access_revoked");
        invalid_identity.resolved_input.sha256 = "changed".into();
        assert!(pending_revocation(&invalid_identity).is_err());
    }
}

//! Synchronous SDK effects use one durable dispatch fact, never a resident task PID.
use super::*;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

pub(crate) const COMPLETED_SCHEMA: &str = "runtime.provider_environment.maintenance.completed.v1";
pub(crate) const DISPATCHED_SCHEMA: &str = "runtime.provider_environment.maintenance.dispatched.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NativeDirectoryEntry {
    pub path: String,
    pub kind: NativeEntryKind,
    pub size_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum NativeEntryKind {
    File,
    Directory,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum DirectOutput {
    Command {
        exit_code: i32,
        stdout_bytes_base64: String,
        stdout_byte_length: u64,
        stderr_bytes_base64: String,
        stderr_byte_length: u64,
    },
    FileWritten {
        byte_length: u64,
        sha256: String,
    },
    FileRead {
        data_base64: String,
        byte_length: u64,
        sha256: String,
    },
    DirectoryListed {
        entries: Vec<NativeDirectoryEntry>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DirectCompletion {
    pub schema: String,
    pub operation_id: String,
    pub environment_id: String,
    pub provider_target: ProviderMaintenanceTarget,
    pub profile_digest: String,
    pub request_digest: String,
    pub authorization_digest: String,
    pub controller_epoch: i64,
    pub controller_lease_digest: String,
    pub output: DirectOutput,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectDispatchState {
    Fresh,
    Unknown,
    Completed(Box<DirectCompletion>),
}

impl DirectCompletion {
    pub(crate) fn new(
        auth: &ProviderMaintenanceAuthorization,
        output: DirectOutput,
    ) -> Result<Self, String> {
        let b = &auth.payload;
        let result = Self {
            schema: COMPLETED_SCHEMA.into(),
            operation_id: operation_id(b)?.into(),
            environment_id: b.environment_id.clone(),
            provider_target: b.provider_target.clone(),
            profile_digest: b.profile_digest.clone(),
            request_digest: b.request_digest.clone(),
            authorization_digest: b.digest()?,
            controller_epoch: b.controller_epoch,
            controller_lease_digest: b.controller_lease_digest.clone(),
            output,
        };
        result.validate_for(auth)?;
        Ok(result)
    }
    pub(crate) fn validate_for(
        &self,
        auth: &ProviderMaintenanceAuthorization,
    ) -> Result<(), String> {
        let b = &auth.payload;
        if self.schema != COMPLETED_SCHEMA
            || self.operation_id != operation_id(b)?
            || self.environment_id != b.environment_id
            || self.provider_target != b.provider_target
            || self.profile_digest != b.profile_digest
            || self.request_digest != b.request_digest
            || self.authorization_digest != b.digest()?
            || self.controller_epoch != b.controller_epoch
            || self.controller_lease_digest != b.controller_lease_digest
            || serde_json::to_vec(self)
                .map_err(|_| "provider_direct_completion_invalid")?
                .len()
                > crate::MAX_HTTP_BODY_BYTES
        {
            return Err("provider_direct_completion_invalid".into());
        }
        let bytes = |encoded: &str, length: u64| -> Result<Vec<u8>, String> {
            if length > crate::MAX_HTTP_BODY_BYTES as u64 {
                return Err("provider_direct_completion_invalid".into());
            }
            let raw = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| "provider_direct_completion_invalid")?;
            if raw.len() as u64 != length
                || base64::engine::general_purpose::STANDARD.encode(&raw) != encoded
            {
                return Err("provider_direct_completion_invalid".into());
            }
            Ok(raw)
        };
        match &self.output {
            DirectOutput::Command {
                stdout_bytes_base64,
                stdout_byte_length,
                stderr_bytes_base64,
                stderr_byte_length,
                ..
            } => {
                bytes(stdout_bytes_base64, *stdout_byte_length)?;
                bytes(stderr_bytes_base64, *stderr_byte_length)?;
            }
            DirectOutput::FileWritten {
                byte_length,
                sha256,
            }
            | DirectOutput::FileRead {
                byte_length,
                sha256,
                ..
            } => {
                if *byte_length > 32 * 1024 * 1024 {
                    return Err("provider_direct_completion_invalid".into());
                }
                if !sha256.strip_prefix("sha256:").is_some_and(|hex| {
                    hex.len() == 64
                        && hex
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }) {
                    return Err("provider_direct_completion_invalid".into());
                }
                if let DirectOutput::FileRead { data_base64, .. } = &self.output {
                    let raw = bytes(data_base64, *byte_length)?;
                    if format!("sha256:{:x}", Sha256::digest(&raw)) != *sha256 {
                        return Err("provider_direct_completion_invalid".into());
                    }
                }
            }
            DirectOutput::DirectoryListed { entries } => {
                let mut seen = HashSet::new();
                if entries.len() > 10000 {
                    return Err("provider_direct_completion_invalid".into());
                }
                for entry in entries {
                    let name = &entry.path;
                    if !name.starts_with('/')
                        || name.starts_with("//")
                        || name.contains('\\')
                        || name.chars().any(char::is_control)
                        || name.nfc().collect::<String>() != *name
                        || (name != "/"
                            && name
                                .split('/')
                                .skip(1)
                                .any(|p| p.is_empty() || p == "." || p == ".."))
                        || !seen.insert(name)
                    {
                        return Err("provider_direct_completion_invalid".into());
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn dispatch_record(&self) -> Result<Value, String> {
        let mut value =
            serde_json::to_value(self).map_err(|_| "provider_direct_completion_invalid")?;
        let fields = value
            .as_object_mut()
            .ok_or("provider_direct_completion_invalid")?;
        fields.remove("output");
        fields.insert("schema".into(), json!(DISPATCHED_SCHEMA));
        Ok(value)
    }
}

fn expected_dispatch(auth: &ProviderMaintenanceAuthorization) -> Result<Value, String> {
    let b = &auth.payload;
    Ok(
        json!({"schema":DISPATCHED_SCHEMA,"operationId":operation_id(b)?,"environmentId":b.environment_id,
        "providerTarget":b.provider_target,"profileDigest":b.profile_digest,"requestDigest":b.request_digest,
        "authorizationDigest":b.digest()?,"controllerEpoch":b.controller_epoch,"controllerLeaseDigest":b.controller_lease_digest}),
    )
}
fn event(
    tx: &mut postgres::Transaction<'_>,
    id: &str,
    session: &str,
    schema: &str,
) -> Result<Option<(i64, Value)>, String> {
    let Some(row)=tx.query_opt("SELECT session_id,task_id,event_type,at_ms,visibility,payload_json FROM runtime.runtime_events WHERE event_id=$1 FOR SHARE",&[&id])
        .map_err(|_|"provider_direct_event_unavailable")? else {return Ok(None)};
    let encoded: String = row.get(5);
    if row.get::<_, String>(0) != session
        || row.get::<_, Option<String>>(1).is_some()
        || row.get::<_, String>(2) != schema
        || row.get::<_, String>(4) != "internal"
        || encoded.len() > crate::MAX_HTTP_BODY_BYTES
    {
        return Err("provider_direct_event_binding_rejected".into());
    }
    Ok(Some((
        row.get(3),
        serde_json::from_str(&encoded).map_err(|_| "provider_direct_event_invalid")?,
    )))
}
fn insert_event(
    tx: &mut postgres::Transaction<'_>,
    id: &str,
    session: &str,
    schema: &str,
    payload: &Value,
) -> Result<u64, String> {
    let encoded = serde_json::to_string(payload).map_err(|_| "provider_direct_event_invalid")?;
    if encoded.len() > crate::MAX_HTTP_BODY_BYTES {
        return Err("provider_direct_completion_budget_exceeded".into());
    }
    tx.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,NULL,$3,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,'internal',$4) ON CONFLICT(event_id) DO NOTHING",
        &[&id,&session,&schema,&encoded]).map_err(|_|"provider_direct_event_unavailable".into())
}
fn completed(
    tx: &mut postgres::Transaction<'_>,
    auth: &ProviderMaintenanceAuthorization,
    dispatch_at: i64,
) -> Result<Option<DirectCompletion>, String> {
    let b = &auth.payload;
    let operation = operation_id(b)?;
    let Some((at, payload)) = event(
        tx,
        &format!("provider_maintenance_completed:{operation}"),
        &b.session_id,
        COMPLETED_SCHEMA,
    )?
    else {
        return Ok(None);
    };
    if at < dispatch_at {
        return Err("provider_direct_completion_event_rejected".into());
    }
    let completion: DirectCompletion =
        serde_json::from_value(payload).map_err(|_| "provider_direct_completion_invalid")?;
    completion.validate_for(auth)?;
    Ok(Some(completion))
}
fn reject_task_completion(
    tx: &mut postgres::Transaction<'_>,
    operation: &str,
) -> Result<(), String> {
    let exists:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.provider_environment_maintenance_completions WHERE operation_id=$1)",&[&operation])
        .map_err(|_|"provider_direct_completion_unavailable")?.get(0);
    if exists {
        return Err("provider_direct_task_contract_conflict".into());
    }
    Ok(())
}

impl PostgresRuntimeStore {
    /// Persist CAS before entering an SDK callback. A later HTTP request or
    /// replacement process sees Unknown until the original full result commits.
    pub(crate) fn claim_direct_provider_maintenance(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        key: &[u8],
        configured_digest: &str,
    ) -> Result<DirectDispatchState, String> {
        let payload = expected_dispatch(auth)?;
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|_| "provider_direct_dispatch_unavailable")?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            locked_target(&mut tx, auth, key, configured_digest)?;
            if auth.payload.controller_purpose != EnvironmentTaskControllerPurpose::Execute {
                return Err("provider_direct_execute_authority_required".into());
            }
            let operation = operation_id(&auth.payload)?;
            reject_task_completion(&mut tx, operation)?;
            let id = format!("provider_maintenance_dispatched:{operation}");
            let inserted = insert_event(
                &mut tx,
                &id,
                &auth.payload.session_id,
                DISPATCHED_SCHEMA,
                &payload,
            )?;
            let (at, stored) = event(&mut tx, &id, &auth.payload.session_id, DISPATCHED_SCHEMA)?
                .ok_or("provider_direct_dispatch_missing")?;
            if stored != payload {
                return Err("provider_direct_dispatch_binding_rejected".into());
            }
            let state = match completed(&mut tx, auth, at)? {
                Some(result) => DirectDispatchState::Completed(Box::new(result)),
                None if inserted == 1 => DirectDispatchState::Fresh,
                None => DirectDispatchState::Unknown,
            };
            tx.commit()
                .map_err(|_| "provider_direct_dispatch_commit_unknown")?;
            Ok(state)
        })
    }
    pub(crate) fn record_direct_provider_maintenance_completion(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        key: &[u8],
        configured_digest: &str,
        completion: &DirectCompletion,
    ) -> Result<(), String> {
        completion.validate_for(auth)?;
        let payload =
            serde_json::to_value(completion).map_err(|_| "provider_direct_completion_invalid")?;
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|_| "provider_direct_completion_unavailable")?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            locked_target(&mut tx, auth, key, configured_digest)?;
            let b = &auth.payload;
            let operation = operation_id(b)?;
            reject_task_completion(&mut tx, operation)?;
            let (at, stored) = event(
                &mut tx,
                &format!("provider_maintenance_dispatched:{operation}"),
                &b.session_id,
                DISPATCHED_SCHEMA,
            )?
            .ok_or("provider_direct_dispatch_missing")?;
            if stored != expected_dispatch(auth)? {
                return Err("provider_direct_dispatch_binding_rejected".into());
            }
            if let Some(previous) = completed(&mut tx, auth, at)? {
                if previous != *completion {
                    return Err("provider_direct_completion_conflict".into());
                }
                tx.commit()
                    .map_err(|_| "provider_direct_completion_commit_unknown")?;
                return Ok(());
            }
            let complete_id = format!("provider_maintenance_completed:{operation}");
            insert_event(
                &mut tx,
                &complete_id,
                &b.session_id,
                COMPLETED_SCHEMA,
                &payload,
            )?;
            let (_, stored) = event(&mut tx, &complete_id, &b.session_id, COMPLETED_SCHEMA)?
                .ok_or("provider_direct_completion_event_missing")?;
            if stored != payload {
                return Err("provider_direct_completion_conflict".into());
            }
            if completed(&mut tx, auth, at)?.as_ref() != Some(completion) {
                return Err("provider_direct_completion_conflict".into());
            }
            tx.commit()
                .map_err(|_| "provider_direct_completion_commit_unknown")?;
            Ok(())
        })
    }
}

//! Immutable cleanup facts. Current controller authority is checked separately.
use super::controller_authority::{
    verify_controller_authority, ControllerAuthorityRequest, ControllerPurpose,
};
use super::PostgresRuntimeStore;
use crate::postgres_store::controller_authority::VerifiedOriginalGrant;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use crate::resident_capacity::ObservedResidentSandbox;
use centaeris_core::execution::ResidentResourceUsage;
use centaeris_core::session::RuntimeJobLeaseFence;
use postgres::GenericClient;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) struct CleanupAuthorityRequest<'a> {
    pub cleanup: &'a crate::contract::AgentRunCleanup,
    pub signing_key: &'a [u8],
    pub lease: &'a RuntimeJobLeaseFence,
    pub host_id: &'a str,
}

#[derive(Debug)]
pub(crate) enum CleanupTarget {
    Destroyed {
        instance_id: String,
    },
    Instance {
        allocation: Box<RetainedInstanceAllocation>,
        resources: ResidentResourceUsage,
        origin_authorization: Box<crate::agent_run_authorization::WorkspaceAgentRunAuthorization>,
    },
    NeverAllocated,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CleanupReceiptQuery {
    pub schema: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub cleanup_receipt_id: String,
}
impl CleanupReceiptQuery {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let opaque = |value: &str| {
            !value.is_empty()
                && value.len() <= 160
                && value.trim() == value
                && !value
                    .chars()
                    .any(|c| c.is_control() || c == '/' || c == '\\')
        };
        if self.schema != "runtime.agent_run.cleanup_receipt.get.v1"
            || !opaque(&self.agent_run_id)
            || !opaque(&self.cleanup_receipt_id)
            || !self
                .authorization_digest
                .strip_prefix("sha256:")
                .is_some_and(|v| {
                    v.len() == 64
                        && v.bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                })
        {
            return Err("cleanup_receipt_query_invalid".into());
        }
        Ok(())
    }
}

impl PostgresRuntimeStore {
    pub(crate) fn check_cleanup_authority(
        &self,
        cleanup: &crate::contract::AgentRunCleanup,
        signing_key: &[u8],
        lease: &RuntimeJobLeaseFence,
    ) -> Result<VerifiedOriginalGrant, String> {
        cleanup.clone().validate(signing_key)?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            let verified = verify_controller_authority(
                &mut tx,
                &ControllerAuthorityRequest {
                    environment_authorization: &cleanup.environment_authorization,
                    signing_key,
                    workspace_id: &cleanup.authorization.workspace_id,
                    lease,
                    purpose: ControllerPurpose::Cleanup,
                },
            )?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(verified)
        })
    }
    pub(crate) fn get_cleanup_receipt(
        &self,
        request: &CleanupReceiptQuery,
    ) -> Result<Option<CleanupReceipt>, String> {
        request.validate()?;
        self.with_client(|client| {
            let Some(receipt) = load_receipt(client, &request.agent_run_id)? else { return Ok(None) };
            if receipt.receipt_id != request.cleanup_receipt_id || receipt.authorization_digest != request.authorization_digest {
                return Ok(None);
            }
            let original=client.query_opt("SELECT a.environment_id,g.digest FROM public.app_core_agentrun a JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=a.id WHERE a.id=$1", &[&request.agent_run_id]).map_err(|e|e.to_string())?.ok_or("cleanup receipt original binding missing")?;
            if original.get::<_,Option<String>>(0).as_deref()!=Some(receipt.environment_id.as_str()) || original.get::<_,String>(1)!=receipt.authorization_digest {
                return Err("cleanup receipt original binding conflict".into());
            }
            Ok(Some(receipt))
        })
    }
    pub(crate) fn with_cleanup_authority(
        &self,
        request: &CleanupAuthorityRequest<'_>,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
        cleanup: impl FnOnce(&CleanupTarget, &VerifiedOriginalGrant) -> Result<(), String> + Send,
    ) -> Result<CleanupReceipt, String> {
        request.cleanup.clone().validate(request.signing_key)?;
        let authority = ControllerAuthorityRequest {
            environment_authorization: &request.cleanup.environment_authorization,
            signing_key: request.signing_key,
            workspace_id: &request.cleanup.authorization.workspace_id,
            lease: request.lease,
            purpose: ControllerPurpose::Cleanup,
        };
        self.with_client(|client| {
            // An immutable completed fact remains valid after its old physical
            // target disappears. Replay still requires today's cleanup authority.
            let mut tx=client.transaction().map_err(|e|e.to_string())?;
            let verified=verify_controller_authority(&mut tx,&authority)?;
            if let Some(receipt)=load_receipt(&mut tx,&request.cleanup.agent_run_id)? {
                validate_replay(&receipt,request,&verified)?;
                tx.commit().map_err(|e|e.to_string())?;
                return Ok(receipt);
            }
            tx.commit().map_err(|e|e.to_string())?;
            super::residency::with_host_inventory(client,request.host_id,inventory,|client,physical| {
                let mut tx=client.transaction().map_err(|e|e.to_string())?;
                super::resource_commit::require_durable_resource_commit(&mut tx)?;
                let verified=verify_controller_authority(&mut tx,&authority)?;
                if let Some(receipt)=load_receipt(&mut tx,&request.cleanup.agent_run_id)? {
                    validate_replay(&receipt,request,&verified)?;
                    tx.commit().map_err(|e|e.to_string())?;
                    return Ok(receipt);
                }
                let target=resolve_target(&mut tx,request,physical)?;
                cleanup(&target,&verified)?;
                // Wall-clock lease expiry can occur while physical cleanup runs
                // even though row locks prevent a competing owner taking over.
                verify_controller_authority(&mut tx,&authority)?;
                let allocation=match &target {
                    CleanupTarget::Instance {allocation,..} => CleanupAllocation::Instance {instance_id:allocation.instance_id.clone()},
                    CleanupTarget::Destroyed {instance_id} => CleanupAllocation::Instance {instance_id:instance_id.clone()},
                    CleanupTarget::NeverAllocated => CleanupAllocation::NeverAllocated {},
                };
                let mut hash=Sha256::new();
                hash.update(b"runtime:agent-run-cleanup-receipt:v1\0");
                for identity in [&request.cleanup.agent_run_id,&request.cleanup.authorization_digest,&request.cleanup.environment_authorization.payload.environment_id] {
                    hash.update(identity.as_bytes()); hash.update([0]);
                }
                let receipt=CleanupReceipt {schema:"runtime.agent_run.cleanup_receipt.v1".into(),receipt_id:format!("cleanup_receipt_{:x}",hash.finalize()),
                    agent_run_id:request.cleanup.agent_run_id.clone(),environment_id:request.cleanup.environment_authorization.payload.environment_id.clone(),
                    allocation,authorization_digest:verified.authorization_digest.clone(),controller_epoch:verified.controller_epoch,cleanup_complete:true};
                let (kind,instance)=match &receipt.allocation {
                    CleanupAllocation::Instance {instance_id} => ("instance",Some(instance_id.as_str())),
                    CleanupAllocation::NeverAllocated {} => ("neverAllocated",None),
                };
                tx.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,true,'deferred',$8) ON CONFLICT(agent_run_id) DO NOTHING",
                    &[&receipt.receipt_id,&receipt.agent_run_id,&receipt.environment_id,&receipt.authorization_digest,&receipt.controller_epoch,&kind,&instance,&crate::now_ms()?]).map_err(|e|e.to_string())?;
                let stored=load_receipt(&mut tx,&receipt.agent_run_id)?.ok_or("cleanup receipt commit missing")?;
                if stored!=receipt {return Err("cleanup receipt identity conflict".into());}
                tx.commit().map_err(|e|e.to_string())?;
                Ok(stored)
            })
        })
    }
}

fn validate_replay(
    receipt: &CleanupReceipt,
    r: &CleanupAuthorityRequest<'_>,
    verified: &VerifiedOriginalGrant,
) -> Result<(), String> {
    if receipt.agent_run_id != r.cleanup.agent_run_id
        || receipt.environment_id != r.cleanup.environment_authorization.payload.environment_id
        || receipt.authorization_digest != verified.authorization_digest
        || !receipt.cleanup_complete
        || !(1..=verified.controller_epoch).contains(&receipt.controller_epoch)
    {
        return Err("cleanup receipt identity conflict".into());
    }
    Ok(())
}

fn load_receipt(db: &mut impl GenericClient, run: &str) -> Result<Option<CleanupReceipt>, String> {
    let row=db.query_opt("SELECT receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1", &[&run]).map_err(|e|e.to_string())?;
    row.map(|row| {
        let kind: String = row.get(5);
        let instance: Option<String> = row.get(6);
        let allocation = match (kind.as_str(), instance) {
            ("instance", Some(instance_id)) if !instance_id.is_empty() => {
                CleanupAllocation::Instance { instance_id }
            }
            ("neverAllocated", None) => CleanupAllocation::NeverAllocated {},
            _ => return Err("cleanup receipt allocation invalid".into()),
        };
        if row.get::<_, String>(8) != "deferred" || !row.get::<_, bool>(7) {
            return Err("cleanup receipt confirmation invalid".into());
        }
        Ok(CleanupReceipt {
            schema: "runtime.agent_run.cleanup_receipt.v1".into(),
            receipt_id: row.get(0),
            agent_run_id: row.get(1),
            environment_id: row.get(2),
            authorization_digest: row.get(3),
            controller_epoch: row.get(4),
            allocation,
            cleanup_complete: true,
        })
    })
    .transpose()
}

fn resolve_target(
    db: &mut impl GenericClient,
    r: &CleanupAuthorityRequest<'_>,
    physical: &[ObservedResidentSandbox],
) -> Result<CleanupTarget, String> {
    let b = &r.cleanup.environment_authorization.payload;
    // Unknown reservations cannot be silently treated as absent allocations.
    let unknown=db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.resident_sandboxes WHERE host_id=$1 AND (allocation_mode='unbound' OR environment_id IS NULL OR (environment_id=$2 AND (state<>'resident' OR container_id IS NULL OR instance_id IS NULL))))", &[&r.host_id,&b.environment_id]).map_err(|e|e.to_string())?.get::<_,bool>(0);
    if unknown {
        return Err("cleanup inventory has unresolved reservation".into());
    }
    let associations=db.query("SELECT allocation_execution_id,environment_id,session_id,authorization_digest,allocation_mode,host_id FROM runtime.execution_instances WHERE agent_run_id=$1", &[&b.agent_run_id]).map_err(|e|e.to_string())?;
    let mut anchors = std::collections::BTreeSet::new();
    for row in &associations {
        if row.get::<_, String>(1) != b.environment_id
            || row.get::<_, String>(2) != b.session_id
            || row.get::<_, String>(3) != b.agent_run_authorization_digest
            || row.get::<_, String>(4) != "retained"
            || row.get::<_, String>(5) != r.host_id
        {
            return Err("cleanup execution association rejected".into());
        }
        anchors.insert(row.get::<_, String>(0));
    }
    if anchors.len() > 1 {
        return Err("cleanup execution allocation ambiguous".into());
    }
    if associations.is_empty() {
        let history=db.query_one("SELECT EXISTS(SELECT 1 FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND session_level=false AND payload->>'type' IN('agent_run_execution_started','agent_run_execution_ended'))", &[&b.agent_run_id]).map_err(|e|e.to_string())?.get::<_,bool>(0);
        if history {
            return Err("cleanup execution association missing".into());
        }
    }
    let holdings=db.query("SELECT execution_id,instance_id,allocation_mode,host_id FROM runtime.resident_sandboxes WHERE environment_id=$1", &[&b.environment_id]).map_err(|e|e.to_string())?;
    if let Some(retirement) = db.query_opt("SELECT instance_id,allocation_execution_id,container_id,host_id,state FROM runtime.environment_retirements WHERE environment_id=$1", &[&b.environment_id]).map_err(|e| e.to_string())? {
        let origin: String = retirement.get(1);
        let container: String = retirement.get(2);
        if retirement.get::<_,String>(3) != r.host_id || retirement.get::<_,String>(4) != "destroyed"
            || !holdings.is_empty() || (!anchors.is_empty() && !anchors.contains(&origin))
            || physical.iter().any(|p| p.container_id==container || p.execution_id==origin) {
            return Err("cleanup maintenance destruction unconfirmed".into());
        }
        return Ok(CleanupTarget::Destroyed {instance_id:retirement.get(0)});
    }
    if holdings.is_empty() {
        if associations.is_empty() {
            return Ok(CleanupTarget::NeverAllocated);
        }
        return Err("cleanup execution environment lost".into());
    }
    if holdings.len() != 1
        || holdings[0].get::<_, String>(2) != "retained"
        || holdings[0].get::<_, String>(3) != r.host_id
    {
        return Err("cleanup environment holding ambiguous".into());
    }
    let execution: String = holdings[0].get(0);
    if !anchors.is_empty() && !anchors.contains(&execution) {
        return Err("cleanup execution allocation mismatch".into());
    }
    let row=db.query_opt("SELECT i.execution_id,i.agent_run_id,i.instance_id,i.environment_id,i.host_id,i.image_digest,i.authorization_digest,i.binding_digest,i.profile_digest,s.container_id,s.workspace_id,s.memory_bytes,s.cpu_milli,s.pids,s.workspace_bytes,s.state FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id WHERE i.execution_id=$1 AND i.allocation_execution_id=i.execution_id AND i.allocation_mode='retained' FOR UPDATE OF s", &[&execution]).map_err(|e|e.to_string())?.ok_or("cleanup allocation anchor missing")?;
    let resources = ResidentResourceUsage {
        sandbox_count: 1,
        memory_bytes: row
            .get::<_, i64>(11)
            .try_into()
            .map_err(|_| "cleanup memory invalid")?,
        cpu_milli: row
            .get::<_, i64>(12)
            .try_into()
            .map_err(|_| "cleanup cpu invalid")?,
        pids: row
            .get::<_, i64>(13)
            .try_into()
            .map_err(|_| "cleanup pids invalid")?,
        workspace_bytes: row
            .get::<_, i64>(14)
            .try_into()
            .map_err(|_| "cleanup workspace invalid")?,
    };
    let allocation = RetainedInstanceAllocation {
        allocation_execution_id: row.get(0),
        allocation_agent_run_id: row.get(1),
        instance_id: row.get(2),
        environment_id: row.get(3),
        host_id: row.get(4),
        image_digest: row.get(5),
        authorization_digest: row.get(6),
        binding_digest: row.get(7),
        profile_digest: row.get(8),
        container_id: row.get(9),
        reused: true,
    };
    if allocation.environment_id != b.environment_id
        || allocation.host_id != r.host_id
        || row.get::<_, String>(10) != r.cleanup.authorization.workspace_id
        || row.get::<_, String>(15) != "resident"
        || !physical.iter().any(|p| {
            p.execution_id == execution
                && p.agent_run_id == allocation.allocation_agent_run_id
                && Some(&p.container_id) == allocation.container_id.as_ref()
                && p.resources == resources
        })
    {
        return Err("cleanup live environment unconfirmed".into());
    }
    // The container may have been created by a previous Run. Its fixed profile
    // is described only by that immutable signed origin grant, never today's
    // cleanup controller's potentially different plugins or resource request.
    let origin = db.query_opt("SELECT g.digest,g.payload::text,g.signature,a.environment_id,a.session_id,a.workspace_id,a.user_id::text,a.\"modelConfig_id\",a.\"thinkingMode\" FROM public.app_core_agentrunauthorization g JOIN public.app_core_agentrun a ON a.id=g.agent_run_id WHERE a.id=$1 FOR SHARE OF g,a", &[&allocation.allocation_agent_run_id]).map_err(|_| "cleanup origin grant unavailable")?.ok_or("cleanup origin grant missing")?;
    let digest: String = origin.get(0);
    let origin_authorization: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
        serde_json::from_str(&origin.get::<_, String>(1))
            .map_err(|_| "cleanup origin grant invalid")?;
    origin_authorization
        .checked_binding(&allocation.allocation_agent_run_id)?
        .verify(&digest, r.signing_key, &origin.get::<_, String>(2))?;
    if digest != allocation.authorization_digest
        || origin.get::<_, Option<String>>(3).as_deref() != Some(b.environment_id.as_str())
        || origin.get::<_, String>(4) != b.session_id
        || origin.get::<_, String>(5) != r.cleanup.authorization.workspace_id
        || origin_authorization.workspace_id != r.cleanup.authorization.workspace_id
        || origin_authorization.session_id != b.session_id
        || origin_authorization.agent_id != r.cleanup.authorization.agent_id
        || origin_authorization.user_id != r.cleanup.authorization.user_id
        || origin_authorization.user_id != origin.get::<_, String>(6)
        || origin_authorization.model_config_ref != origin.get::<_, String>(7)
        || origin_authorization.thinking_mode
            != origin
                .get::<_, Option<String>>(8)
                .filter(|value| !value.is_empty())
        || origin_authorization.image_digest != allocation.image_digest
        || crate::resident_capacity::declared_usage(origin_authorization.resources) != resources
    {
        return Err("cleanup origin grant binding rejected".into());
    }
    Ok(CleanupTarget::Instance {
        allocation: Box::new(allocation),
        resources,
        origin_authorization: Box::new(origin_authorization),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum CleanupAllocation {
    Instance {
        #[serde(rename = "instanceId")]
        instance_id: String,
    },
    NeverAllocated {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CleanupReceipt {
    pub schema: String,
    pub receipt_id: String,
    pub agent_run_id: String,
    pub environment_id: String,
    pub allocation: CleanupAllocation,
    pub authorization_digest: String,
    pub controller_epoch: i64,
    pub cleanup_complete: bool,
}

pub(super) const DDL: &str = "CREATE TABLE runtime.agent_run_cleanup_receipts(
    receipt_id text PRIMARY KEY, agent_run_id text NOT NULL UNIQUE, environment_id text NOT NULL,
    authorization_digest text NOT NULL, controller_epoch bigint NOT NULL CHECK(controller_epoch>0),
    allocation_kind text NOT NULL CHECK(allocation_kind IN('instance','neverAllocated')), instance_id text,
    cleanup_complete boolean NOT NULL CHECK(cleanup_complete),
    file_protection text NOT NULL CHECK(file_protection='deferred'), recorded_at_ms bigint NOT NULL,
    CHECK((allocation_kind='instance' AND instance_id IS NOT NULL AND length(instance_id)>0)
       OR (allocation_kind='neverAllocated' AND instance_id IS NULL))
);";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_is_an_exclusive_identity_or_proven_absence_not_an_unknown_alias() {
        for value in [
            serde_json::json!({"kind":"instance","instanceId":"instance-1"}),
            serde_json::json!({"kind":"neverAllocated"}),
        ] {
            let parsed: CleanupAllocation = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        }
        for value in [
            serde_json::json!({"kind":"instance"}),
            serde_json::json!({"kind":"neverAllocated","instanceId":"invented"}),
            serde_json::json!({"kind":"unknown"}),
            serde_json::Value::Null,
        ] {
            assert!(serde_json::from_value::<CleanupAllocation>(value).is_err());
        }
    }
}

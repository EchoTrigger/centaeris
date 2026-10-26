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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CleanupPhysicalResult {
    Retained,
    Removed {
        instance_id: String,
        container_id: String,
    },
}
impl From<()> for CleanupPhysicalResult {
    fn from(_: ()) -> Self {
        Self::Retained
    }
}

#[derive(Debug)]
pub(crate) enum CleanupTarget {
    Unmaterialized {
        instance_id: String,
    },
    Destroyed {
        instance_id: String,
    },
    Instance {
        allocation: Box<RetainedInstanceAllocation>,
        resources: ResidentResourceUsage,
        origin_authorization: Box<super::allocation_origin::AllocationOrigin>,
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
    pub(crate) fn with_cleanup_authority<P: Into<CleanupPhysicalResult>>(
        &self,
        request: &CleanupAuthorityRequest<'_>,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
        cleanup: impl FnOnce(&CleanupTarget, &VerifiedOriginalGrant) -> Result<P, String> + Send,
    ) -> Result<CleanupReceipt, String> {
        self.with_cleanup_authority_and_power_observation(
            request,
            inventory,
            |allocation, profile| {
                use crate::docker_execution_host::runsc_power::{
                    observe_power, RunscPowerObservation,
                };
                observe_power(allocation, profile)
                    .map(|state| state == RunscPowerObservation::Paused)
            },
            cleanup,
        )
    }
    pub(crate) fn with_cleanup_authority_and_power_observation<P: Into<CleanupPhysicalResult>>(
        &self,
        request: &CleanupAuthorityRequest<'_>,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
        observe_paused: impl FnOnce(
                &RetainedInstanceAllocation,
                &crate::agent_run_authorization::EnvironmentProfile,
            ) -> Result<bool, String>
            + Send,
        cleanup: impl FnOnce(&CleanupTarget, &VerifiedOriginalGrant) -> Result<P, String> + Send,
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
                require_physical_replay(&mut tx,request,&receipt)?;
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
                require_physical_replay(&mut tx,request,&receipt)?;
                    tx.commit().map_err(|e|e.to_string())?;
                    return Ok(receipt);
                }
                let target=resolve_target(&mut tx,request,physical)?;
                let per_run=request.cleanup.environment_authorization.payload.execution_mode=="perRun";
                if per_run{
                    if let CleanupTarget::Instance{allocation,..}=&target{
                        tx.execute("UPDATE runtime.resident_sandboxes SET state='releasing' WHERE host_id=$1 AND execution_id=$2 AND instance_id=$3",&[&request.host_id,&allocation.allocation_execution_id,&allocation.instance_id]).map_err(|e|e.to_string())?;
                        tx.commit().map_err(|e|e.to_string())?;
                        tx=client.transaction().map_err(|e|e.to_string())?;super::resource_commit::require_durable_resource_commit(&mut tx)?;
                        verify_controller_authority(&mut tx,&authority)?;
                    }
                }
                // A reused association can commit before its pre-host callback
                // fails. Only fresh proof of the original still-paused container
                // closes that unstarted Run without sending task control RPCs.
                let unstarted_paused = if let CleanupTarget::Instance {
                    allocation, origin_authorization, ..
                } = &target {
                    if unstarted_reused_resume(&mut tx, request, allocation)? {
                        observe_paused(allocation, &origin_authorization.environment_profile)?
                    } else { false }
                } else { false };
                let physical:CleanupPhysicalResult=if unstarted_paused {
                    CleanupPhysicalResult::Retained
                } else { cleanup(&target,&verified)?.into() };
                let (physical_removed,removed_container)=match(&target,physical){
                    (CleanupTarget::Instance{allocation,..},CleanupPhysicalResult::Removed{instance_id,container_id}) if per_run&&instance_id==allocation.instance_id&&allocation.container_id.as_deref()==Some(&container_id)=>{
                        let deleted=tx.execute("DELETE FROM runtime.resident_sandboxes WHERE host_id=$1 AND execution_id=$2 AND instance_id=$3 AND container_id=$4 AND state='releasing'",&[&request.host_id,&allocation.allocation_execution_id,&instance_id,&container_id]).map_err(|e|e.to_string())?;
                        if deleted!=1{return Err("cleanup_physical_release_unconfirmed".into());}(true,Some(container_id))
                    }
                    (CleanupTarget::Instance{..},CleanupPhysicalResult::Retained)if !per_run=>(false,None),
                    (CleanupTarget::NeverAllocated|CleanupTarget::Destroyed{..}|CleanupTarget::Unmaterialized{..},CleanupPhysicalResult::Retained)=>(false,None),
                    _=>return Err("cleanup_physical_release_unconfirmed".into()),
                };
                // Wall-clock lease expiry can occur while physical cleanup runs
                // even though row locks prevent a competing owner taking over.
                verify_controller_authority(&mut tx,&authority)?;
                let allocation=match &target {
                    CleanupTarget::Instance {allocation,..} => CleanupAllocation::Instance {instance_id:allocation.instance_id.clone()},
                    CleanupTarget::Destroyed {instance_id} | CleanupTarget::Unmaterialized {instance_id} => CleanupAllocation::Instance {instance_id:instance_id.clone()},
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
                tx.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms,physical_removed,removed_container_id) VALUES($1,$2,$3,$4,$5,$6,$7,true,'deferred',$8,$9,$10) ON CONFLICT(agent_run_id) DO NOTHING",
                    &[&receipt.receipt_id,&receipt.agent_run_id,&receipt.environment_id,&receipt.authorization_digest,&receipt.controller_epoch,&kind,&instance,&crate::now_ms()?,&physical_removed,&removed_container]).map_err(|e|e.to_string())?;
                let stored=load_receipt(&mut tx,&receipt.agent_run_id)?.ok_or("cleanup receipt commit missing")?;
                if stored!=receipt {return Err("cleanup receipt identity conflict".into());}
                if physical_removed {
                    super::host_retirement::enqueue(&mut tx,request.host_id,instance.ok_or("cleanup physical instance missing")?)?;
                }
                tx.commit().map_err(|e|e.to_string())?;
                Ok(stored)
            })
        })
    }
}

// This is an association-only cleanup proof, never a release or pause receipt.
// Caller holds the current cleanup authority and exact resident binding locks.
fn unstarted_reused_resume(
    db: &mut impl GenericClient,
    request: &CleanupAuthorityRequest<'_>,
    allocation: &RetainedInstanceAllocation,
) -> Result<bool, String> {
    let current = &request.cleanup.agent_run_id;
    if request
        .cleanup
        .environment_authorization
        .payload
        .execution_mode
        != "retained"
        || allocation
            .allocation_agent_run_id
            .as_ref()
            .is_none_or(|origin| origin == current)
    {
        return Ok(false);
    }
    let associated: bool = db.query_one(
        "SELECT EXISTS(SELECT 1 FROM runtime.execution_instances WHERE agent_run_id=$1 AND allocation_execution_id=$2 AND execution_id<>allocation_execution_id AND instance_id=$3 AND environment_id=$4 AND host_id=$5)",
        &[current, &allocation.allocation_execution_id, &allocation.instance_id, &allocation.environment_id, &allocation.host_id],
    ).map_err(|e| e.to_string())?.get(0);
    if !associated {
        return Ok(false);
    }
    let power = db.query_opt(
        "SELECT state FROM runtime.runsc_environment_power WHERE environment_id=$1 AND host_id=$2 AND allocation_execution_id=$3 AND instance_id=$4 AND container_id=$5 FOR UPDATE",
        &[&allocation.environment_id, &allocation.host_id, &allocation.allocation_execution_id, &allocation.instance_id, &allocation.container_id],
    ).map_err(|e| e.to_string())?;
    if power.is_none_or(|row| row.get::<_, String>(0) != "resumePrepared") {
        return Ok(false);
    }
    let admitted: bool = db.query_one(
        "SELECT EXISTS(SELECT 1 FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND NOT session_level)",
        &[current],
    ).map_err(|e| e.to_string())?.get(0);
    if admitted {
        return Ok(false);
    }
    let failed = super::pre_admission_failure::has_pre_admission_failure(
        db,
        &request.cleanup.authorization.workspace_id,
        &request.cleanup.authorization.session_id,
        current,
        &request.cleanup.authorization_digest,
    )?;
    let unexplained: bool = db.query_one(
        "SELECT EXISTS(SELECT 1 FROM runtime.runtime_events WHERE task_id=$1 AND NOT($2 AND event_id=$3 AND event_type=$4 AND session_id=$5))",
        &[current, &failed, &super::pre_admission_failure::event_id(current),
          &super::pre_admission_failure::SCHEMA, &request.cleanup.authorization.session_id],
    ).map_err(|e| e.to_string())?.get(0);
    Ok(!unexplained)
}

fn require_physical_replay(
    db: &mut impl GenericClient,
    r: &CleanupAuthorityRequest<'_>,
    receipt: &CleanupReceipt,
) -> Result<(), String> {
    if r.cleanup.environment_authorization.payload.execution_mode == "perRun"
        && matches!(receipt.allocation, CleanupAllocation::Instance { .. })
    {
        let removed=db.query_one("SELECT physical_removed FROM runtime.agent_run_cleanup_receipts WHERE receipt_id=$1",&[&receipt.receipt_id]).map_err(|e|e.to_string())?.get::<_,bool>(0);
        if !removed {
            let retired=db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.environment_retirements WHERE environment_id=$1 AND instance_id=$2 AND state='destroyed')",&[&receipt.environment_id,&match &receipt.allocation{CleanupAllocation::Instance{instance_id}=>Some(instance_id),_=>None}]).map_err(|e|e.to_string())?.get::<_,bool>(0);
            let unmaterialized = match &receipt.allocation {
                CleanupAllocation::Instance { instance_id } => {
                    let row=db.query_opt("SELECT allocation_execution_id FROM runtime.execution_instances WHERE agent_run_id=$1 AND instance_id=$2 AND environment_id=$3",&[&receipt.agent_run_id,instance_id,&receipt.environment_id]).map_err(|e|e.to_string())?;
                    if let Some(row) = row {
                        super::creation_attempts::not_materialized(
                            db,
                            &row.get::<_, String>(0),
                            instance_id,
                        )?
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if !retired && !unmaterialized {
                return Err("cleanup_physical_release_unconfirmed".into());
            }
        }
    }
    Ok(())
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
    let unknown=db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.resident_sandboxes WHERE host_id=$1 AND (allocation_mode='unbound' OR environment_id IS NULL OR (environment_id=$2 AND ((state<>'resident' AND NOT($3 AND state='releasing')) OR container_id IS NULL OR instance_id IS NULL))))", &[&r.host_id,&b.environment_id,&(b.execution_mode=="perRun")]).map_err(|e|e.to_string())?.get::<_,bool>(0);
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
    if let Some(execution) = anchors.first() {
        let row = db
            .query_one(
                "SELECT instance_id FROM runtime.execution_instances WHERE execution_id=$1",
                &[execution],
            )
            .map_err(|e| e.to_string())?;
        let instance_id: String = row.get(0);
        if super::creation_attempts::not_materialized(db, execution, &instance_id)? {
            if physical.iter().any(|p| &p.execution_id == execution) {
                return Err("cleanup_non_dispatch_evidence_conflict".into());
            }
            return Ok(CleanupTarget::Unmaterialized { instance_id });
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
    let row=db.query_opt("SELECT i.execution_id,i.agent_run_id,i.instance_id,i.environment_id,i.host_id,i.image_digest,i.authorization_digest,i.binding_digest,i.profile_digest,s.container_id,s.workspace_id,s.memory_bytes,s.cpu_milli,s.pids,s.workspace_bytes,s.state,i.allocation_owner_json FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id WHERE i.execution_id=$1 AND i.allocation_execution_id=i.execution_id AND i.allocation_mode='retained' FOR UPDATE OF s", &[&execution]).map_err(|e|e.to_string())?.ok_or("cleanup allocation anchor missing")?;
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
        allocation_owner: serde_json::from_str(&row.get::<_, String>(16))
            .map_err(|_| "allocation_owner_invalid")?,
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
        || !(row.get::<_, String>(15) == "resident"
            || (b.execution_mode == "perRun" && row.get::<_, String>(15) == "releasing"))
        || !(physical.iter().any(|p| {
            p.execution_id == execution
                && p.allocation_owner == allocation.allocation_owner
                && Some(&p.container_id) == allocation.container_id.as_ref()
                && p.resources == resources
        }) || (b.execution_mode == "perRun" && row.get::<_, String>(15) == "releasing"))
    {
        return Err("cleanup live environment unconfirmed".into());
    }
    // The container may have been created by a previous Run. Its fixed profile
    // is described only by that immutable signed origin grant, never today's
    // cleanup controller's potentially different plugins or resource request.
    let origin_authorization = super::allocation_origin::load(db, &allocation, r.signing_key)?;
    if origin_authorization.workspace_id != r.cleanup.authorization.workspace_id
        || origin_authorization.session_id != b.session_id
        || origin_authorization.agent_id != r.cleanup.authorization.agent_id
        || origin_authorization.user_id != r.cleanup.authorization.user_id
        || crate::resident_capacity::declared_usage(
            origin_authorization
                .environment_profile
                .resources
                .sandbox_resources(),
        ) != resources
    {
        return Err("cleanup_origin_binding_rejected".into());
    }
    // A queued Run that never acquired an execution association has no task to
    // close in an older paused environment. Keep the original physical holding,
    // signed origin and pause acknowledgement intact; only this Run is settled.
    if associations.is_empty()
        && db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.runsc_environment_power WHERE environment_id=$1 AND host_id=$2 AND allocation_execution_id=$3 AND instance_id=$4 AND container_id=$5 AND state='paused')", &[&allocation.environment_id,&allocation.host_id,&allocation.allocation_execution_id,&allocation.instance_id,&allocation.container_id]).map_err(|e|e.to_string())?.get::<_,bool>(0)
    {
        return Ok(CleanupTarget::NeverAllocated);
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

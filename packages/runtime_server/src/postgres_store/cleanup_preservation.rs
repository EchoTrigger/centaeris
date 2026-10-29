//! Durable file preservation authorizes only recovery of one original allocation.
use super::cleanup_receipts::{CleanupAuthorityRequest, CleanupTarget};
use super::execution_instances::RetainedInstanceAllocation;
use crate::agent_run_authorization::SessionWorkspace;
use postgres::GenericClient;

#[derive(Clone, Debug)]
pub(crate) struct CleanupRecovery {
    pub expected_allocation: RetainedInstanceAllocation,
    pub evidence: CleanupRecoveryEvidence,
}
#[derive(Clone, Debug)]
pub(crate) enum CleanupRecoveryEvidence {
    Preserved {
        receipt_id: String,
        workspace: SessionWorkspace,
    },
    Absent,
}

fn same_allocation(left: &RetainedInstanceAllocation, right: &RetainedInstanceAllocation) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    // Reuse describes a caller's preparation result, not immutable provenance.
    left.reused = false;
    right.reused = false;
    left == right
}
fn storage_key(
    workspace_id: &str,
    session_id: &str,
    workspace: &SessionWorkspace,
) -> Result<String, String> {
    workspace
        .validate()
        .map_err(|_| "cleanup_preservation_descriptor_rejected")?;
    if workspace.snapshot_size_bytes == 0 {
        return Ok(String::new());
    }
    let hash = workspace
        .snapshot_sha256
        .strip_prefix("sha256:")
        .ok_or("cleanup_preservation_descriptor_rejected")?;
    Ok(format!(
        "workspaces/{workspace_id}/sessions/{session_id}/snapshots/{}/{hash}.snapshot",
        workspace.generation
    ))
}
fn receipt_id_valid(id: &str) -> bool {
    id.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Called with today's verified cleanup authority and original resident pins.
/// Creator lease/epoch remain immutable audit facts; they grant no current power.
pub(super) fn verify_recovery(
    db: &mut impl GenericClient,
    request: &CleanupAuthorityRequest<'_>,
    target: &CleanupTarget,
    recovery: &CleanupRecovery,
) -> Result<(), String> {
    let CleanupTarget::Instance { allocation, .. } = target else {
        return Err("cleanup_recovery_target_rejected".into());
    };
    if request
        .cleanup
        .environment_authorization
        .payload
        .execution_mode
        != "perRun"
        || !same_allocation(allocation, &recovery.expected_allocation)
    {
        return Err("cleanup_recovery_target_rejected".into());
    }
    let CleanupRecoveryEvidence::Preserved {
        receipt_id,
        workspace,
    } = &recovery.evidence
    else {
        // The physical callback must independently observe exact-ID absence.
        return Ok(());
    };
    if !receipt_id_valid(receipt_id)
        || Some(workspace.generation)
            != request
                .cleanup
                .authorization
                .session_workspace
                .generation
                .checked_add(1)
    {
        return Err("cleanup_preservation_receipt_rejected".into());
    }
    workspace
        .validate()
        .map_err(|_| "cleanup_preservation_receipt_rejected")?;
    let row=db.query_opt("SELECT session_id,agent_run_id,origin_agent_run_id,environment_id,authorization_digest,origin_authorization_digest,allocation_execution_id,instance_id,container_id,profile_digest,baseline::text,workspace::text,snapshot_encoding,storage_key FROM public.app_core_sessionworkspacepreservation WHERE id=$1 FOR SHARE",&[receipt_id])
        .map_err(|_|"cleanup_preservation_receipt_unavailable")?.ok_or("cleanup_preservation_receipt_missing")?;
    let cleanup = request.cleanup;
    let baseline: SessionWorkspace = serde_json::from_str(&row.get::<_, String>(10))
        .map_err(|_| "cleanup_preservation_receipt_rejected")?;
    let recorded: SessionWorkspace = serde_json::from_str(&row.get::<_, String>(11))
        .map_err(|_| "cleanup_preservation_receipt_rejected")?;
    if row.get::<_, String>(0) != cleanup.authorization.session_id
        || row.get::<_, String>(1) != cleanup.agent_run_id
        || Some(row.get::<_, String>(2).as_str()) != allocation.allocation_agent_run_id.as_deref()
        || row.get::<_, String>(3) != allocation.environment_id
        || row.get::<_, String>(4) != cleanup.authorization_digest
        || row.get::<_, String>(5) != allocation.authorization_digest
        || row.get::<_, String>(6) != allocation.allocation_execution_id
        || row.get::<_, String>(7) != allocation.instance_id
        || Some(row.get::<_, String>(8).as_str()) != allocation.container_id.as_deref()
        || row.get::<_, String>(9) != allocation.profile_digest
        || baseline != cleanup.authorization.session_workspace
        || &recorded != workspace
        || row.get::<_, String>(12) != "frameV1"
        || row.get::<_, String>(13)
            != storage_key(
                &cleanup.authorization.workspace_id,
                &cleanup.authorization.session_id,
                workspace,
            )?
    {
        return Err("cleanup_preservation_receipt_rejected".into());
    }
    let row=db.query_opt(r#"SELECT "workspaceBaselineKnown","workspaceGeneration"::bigint,"workspaceSnapshotSha256","workspaceSnapshotSizeBytes"::bigint,"workspaceExpandedSizeBytes"::bigint,"workspaceFileCount"::bigint,"workspaceSnapshotEncoding","workspaceStorageKey" FROM public.app_core_session WHERE id=$1 FOR SHARE"#,&[&cleanup.authorization.session_id])
        .map_err(|_|"cleanup_preservation_head_unavailable")?.ok_or("cleanup_preservation_head_missing")?;
    let head = SessionWorkspace {
        generation: row
            .get::<_, i64>(1)
            .try_into()
            .map_err(|_| "cleanup_preservation_head_rejected")?,
        snapshot_sha256: row.get(2),
        snapshot_size_bytes: row
            .get::<_, i64>(3)
            .try_into()
            .map_err(|_| "cleanup_preservation_head_rejected")?,
        expanded_size_bytes: row
            .get::<_, i64>(4)
            .try_into()
            .map_err(|_| "cleanup_preservation_head_rejected")?,
        file_count: row
            .get::<_, i64>(5)
            .try_into()
            .map_err(|_| "cleanup_preservation_head_rejected")?,
    };
    if !row.get::<_, bool>(0)
        || head.generation < workspace.generation
        || (head.generation == workspace.generation && &head != workspace)
        || row.get::<_, String>(6) != "frameV1"
        || row.get::<_, String>(7)
            != storage_key(
                &cleanup.authorization.workspace_id,
                &cleanup.authorization.session_id,
                &head,
            )?
    {
        return Err("cleanup_preservation_head_rejected".into());
    }
    Ok(())
}

pub(super) fn verify_completed_recovery(
    db: &mut impl GenericClient,
    receipt: &super::cleanup_receipts::CleanupReceipt,
    recovery: &CleanupRecovery,
) -> Result<(), String> {
    let allocation = &recovery.expected_allocation;
    if receipt.environment_id != allocation.environment_id
        || !matches!(&receipt.allocation,super::cleanup_receipts::CleanupAllocation::Instance {instance_id} if instance_id==&allocation.instance_id)
    {
        return Err("cleanup_recovery_target_rejected".into());
    }
    let row=db.query_one("SELECT physical_removed,removed_container_id FROM runtime.agent_run_cleanup_receipts WHERE receipt_id=$1",&[&receipt.receipt_id]).map_err(|e|e.to_string())?;
    if !row.get::<_, bool>(0) || row.get::<_, Option<String>>(1) != allocation.container_id {
        return Err("cleanup_recovery_target_rejected".into());
    }
    Ok(())
}

"""Read-only eligibility for known, never-admitted historical environments.

The released product path committed its creation intent and dispatch CAS before
sending physical creation. Absence from that mandatory ledger, combined with a
bound failure receipt, is a no-effects proof. This grants no execution, cleanup,
lease, release, or budget-refund authority.
"""

import json

from django.conf import settings
from django.db import DatabaseError, connection, transaction

from .runtime_contract import (
    AGENT_RUN_AUTHORIZATION_FIELDS,
    AGENT_RUN_AUTHORIZATION_SCHEMA,
    _validated_authorization_digest,
    _verify_authorization_digest_signature,
)

# All facts and absence predicates share ONE PostgreSQL statement snapshot.
# Runtime separately validates the released schema and physical responsibilities.
PROOF_SQL = """
SELECT jsonb_build_object(
 'environment',to_jsonb(e),'session',to_jsonb(s),'run',to_jsonb(r),
 'grant',to_jsonb(g),'job',to_jsonb(j),'failure',to_jsonb(f),
 'agentOwner',a.owner_id::text,'agentWorkspace',a.workspace_id,
 'versions',(SELECT jsonb_agg(version ORDER BY version) FROM runtime.schema_migrations),
 'nowMs',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,
 'coreHistory',EXISTS(SELECT 1 FROM public.app_core_sessionevent se
   JOIN public.app_core_agentrun other ON other.id=se.agent_run_id
   WHERE other.environment_id=e.id AND NOT se.session_level),
 'activeJobs',EXISTS(SELECT 1 FROM runtime.runtime_jobs other
   JOIN public.app_core_agentrun ar ON other.payload_ref='record:agent_run:'||ar.id
   WHERE ar.environment_id=e.id AND other.status IN('queued','leased','running')),
 'activeRuns',EXISTS(SELECT 1 FROM public.app_core_agentrun other
   WHERE other.environment_id=e.id AND other.status IN('queued','running')),
 'effects',(
   s."workspaceGeneration"<>0 OR s."workspaceStorageKey"<>'' OR s."workspaceSnapshotSha256"<>''
   OR s."workspaceLastAdvancedAgentRun_id" IS NOT NULL
   OR
   EXISTS(SELECT 1 FROM runtime.provider_creation_operations p
     WHERE p.environment_id=e.id OR p.agent_run_id=r.id OR p.processing_job_id=j.job_id)
   OR EXISTS(SELECT 1 FROM runtime.provider_run_cleanup_receipts p WHERE p.environment_id=e.id OR p.agent_run_id=r.id)
   OR EXISTS(SELECT 1 FROM runtime.provider_environment_bindings p WHERE p.environment_id=e.id)
   OR EXISTS(SELECT 1 FROM runtime.provider_environment_run_uses p WHERE p.environment_id=e.id OR p.agent_run_id=r.id)
   OR EXISTS(SELECT 1 FROM runtime.provider_environment_power_operations p WHERE p.environment_id=e.id)
   OR EXISTS(SELECT 1 FROM runtime.provider_environment_retirements p WHERE p.environment_id=e.id)
   OR EXISTS(SELECT 1 FROM runtime.provider_environment_provision_completions p WHERE p.environment_id=e.id)
   OR EXISTS(SELECT 1 FROM runtime.provider_environment_maintenance_completions p WHERE p.environment_id=e.id)
   OR EXISTS(SELECT 1 FROM runtime.execution_instances i WHERE i.environment_id=e.id OR i.agent_run_id=r.id)
   OR EXISTS(SELECT 1 FROM runtime.resident_sandboxes h WHERE h.environment_id=e.id OR h.agent_run_id=r.id)
   OR EXISTS(SELECT 1 FROM runtime.environment_creation_attempts c LEFT JOIN runtime.execution_instances i
     ON i.execution_id=c.allocation_execution_id
     WHERE i.environment_id=e.id OR i.agent_run_id=r.id OR i.execution_id IS NULL)
   OR EXISTS(SELECT 1 FROM runtime.environment_retirements p WHERE p.environment_id=e.id)
   OR EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts p WHERE p.environment_id=e.id OR p.agent_run_id=r.id)
   OR EXISTS(SELECT 1 FROM runtime.resource_claims c WHERE c.session_id=s.id)
   OR EXISTS(SELECT 1 FROM public.app_core_workspacesnapshotpublication p WHERE p.session_id=s.id OR p.agent_run_id=r.id)
   OR EXISTS(SELECT 1 FROM public.app_core_environmentmaintenanceoperation p WHERE p.environment_id=e.id)
 ))
FROM public.app_core_sessionenvironment e
JOIN public.app_core_session s ON s.id=e.session_id
JOIN public.app_core_agentrun r ON r.id=e.controller_run_id AND r.environment_id=e.id
JOIN public.app_core_agent a ON a.id=s.agent_id
JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id
JOIN runtime.runtime_jobs j ON j.job_id='agent_run.lifecycle:'||r.id
JOIN runtime.runtime_events f ON f.event_id='agent_run_pre_admission_failure:'||r.id
WHERE e.id=%s
"""


def archived_preallocation_is_unused(environment_id):
    """Fail closed; preserve every old row, including an unreleased controller."""
    if connection.vendor != "postgresql":
        return False
    try:
        # Isolate a missing/unsupported ledger error from a caller-owned
        # transaction. The proof still contains exactly one data-read statement.
        with transaction.atomic():
            with connection.cursor() as cursor:
                cursor.execute(PROOF_SQL, [environment_id])
                row = cursor.fetchone()
        if row is None:
            return False
        proof = json.loads(row[0]) if isinstance(row[0], str) else row[0]
        return _bound_unused_proof(proof, environment_id)
    except (DatabaseError, KeyError, TypeError, ValueError, OverflowError):
        # Do not log the original signed payload or credential configuration.
        return False


def _bound_unused_proof(proof, environment_id):
    if proof["versions"] not in (list(range(1, 25)), list(range(1, 26))):
        return False
    if any(
        proof[field] is not False
        for field in ("coreHistory", "activeJobs", "activeRuns", "effects")
    ):
        return False
    e, s, r, g, j, f = (
        proof[field]
        for field in ("environment", "session", "run", "grant", "job", "failure")
    )
    profile, payload = e["environment_profile"], g["payload"]
    if (
        not _known_archived_profile_shape(profile)
        or not isinstance(payload, dict)
        or set(payload) != AGENT_RUN_AUTHORIZATION_FIELDS
        or payload["schema"] != AGENT_RUN_AUTHORIZATION_SCHEMA
    ):
        return False
    digest = _validated_authorization_digest(payload)
    _verify_authorization_digest_signature(
        digest,
        settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
        g["signature"],
        schema=payload["schema"],
    )
    if (
        digest != g["digest"]
        or payload["environmentProfile"] != profile
        or payload["environmentProfileDigest"] != e["environment_profile_digest"]
        or _validated_authorization_digest(profile) != e["environment_profile_digest"]
        or profile.get("network") != e["network"]
    ):
        return False
    bindings = {
        "id": g["id"],
        "workspaceId": r["workspace_id"],
        "userId": str(r["user_id"]),
        "agentId": s["agent_id"],
        "sessionId": r["session_id"],
        "agentRunId": r["id"],
        "modelConfigRef": r["modelConfig_id"],
        "thinkingMode": r["thinkingMode"] or None,
        "artifactScopeRef": "artifact_scope_" + r["id"],
    }
    empty_workspace = {
        "generation": 0,
        "snapshotSha256": "",
        "snapshotSizeBytes": 0,
        "expandedSizeBytes": 0,
        "fileCount": 0,
    }
    if payload["sessionWorkspace"] != empty_workspace:
        return False
    if any(payload[field] != value for field, value in bindings.items()):
        return False
    if (
        e["id"] != environment_id
        or e["session_id"] != s["id"]
        or r["environment_id"] != e["id"]
        or r["session_id"] != s["id"]
        or r["workspace_id"] != s["workspace_id"]
        or str(s["owner_id"]) != str(r["user_id"])
        or proof["agentOwner"] != str(r["user_id"])
        or proof["agentWorkspace"] != r["workspace_id"]
        or g["agent_run_id"] != r["id"]
        or r["status"] != "failed"
        or r["preAdmissionCancelledAt"] is not None
        or e["controller_run_id"] != r["id"]
        or e["controller_maintenance_operation_id"] is not None
        or e["controller_released"] is not False
        or e["controller_purpose"] != "cleanup"
        or type(e["controller_epoch"]) is not int
        or e["controller_epoch"] <= 0
        or not isinstance(e["controller_lease_owner"], str)
        or not e["controller_lease_owner"].strip()
    ):
        return False
    job_id = "agent_run.lifecycle:" + r["id"]
    if (
        j["job_id"] != job_id
        or j["job_kind"] != "agent_run.lifecycle"
        or j["status"] != "dead_lettered"
        or j["session_id"] != s["id"]
        or j["idempotency_key"] != job_id + ":" + digest
        or j["payload_ref"] != "record:agent_run:" + r["id"]
    ):
        return False
    receipt = json.loads(f["payload_json"])
    if not isinstance(receipt, dict) or type(receipt.get("failedAtMs")) is not int:
        return False
    owner = receipt.get("leaseOwner")
    if (
        not isinstance(owner, str)
        or not owner.strip()
        or type(f["at_ms"]) is not int
        or not 0 < f["at_ms"] <= min(proof["nowMs"], 253402300799999)
    ):
        return False
    expected = {
        "schema": "runtime.agent_run.pre_admission_failure.v1",
        "workspaceId": r["workspace_id"],
        "sessionId": s["id"],
        "agentRunId": r["id"],
        "authorizationDigest": digest,
        "jobId": job_id,
        "leaseOwner": owner,
        "failedAtMs": f["at_ms"],
    }
    return (
        receipt == expected
        and f["event_id"] == "agent_run_pre_admission_failure:" + r["id"]
        and f["session_id"] == s["id"]
        and f["task_id"] == r["id"]
        and f["event_type"] == expected["schema"]
        and f["visibility"] == "internal"
    )


def _known_archived_profile_shape(profile):
    # Released v1 record structure only. This does not parse an executable
    # provider configuration or grant any native operation on this profile.
    required = {
        "schema",
        "provider",
        "providerConfigDigest",
        "templateRef",
        "workspaceRoot",
        "user",
        "network",
        "lifetime",
        "timeoutSeconds",
        "inputBudgetBytes",
    }
    if (
        not isinstance(profile, dict)
        or set(profile) not in (required, required | {"workspaceInitialization"})
        or profile["schema"] != "runtime.e2b_environment_profile.v1"
        or profile["provider"] != "e2b"
        or not isinstance(profile["templateRef"], dict)
        or set(profile["templateRef"]) != {"templateId", "buildId"}
        or any(not isinstance(value, str) for value in profile["templateRef"].values())
        or any(
            not isinstance(profile[field], str)
            for field in (
                "providerConfigDigest",
                "workspaceRoot",
                "user",
                "network",
                "lifetime",
            )
        )
        or any(
            type(profile[field]) is not int
            for field in ("timeoutSeconds", "inputBudgetBytes")
        )
    ):
        return False
    return "workspaceInitialization" not in profile or profile[
        "workspaceInitialization"
    ] in (
        "managedFiles",
        "taskImage",
    )

"""Business controller authority, never physical instance liveness.

Lock order is Workspace → original membership → Session → Environment → Run →
original grant → lifecycle job. The final job lock uses FOR SHARE NOWAIT to avoid
waiting on a job holder that may itself be waiting for Session.
The job lock only pins existing authority; this module never creates a lease.
Callers must not enter with partial locks acquired in a different order.
Public authority operations require an outermost transaction: they return only
after its durable commit and reject invocation inside a caller-owned transaction.
"""
import hashlib

from django.conf import settings
from django.db import DatabaseError, connection, transaction

from .models import AgentRun, AgentRunAuthorization, AgentRunEnvironmentRelease, Session, SessionEnvironment, Workspace, WorkspaceMembership
from .runtime_contract import agent_run_binding_matches, authorization_digest, verify_agent_run_authorization_signature
from .resource_commit import require_durable_commit

MAX_CONTROLLER_EPOCH = 2**63 - 1


class EnvironmentControllerBusy(ValueError):
    """Authority is valid but a concurrent operation requires a retry."""


class EnvironmentControllerTerminal(ValueError):
    """Core terminal committed while execute authority was being acquired."""


def controller_lease_digest(lease_owner):
    if not isinstance(lease_owner, str) or not 1 <= len(lease_owner) <= 160:
        raise ValueError("environment_controller_lease_invalid")
    return "sha256:" + hashlib.sha256(b"workspace:environment-controller-lease:v1\0"
        + lease_owner.encode("utf-8")).hexdigest()


def _authority(environment):
    return {"environmentId": environment.pk, "controllerId": environment.controller_run_id,
        "controllerEpoch": environment.controller_epoch,
        "controllerLeaseDigest": controller_lease_digest(environment.controller_lease_owner),
        "controllerPurpose": environment.controller_purpose}


def _locked_binding(run_id, lease_owner, *, acquiring):
    controller_lease_digest(lease_owner)
    initial = AgentRun.objects.only("session_id", "environment_id", "workspace_id", "membership_ref", "user_id").get(pk=run_id)
    workspace = Workspace.objects.select_for_update().get(pk=initial.workspace_id)
    member = WorkspaceMembership.objects.select_for_update().filter(pk=initial.membership_ref,
        workspace_id=initial.workspace_id, user_id=initial.user_id, role__in=("owner", "admin", "member")).first()
    session = Session.objects.select_for_update(of=("self",)).get(pk=initial.session_id)
    environment = SessionEnvironment.objects.select_for_update().get(pk=initial.environment_id)
    run = AgentRun.objects.select_for_update(of=("self",)).select_related("session__agent").get(pk=run_id)
    grant = AgentRunAuthorization.objects.select_for_update().get(agent_run_id=run_id)
    if (run.session_id != session.pk or run.environment_id != environment.pk
            or environment.session_id != session.pk or run.workspace_id != workspace.pk):
        raise ValueError("environment_controller_binding_invalid")
    if acquiring and (member is None or workspace.status != "active"
            or session.current_environment_id != environment.pk or session.deletedAt is not None):
        raise ValueError("environment_controller_membership_invalid")
    if (authorization_digest(grant.payload) != grant.digest or not agent_run_binding_matches(grant.payload, run)
            or grant.payload["thinkingMode"] != (run.thinkingMode or None)):
        raise ValueError("environment_controller_grant_invalid")
    verify_agent_run_authorization_signature(grant.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, grant.signature)
    job = f"agent_run.lifecycle:{run.pk}"
    with connection.cursor() as cursor:
        cursor.execute("SELECT job_id FROM runtime.runtime_jobs WHERE job_id=%s "
            "AND job_kind='agent_run.lifecycle' AND status IN ('leased','running') "
            "AND lease_owner=%s AND session_id=%s AND idempotency_key=%s AND payload_ref=%s "
            "AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FOR SHARE NOWAIT",
            [job, lease_owner, run.session_id, f"{job}:{grant.digest}", f"record:agent_run:{run.pk}"])
        if cursor.fetchone() is None:
            raise ValueError("environment_controller_lease_rejected")
    return run, environment


def _translate_contention(operation):
    if connection.in_atomic_block:
        raise RuntimeError("environment_controller_requires_outermost_transaction")
    try:
        with transaction.atomic(durable=True):
            require_durable_commit(connection, ValueError("environment_controller_durability_required"))
            return operation()
    except DatabaseError as error:
        if getattr(error.__cause__, "sqlstate", None) == "55P03":
            raise EnvironmentControllerBusy("environment_controller_busy") from error
        raise


def _acquire_controller(run_id, lease_owner, purpose):
    def acquire():
        run, environment = _locked_binding(run_id, lease_owner, acquiring=purpose == "execute")
        if purpose == "execute":
            from .session_event import committed_session_terminal_state
            if committed_session_terminal_state(run) is not None:
                raise EnvironmentControllerTerminal("environment_controller_core_terminal")
        if _stored_release(run) is not None:
            raise ValueError("environment_controller_already_released")
        allowed = {"queued", "running"} if purpose == "execute" else {"completed", "failed", "cancelled"}
        if run.status not in allowed:
            raise ValueError("environment_controller_run_terminal")
        if purpose == "cleanup" and environment.controller_run_id == run.pk and environment.controller_released:
            raise ValueError("environment_release_history_missing")
        if not environment.controller_released and environment.controller_maintenance_operation_id is not None:
            raise ValueError("environment_controller_owned")
        if not environment.controller_released and environment.controller_run_id not in (None, run.pk):
            raise ValueError("environment_controller_owned")
        if (not environment.controller_released and environment.controller_run_id == run.pk
                and environment.controller_lease_owner == lease_owner
                and environment.controller_purpose == purpose):
            return _authority(environment)
        if environment.controller_epoch >= MAX_CONTROLLER_EPOCH:
            raise ValueError("environment_controller_epoch_exhausted")
        environment.controller_epoch += 1
        environment.controller_run = run
        environment.controller_maintenance_operation = None
        environment.controller_lease_owner = lease_owner
        environment.controller_released = False
        environment.controller_purpose = purpose
        environment.save(update_fields=["controller_epoch", "controller_run", "controller_maintenance_operation",
            "controller_lease_owner", "controller_released", "controller_purpose"])
        return _authority(environment)
    return _translate_contention(acquire)


def acquire_environment_controller(run_id, lease_owner):
    return _acquire_controller(run_id, lease_owner, "execute")


def acquire_environment_cleanup_controller(run_id, lease_owner):
    """Issue terminal cleanup authority; this cannot authorize command execution."""
    return _acquire_controller(run_id, lease_owner, "cleanup")


def _stored_release(run):
    receipt = AgentRunEnvironmentRelease.objects.filter(agent_run_id=run.pk).first()
    if receipt is not None and (receipt.environment_id != run.environment_id
            or receipt.authorization_digest != run.authorization.digest):
        raise ValueError("environment_release_binding_invalid")
    return receipt


def _release_result(receipt):
    return {"environmentId": receipt.environment_id, "controllerId": receipt.agent_run_id,
        "controllerEpoch": receipt.controller_epoch, "controllerLeaseDigest": receipt.controller_lease_digest,
        "controllerPurpose": "cleanup", "released": True, "releaseReceiptId": receipt.pk}


def get_environment_release_receipt(run_id, lease_owner):
    """Read only the authenticated job's release history without acquiring control."""
    def read():
        run, _ = _locked_binding(run_id, lease_owner, acquiring=False)
        receipt = _stored_release(run)
        return None if receipt is None else _release_result(receipt)
    return _translate_contention(read)


def release_environment_controller(run_id, controller_epoch, lease_owner):
    if type(controller_epoch) is not int or not 1 <= controller_epoch <= MAX_CONTROLLER_EPOCH:
        raise ValueError("environment_controller_epoch_invalid")
    def release():
        run, environment = _locked_binding(run_id, lease_owner, acquiring=False)
        receipt = _stored_release(run)
        if receipt is not None:
            if receipt.controller_epoch != controller_epoch:
                raise ValueError("environment_controller_release_rejected")
            return _release_result(receipt)
        if (run.status not in {"completed", "failed", "cancelled"}
                or environment.controller_run_id != run.pk
                or environment.controller_lease_owner != lease_owner
                or environment.controller_epoch != controller_epoch
                or environment.controller_purpose != "cleanup"):
            raise ValueError("environment_controller_release_rejected")
        environment.controller_released = True
        environment.save(update_fields=["controller_released"])
        receipt = AgentRunEnvironmentRelease.objects.create(agent_run=run, environment=environment,
            authorization_digest=run.authorization.digest, controller_epoch=controller_epoch,
            controller_lease_digest=controller_lease_digest(lease_owner))
        return _release_result(receipt)
    return _translate_contention(release)

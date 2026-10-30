"""Rebind drained Sessions to the workspace budget contract without editing grants.

Application writers and workers must be stopped before upgrading. Existing
Environment rows, physical-origin records and signed grants remain immutable.
"""
import copy
import hashlib
from importlib import import_module
import json

from django.db import migrations
from django.utils import timezone


TERMINAL_RUNS = ("completed", "failed", "cancelled")


def _digest(value):
    return "sha256:" + hashlib.sha256(json.dumps(value, ensure_ascii=False,
        sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()


def _require_drained(apps, schema_editor, environment):
    alias = schema_editor.connection.alias
    runs = apps.get_model("app_core", "AgentRun").objects.using(alias)
    maintenance = apps.get_model("app_core", "EnvironmentMaintenanceOperation").objects.using(alias)
    if (not environment.controller_released
            or runs.filter(environment_id=environment.pk).exclude(status__in=TERMINAL_RUNS).exists()
            or maintenance.filter(environment_id=environment.pk, state="active").exists()):
        raise RuntimeError("workspace_resource_migration_requires_drained_environment")
    if schema_editor.connection.vendor != "postgresql":
        return  # SQLite validates relational migration structure, not Runtime authority.
    with schema_editor.connection.cursor() as cursor:
        cursor.execute("SELECT EXISTS(SELECT 1 FROM runtime.runtime_jobs j "
            "JOIN public.app_core_agentrun r ON j.job_id='agent_run.lifecycle:'||r.id "
            "WHERE r.environment_id=%s AND j.status NOT IN('succeeded','failed','dead_lettered','cancelled')) "
            "OR EXISTS(SELECT 1 FROM runtime.resident_sandboxes WHERE environment_id=%s) "
            "OR EXISTS(SELECT 1 FROM runtime.environment_creation_attempts c "
            "LEFT JOIN runtime.execution_instances i ON i.execution_id=c.allocation_execution_id "
            "AND i.instance_id=c.instance_id "
            "WHERE c.state IN('prepared','dispatched') AND (i.execution_id IS NULL OR (i.environment_id=%s "
            "AND NOT EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts p "
            "WHERE p.instance_id=i.instance_id AND p.environment_id=i.environment_id "
            "AND p.cleanup_complete AND p.physical_removed) "
            "AND NOT EXISTS(SELECT 1 FROM runtime.environment_retirements t "
            "WHERE t.instance_id=i.instance_id AND t.environment_id=i.environment_id AND t.state='destroyed'))))",
            [environment.pk, environment.pk, environment.pk])
        if cursor.fetchone()[0]:
            raise RuntimeError("workspace_resource_migration_requires_drained_environment")


def rebind_workspace_resources(apps, schema_editor):
    alias = schema_editor.connection.alias
    sessions = apps.get_model("app_core", "Session").objects.using(alias)
    environments = apps.get_model("app_core", "SessionEnvironment").objects.using(alias)
    validate_original = import_module("app_core.migrations.0023_unified_environment_profile").validate_historical_profile
    for session in sessions.filter(current_environment__environment_profile__isnull=False).select_related(
            "current_environment").iterator():
        original = session.current_environment
        profile = original.environment_profile
        validate_original(profile)
        if _digest(profile) != original.environment_profile_digest:
            raise RuntimeError("workspace_resource_migration_profile_digest_invalid")
        _require_drained(apps, schema_editor, original)
        rebuilt = copy.deepcopy(profile)
        rebuilt["resources"]["workspaceBytes"] = rebuilt["resources"].pop("dataTmpfsBytes")
        replacement = environments.create(session_id=session.pk)
        environments.filter(pk=replacement.pk).update(environment_profile=rebuilt,
            environment_profile_digest=_digest(rebuilt), network=rebuilt["network"],
            environment_profile_operation_id="workspace_resource:" + original.pk,
            environment_profile_bound_at=timezone.now())
        sessions.filter(pk=session.pk).update(current_environment_id=replacement.pk)


def reject_reverse(apps, schema_editor):
    raise RuntimeError("workspace_resource_contract_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0034_workspace_snapshot_publication")]
    operations = [migrations.RunPython(rebind_workspace_resources, reject_reverse)]

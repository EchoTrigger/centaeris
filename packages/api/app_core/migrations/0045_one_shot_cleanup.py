"""Allow erasing settled one-shot execution records after their delivery closes."""
from importlib import import_module

from django.db import migrations


RECLAIMABLE = """
CREATE FUNCTION one_shot_session_cleanup_allowed(sid text) RETURNS boolean
LANGUAGE sql STABLE AS $$
 SELECT EXISTS (
  SELECT 1 FROM app_core_session s JOIN app_core_oneshotoperation o ON o."sessionId"=s.id
  JOIN app_core_agentrun r ON r.id=o."agentRunId" AND r.session_id=s.id
  JOIN app_core_agentrunauthorization a ON a.agent_run_id=r.id
  WHERE s.id=sid AND s.purpose='oneShot' AND o."cleanupStatus"='cleaning'
   AND (o."deliveryClosedAt" IS NOT NULL OR o."deliveryLeaseUntil"<=CURRENT_TIMESTAMP)
   AND a.payload->>'schema'='workspace.agent_run_authorization.v3'
   AND a.payload->'executionPolicy'->>'purpose'='oneShot'
   AND r.status IN ('completed','failed','cancelled')
   AND NOT EXISTS(SELECT 1 FROM app_core_agentrun live WHERE live.session_id=s.id
                  AND live.status NOT IN ('completed','failed','cancelled'))
   AND NOT EXISTS(SELECT 1 FROM app_core_sessionenvironment e WHERE e.session_id=s.id
                  AND NOT e.controller_released)
   AND (r."preAdmissionCancelledAt" IS NOT NULL OR EXISTS (
    SELECT 1 FROM app_core_agentrunenvironmentrelease p WHERE p.agent_run_id=r.id))
 );
$$;
"""


def install_cleanup_guards(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return
    profile = import_module("app_core.migrations.0023_unified_environment_profile").GUARDS
    profile = profile.split("CREATE TRIGGER", 1)[0].replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION", 1)
    profile = profile.replace(
        "    IF TG_OP='DELETE' THEN",
        "    IF TG_OP='DELETE' THEN\n"
        "        IF one_shot_session_cleanup_allowed(OLD.session_id) THEN RETURN OLD; END IF;",
        1,
    )
    release = """
CREATE OR REPLACE FUNCTION enforce_environment_release_immutable() RETURNS trigger AS $$
BEGIN
 IF TG_OP='DELETE' AND EXISTS(SELECT 1 FROM app_core_agentrun r
     WHERE r.id=OLD.agent_run_id AND one_shot_session_cleanup_allowed(r.session_id)) THEN
  RETURN OLD;
 END IF;
 RAISE EXCEPTION 'environment_release_immutable' USING ERRCODE='23514';
END;
$$ LANGUAGE plpgsql;
"""
    schema_editor.execute(RECLAIMABLE + profile + release, params=None)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0044_one_shot_runs")]
    operations = [migrations.RunPython(install_cleanup_guards)]

from django.db import migrations


GUARDS = """
CREATE FUNCTION agent_run_api_session_cleanup_started(sid text) RETURNS boolean
LANGUAGE sql STABLE AS $$
 SELECT agent_run_api_session_retired(sid) AND EXISTS (
  SELECT 1 FROM app_core_session s JOIN app_core_agentrunapioperation o ON o."sessionId"=s.id
  JOIN app_core_agentrun r ON r.id=o."agentRunId" AND r.session_id=s.id
  JOIN app_core_agentrunauthorization a ON a.agent_run_id=r.id
  WHERE s.id=sid AND s.purpose='agentRunApi' AND o."cleanupStatus"='cleaning'
   AND a.payload->>'schema'='workspace.agent_run_authorization.v3'
   AND a.payload->'executionPolicy'->>'purpose'='agentRunApi'
 );
$$;

CREATE OR REPLACE FUNCTION agent_run_api_session_cleanup_allowed(sid text) RETURNS boolean
LANGUAGE sql STABLE AS $$
 SELECT agent_run_api_session_cleanup_started(sid) AND agent_run_api_session_settled(sid);
$$;

CREATE OR REPLACE FUNCTION enforce_environment_release_immutable() RETURNS trigger AS $$
BEGIN
 IF TG_OP='DELETE' AND EXISTS (
  SELECT 1 FROM app_core_agentrun r WHERE r.id=OLD.agent_run_id
   AND agent_run_api_session_cleanup_started(r.session_id)
   AND NOT EXISTS (SELECT 1 FROM app_core_sessionenvironment e WHERE e.session_id=r.session_id)
   AND NOT EXISTS (SELECT 1 FROM app_core_agentrun live WHERE live.session_id=r.session_id
    AND live.status NOT IN ('completed','failed','cancelled'))
   AND NOT EXISTS (SELECT 1 FROM app_core_agentrunapioperation o WHERE o."sessionId"=r.session_id
    AND o."deliveryClosedAt" IS NULL
    AND (o."deliveryLeaseUntil" IS NULL OR o."deliveryLeaseUntil">CURRENT_TIMESTAMP))
 ) THEN
  RETURN OLD;
 END IF;
 RAISE EXCEPTION 'environment_release_immutable' USING ERRCODE='23514';
END;
$$ LANGUAGE plpgsql;
"""


def install_guards(apps, schema_editor):
    if schema_editor.connection.vendor == "postgresql":
        # Environment deletion proves that all releases still existed. Release
        # rows then use that fence because they disappear one at a time.
        schema_editor.execute(GUARDS, params=None)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0050_agent_api_conversations")]
    operations = [migrations.RunPython(install_guards)]

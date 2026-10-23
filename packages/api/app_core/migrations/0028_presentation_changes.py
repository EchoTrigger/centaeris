"""Persist scoped presentation dirtiness independently of transient wake hints.

SQLite is supported for schema-generation checks only. Live presentation readers
explicitly require PostgreSQL; Python signals cannot observe Runtime SQL commits.
"""
from django.db import migrations, models


# Field lists describe observable output or authority, not bookkeeping timestamps
# and leases. The trigger compares only these values before constructing tiny
# identity documents; it never copies model text into the change ledger or NOTIFY.
SOURCES = {
    "Session": ("session", ["id", "workspace", "owner", "agent", "project", "current_environment", "title", "origin", "status", "deletedAt", "purgedAt", "isPinned", "isUnread", "attentionRevision", "latestAttentionSequence", "readThroughSequence", "updatedAt"]),
    "SessionEvent": ("event", ["eventId", "workspace", "session", "agent_run", "sequence", "agent_run_sequence", "session_level", "projects_to_agent_run_stream", "payload"]),
    "AgentRun": ("run", ["id", "workspace", "session", "user", "status", "startedAt", "completedAt", "preAdmissionCancelledAt", "modelConfig", "transitionReason"]),
    "Agent": ("agent", ["id", "workspace", "owner", "definition", "name", "description", "avatar_kind", "status", "deletedAt", "purgedAt", "is_business_instance", "model_config", "thinking_mode"]),
    "AgentInput": ("input", ["id", "agent", "session", "input_id", "sequence", "accepted_source_sequence", "body", "attachments"]),
    "AgentInputQueue": ("queue", ["agent_run", "initial_input", "initial_host", "authorization_digest", "accepting", "closed_reason", "closed_at_ms"]),
    "AgentInputDelivery": ("delivery", ["input", "queue", "acknowledged_at_ms"]),
    "AgentRunAuthorization": ("authorization", ["agent_run", "digest", "signature", "payload"]),
    "AgentWorkSession": ("work", ["session", "coordination_session", "source_run", "source_event_id", "operation"]),
    "AgentWorkReturn": ("return", ["id", "work", "child_run", "fact_kind", "fact_ref", "payload"]),
    "AgentWorkConsumeAttempt": ("consume", ["id", "notice", "coordinator_run", "operation", "acknowledged_at_ms"]),
    "AgentCoordinationSession": ("coordination", ["agent", "session"]),
    "BusinessAgentBranch": ("branch", ["id", "app", "root_agent", "business_user_id", "agent", "session"]),
    "ModelConfig": ("model", ["id", "contextTokens"]),
    "HostedOperationReceipt": ("receipt", ["id", "sessionId", "agentRunId", "turnId", "command", "operationId"]),
    "SessionProject": ("project", ["id", "workspace", "agent", "name"]),
    "Workspace": ("authority", ["id", "status"]),
    "WorkspaceMembership": ("authority", ["id", "workspace", "user", "role"]),
    "AgentDefinition": ("authority", ["id", "workspace", "status", "availability_scope", "published_version"]),
    "AgentDefinitionMember": ("authority", ["id", "definition", "membership"]),
    "UserAppDelegation": ("authority", ["id", "user", "app", "workspace", "definition", "agent", "membership_ref", "issuer", "audience", "scopes", "token_digest", "credential_version", "expires_at", "revoked_at"]),
    "BusinessApplication": ("authority", ["id", "status"]),
    "BrowserLoginSession": ("authority", ["session_key", "session_data", "expire_date"]),
    "ExternalIdentity": ("authority", ["id", "user", "provider_id", "provider_fingerprint", "subject"]),
}


FUNCTIONS = r"""
CREATE FUNCTION public.workspace_presentation_bump_v1(change_key text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF change_key IS NULL THEN RETURN; END IF;
    INSERT INTO public.app_core_presentationchangeoutbox (key,"transactionId")
    VALUES(change_key,txid_current()) ON CONFLICT(key,"transactionId") DO NOTHING;
    IF FOUND THEN PERFORM pg_notify('workspace_presentation_changed_v1',change_key); END IF;
END;
$$;
CREATE FUNCTION public.workspace_presentation_agent_v1(agent_id text,kinds text[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE kind text;
BEGIN
    IF agent_id IS NULL THEN RETURN; END IF;
    FOREACH kind IN ARRAY kinds LOOP
        PERFORM public.workspace_presentation_bump_v1('agent:'||agent_id||':'||kind);
    END LOOP;
END;
$$;
CREATE FUNCTION public.workspace_presentation_apply_v1(mode text,item jsonb,changed text[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE sid text; aid text; kind text;
BEGIN
    IF item IS NULL THEN RETURN; END IF;
    sid := item->>'session_id'; aid := item->>'agent_id';
    IF mode='authority' THEN
        PERFORM public.workspace_presentation_bump_v1('authority'); RETURN;
    ELSIF mode='event' THEN
        PERFORM public.workspace_presentation_bump_v1('session:'||sid||':state');
        SELECT agent_id INTO aid FROM public.app_core_session WHERE id=sid;
        kind := item->>'eventType';
        IF kind='model_request_started' THEN
            PERFORM public.workspace_presentation_bump_v1('session:'||sid||':context');
            IF item->>'purpose'='main' THEN
                PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['history']);
            END IF;
        END IF;
        IF kind IN ('agent_run_started','agent_run_completed','agent_run_failed','agent_run_interrupted')
            OR (kind='checkpoint_ref' AND item->>'checkpointStatus'='paused_question') THEN
            PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['navigation','work']);
        END IF;
        IF kind IN ('tool_call','tool_result') AND item->>'toolName'='send_message' THEN
            PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['history']);
        ELSIF kind IN ('tool_call','tool_result') AND item->>'toolName' IN ('dispatch_work','confirm_work_return','get_work') THEN
            PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['history','work']);
        END IF;
        RETURN;
    ELSIF mode='session' THEN
        sid := item->>'id';
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['navigation']);
        IF changed && ARRAY['id','status','deletedAt','purgedAt','owner_id','workspace_id','agent_id'] THEN
            PERFORM public.workspace_presentation_bump_v1('session:'||sid||':state');
            PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['history','work']);
            PERFORM public.workspace_presentation_bump_v1('authority');
        END IF;
        RETURN;
    ELSIF mode='run' THEN
        IF changed <@ ARRAY['transitionReason'] THEN
            IF item->>'transitionReason'='agent_run_lifecycle_dead_lettered' THEN
                SELECT agent_id INTO aid FROM public.app_core_session WHERE id=sid;
                PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['work','history']);
            END IF;
            RETURN;
        END IF;
        SELECT agent_id INTO aid FROM public.app_core_session WHERE id=sid;
        PERFORM public.workspace_presentation_bump_v1('session:'||sid||':state');
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['navigation','work']);
        IF changed && ARRAY['id','status','modelConfig_id'] THEN
            PERFORM public.workspace_presentation_bump_v1('session:'||sid||':context');
        END IF;
        RETURN;
    ELSIF mode='agent' THEN
        aid := item->>'id';
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['navigation','history','work']);
        IF changed && ARRAY['id','status','deletedAt','purgedAt','owner_id','workspace_id','definition_id','is_business_instance'] THEN
            PERFORM public.workspace_presentation_bump_v1('authority');
        END IF;
        RETURN;
    ELSIF mode='input' THEN
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['history']); RETURN;
    ELSIF mode='queue' OR mode='authorization' THEN
        SELECT s.agent_id INTO aid FROM public.app_core_agentrun r JOIN public.app_core_session s ON s.id=r.session_id WHERE r.id=item->>'agent_run_id';
        PERFORM public.workspace_presentation_agent_v1(aid,CASE WHEN mode='queue' THEN ARRAY['history'] ELSE ARRAY['history','work'] END); RETURN;
    ELSIF mode='delivery' THEN
        SELECT agent_id INTO aid FROM public.app_core_agentinput WHERE id=(item->>'input_id')::bigint;
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['history']); RETURN;
    ELSIF mode='return' THEN
        sid := item->>'work_id';
    ELSIF mode='consume' THEN
        SELECT work_id INTO sid FROM public.app_core_agentworkreturn WHERE id=item->>'notice_id';
    ELSIF mode='coordination' OR mode='branch' THEN
        PERFORM public.workspace_presentation_bump_v1('authority');
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['navigation','history','work']);
        IF mode='branch' THEN
            PERFORM public.workspace_presentation_agent_v1(item->>'root_agent_id',ARRAY['navigation','history','work']);
        END IF;
        RETURN;
    ELSIF mode='model' THEN
        FOR sid IN SELECT DISTINCT session_id FROM public.app_core_agentrun WHERE "modelConfig_id"=item->>'id' LOOP
            PERFORM public.workspace_presentation_bump_v1('session:'||sid||':context');
        END LOOP;
        RETURN;
    ELSIF mode='receipt' THEN
        SELECT agent_id INTO aid FROM public.app_core_session WHERE id=item->>'sessionId';
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['work','history']); RETURN;
    ELSIF mode='project' THEN
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['navigation']); RETURN;
    END IF;
    IF mode IN ('work','return','consume') THEN
        SELECT agent_id INTO aid FROM public.app_core_session WHERE id=sid;
        PERFORM public.workspace_presentation_agent_v1(aid,ARRAY['navigation','history','work']);
    END IF;
END;
$$;
"""


def install(apps, schema_editor):
    if schema_editor.connection.vendor == "sqlite":
        return  # Only migration drift checks use SQLite; readers reject it.
    if schema_editor.connection.vendor != "postgresql":
        raise RuntimeError("presentation_changes_require_postgresql")
    schema_editor.execute(FUNCTIONS)
    sources = [(apps.get_model("app_core", name), mode, fields) for name, (mode, fields) in SOURCES.items()]
    sources.append((apps.get_model("auth", "User"), "authority", ["id", "password", "is_active", "is_staff", "is_superuser"]))
    identities = {"id", "agent_id", "session_id", "agent_run_id", "input_id", "work_id", "notice_id", "root_agent_id", "transitionReason", "sessionId"}
    quote = schema_editor.quote_name
    for model, mode, fields in sources:
        columns = [model._meta.get_field(field).column for field in fields]
        # Compare source columns directly. In particular an inserted event does
        # not serialize its entire potentially large payload into trigger JSON.
        changes = ",".join("CASE WHEN OLD."+quote(c)+" IS DISTINCT FROM NEW."+quote(c)+" THEN '"+c+"' END" for c in columns)
        values = ",".join("'"+c+"',R."+quote(c) for c in columns if c in identities)
        if mode == "event":
            values += ", 'eventType',R.payload->>'type','purpose',R.payload#>>'{payload,purpose}','toolName',R.payload#>>'{payload,toolName}','checkpointStatus',R.payload#>>'{payload,status}'"
        row = "jsonb_build_object("+values+")" if values else "'{}'::jsonb"
        function = "workspace_presentation_capture_"+model._meta.model_name+"_v1"
        all_columns = "ARRAY["+",".join("'"+c+"'" for c in columns)+"]::text[]"
        sql = f"""CREATE FUNCTION public.{quote(function)}() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE changed text[];
BEGIN
    IF TG_OP='UPDATE' THEN
        changed := array_remove(ARRAY[{changes}],NULL);
        IF cardinality(changed)=0 THEN RETURN NEW; END IF;
    ELSE changed := {all_columns}; END IF;
    IF TG_OP<>'INSERT' THEN
        PERFORM public.workspace_presentation_apply_v1('{mode}',{row.replace('R.', 'OLD.')},changed);
    END IF;
    IF TG_OP<>'DELETE' THEN
        PERFORM public.workspace_presentation_apply_v1('{mode}',{row.replace('R.', 'NEW.')},changed);
    END IF;
    RETURN NULL;
END;
$$;
CREATE TRIGGER workspace_presentation_change_v1 AFTER INSERT OR UPDATE OR DELETE ON {quote(model._meta.db_table)}
FOR EACH ROW EXECUTE FUNCTION public.{quote(function)}();"""
        schema_editor.execute(sql)


def uninstall(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return
    models_to_remove = [apps.get_model("app_core", name) for name in SOURCES]
    models_to_remove.append(apps.get_model("auth", "User"))
    for model in models_to_remove:
        schema_editor.execute("DROP TRIGGER workspace_presentation_change_v1 ON " + schema_editor.quote_name(model._meta.db_table))
        schema_editor.execute("DROP FUNCTION public." + schema_editor.quote_name("workspace_presentation_capture_" + model._meta.model_name + "_v1") + "()")
    schema_editor.execute("DROP FUNCTION public.workspace_presentation_apply_v1(text,jsonb,text[])")
    schema_editor.execute("DROP FUNCTION public.workspace_presentation_agent_v1(text,text[])")
    schema_editor.execute("DROP FUNCTION public.workspace_presentation_bump_v1(text)")


def guard_reversal(apps, schema_editor):
    alias = schema_editor.connection.alias
    if (apps.get_model("app_core", "PresentationChange").objects.using(alias).exists()
            or apps.get_model("app_core", "PresentationChangeOutbox").objects.using(alias).exists()):
        raise RuntimeError("presentation_change_history_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0027_session_attention_watermarks")]
    operations = [
        migrations.CreateModel(name="PresentationChange", fields=[
            ("key", models.CharField(max_length=192, primary_key=True, serialize=False)),
            ("revision", models.PositiveBigIntegerField(default=0)),
        ]),
        migrations.CreateModel(name="PresentationChangeOutbox", fields=[
            ("id", models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name="ID")),
            ("key", models.CharField(max_length=192)),
            ("transactionId", models.PositiveBigIntegerField()),
        ], options={"constraints": [models.UniqueConstraint(fields=("key", "transactionId"),
            name="presentation_change_transaction_unique")]}),
        migrations.RunPython(install, uninstall),
        migrations.RunPython(migrations.RunPython.noop, guard_reversal),
    ]

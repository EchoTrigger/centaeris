"""Remove the unused display authorization epoch while preserving source facts.

Authorization remains a fresh read before payload emission and on the idle stream
clock. Display versions track only output caches; they never prove permissions.
"""
from django.db import migrations


AUTHORITY_ONLY_SOURCES = (
    ("app_core", "Workspace"),
    ("app_core", "WorkspaceMembership"),
    ("app_core", "AgentDefinition"),
    ("app_core", "AgentDefinitionMember"),
    ("app_core", "UserAppDelegation"),
    ("app_core", "BusinessApplication"),
    ("app_core", "BrowserLoginSession"),
    ("app_core", "ExternalIdentity"),
    ("auth", "User"),
)


APPLY_FUNCTION = r"""
CREATE OR REPLACE FUNCTION public.workspace_presentation_apply_v1(mode text,item jsonb,changed text[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE sid text; aid text; kind text;
BEGIN
    IF item IS NULL THEN RETURN; END IF;
    sid := item->>'session_id'; aid := item->>'agent_id';
    IF mode='event' THEN
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


def remove_unused_epoch(apps, schema_editor):
    if schema_editor.connection.vendor == "postgresql":
        schema_editor.execute(APPLY_FUNCTION)
        quote = schema_editor.quote_name
        for app, name in AUTHORITY_ONLY_SOURCES:
            model = apps.get_model(app, name)
            schema_editor.execute("DROP TRIGGER workspace_presentation_change_v1 ON " + quote(model._meta.db_table))
            schema_editor.execute("DROP FUNCTION public." + quote("workspace_presentation_capture_" + model._meta.model_name + "_v1") + "()")
    elif schema_editor.connection.vendor != "sqlite":
        raise RuntimeError("presentation_changes_require_postgresql")
    # This key contains disposable display invalidations, never execution facts,
    # authorization grants, delivered transcript water or read acknowledgements.
    alias = schema_editor.connection.alias
    for name in ("PresentationChangeOutbox", "PresentationChange"):
        apps.get_model("app_core", name).objects.using(alias).filter(key="authority").delete()


class Migration(migrations.Migration):
    dependencies = [("app_core", "0028_presentation_changes")]
    operations = [migrations.RunPython(remove_unused_epoch)]

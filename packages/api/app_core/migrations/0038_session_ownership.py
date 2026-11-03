"""Retire business Agents while preserving ordinary Session identity and signed history.

The old deployment must first settle Runs, controllers and physical residents.
This migration never re-signs historical authorization or performs host cleanup.
"""
import hashlib
from importlib import import_module
from django.db import migrations, models
from django.utils import timezone

RETIRED = {
    "agent", "agentdefinition", "agentdefinitionversion", "agentdefinitionmember",
    "agentconnectorbinding", "agentconnectorcredentialapproval", "agentcoordinationsession",
    "businessagentbranch", "agentinput", "agentinputqueue", "agentinputdelivery",
    "agentworksession", "agentworkreturn", "agentworkconsumeattempt",
}


def retire_agent_data(apps, schema_editor):
    db = schema_editor.connection.alias
    get = lambda name: apps.get_model("app_core", name).objects.using(db)
    if schema_editor.connection.vendor == "postgresql":
        schema_editor.execute("LOCK TABLE app_core_session, app_core_agentrun, app_core_sessionenvironment IN ACCESS EXCLUSIVE MODE")
    if get("AgentRun").filter(status__in=("queued", "running")).exists():
        raise RuntimeError("session_ownership_requires_settled_runs")
    if get("SessionEnvironment").filter(controller_released=False).exists():
        raise RuntimeError("session_ownership_requires_released_controllers")
    if schema_editor.connection.vendor == "postgresql":
        with schema_editor.connection.cursor() as cursor:
            for table, predicate in (("resident_sandboxes", "true"), ("environment_retirements", "state='pending'")):
                cursor.execute("SELECT to_regclass(%s)", ["runtime." + table])
                if cursor.fetchone()[0] is not None:
                    cursor.execute("SELECT EXISTS(SELECT 1 FROM runtime." + table + " WHERE " + predicate + ")")
                    if cursor.fetchone()[0]:
                        raise RuntimeError("session_ownership_requires_settled_physical_resources")
    if schema_editor.connection.vendor == "postgresql":
        with schema_editor.connection.cursor() as cursor:
            cursor.execute("SELECT to_regclass('runtime.runtime_jobs')")
            if cursor.fetchone()[0] is not None:
                cursor.execute("SELECT EXISTS(SELECT 1 FROM runtime.runtime_jobs "
                    "WHERE job_kind='agent_work.return' AND status IN ('queued','leased','running'))")
                if cursor.fetchone()[0]:
                    raise RuntimeError("session_ownership_requires_settled_work_returns")
    coordination = set(get("AgentCoordinationSession").values_list("session_id", flat=True))
    schema_editor.execute("CREATE TEMP TABLE retired_coordination_sessions (id varchar(64) PRIMARY KEY)")
    with schema_editor.connection.cursor() as cursor:
        cursor.executemany("INSERT INTO retired_coordination_sessions(id) VALUES(%s)", [(sid,) for sid in coordination])
    schema_editor.execute("CREATE TEMP TABLE retired_coordination_runs (id varchar(64) PRIMARY KEY)")
    with schema_editor.connection.cursor() as cursor:
        cursor.execute("INSERT INTO retired_coordination_runs(id) SELECT id FROM app_core_agentrun WHERE session_id IN (SELECT id FROM retired_coordination_sessions)")
    artifact_ids = list(get("Artifact").filter(session_id__in=coordination).values_list("id", flat=True))
    keys = set(get("WorkspaceSnapshotObject").filter(session_id__in=coordination).values_list("storageKey", flat=True))
    keys.update(get("Session").filter(pk__in=coordination).values_list("workspaceStorageKey", flat=True))
    keys.update(get("SessionWorkspacePreservation").filter(session_id__in=coordination).values_list("storage_key", flat=True))
    keys.update(get("Artifact").filter(id__in=artifact_ids).values_list("storageKey", flat=True))
    keys.update(get("DerivedResource").filter(ownerKind="artifact", ownerId__in=artifact_ids).values_list("resourceKey", flat=True))
    for row in get("DerivedRepresentation").filter(ownerKind="artifact", ownerId__in=artifact_ids):
        keys.update((row.canonicalTextKey, row.previewPdfKey, row.workbookPreviewKey))
    for key in sorted(keys - {""}):
        digest = hashlib.sha256(key.encode("utf-8")).hexdigest()
        retry = get("StorageCleanupRetry").filter(pk=digest).first()
        if retry is not None and retry.storageKey != key:
            raise RuntimeError("session_ownership_storage_key_conflict")
        if retry is None:
            get("StorageCleanupRetry").create(keyDigest=digest, storageKey=key, collector="retiredSession")
        elif retry.state != "cleaned":
            if retry.state == "cleaning" and retry.leaseExpiresAt and retry.leaseExpiresAt > timezone.now():
                raise RuntimeError("session_ownership_requires_settled_storage_cleanup")
            # The same exact key keeps its existing retry/backoff responsibility.
            get("StorageCleanupRetry").filter(pk=digest).update(collector="retiredSession", publicationToken="")
    # Generic owner identities are not foreign keys. Retire only coordinator
    # artifact representations; library/source content is independent and stays.
    reps = get("DerivedRepresentation").filter(ownerKind="artifact", ownerId__in=artifact_ids)
    get("KnowledgeSegment").filter(representation_id__in=reps.values_list("representationId", flat=True)).delete()
    reps.delete()
    get("DerivedResource").filter(ownerKind="artifact", ownerId__in=artifact_ids).delete()
    if schema_editor.connection.vendor == "postgresql":
        previous = import_module("app_core.migrations.0028_presentation_changes")
        quote = schema_editor.quote_name
        for name in previous.SOURCES:
            model = apps.get_model("app_core", name)
            schema_editor.execute("DROP TRIGGER IF EXISTS workspace_presentation_change_v1 ON " + quote(model._meta.db_table))
            schema_editor.execute("DROP FUNCTION IF EXISTS public." + quote("workspace_presentation_capture_" + model._meta.model_name + "_v1") + "()")
        for function in ("workspace_presentation_apply_v1(text,jsonb,text[])",
                         "workspace_presentation_agent_v1(text,text[])", "workspace_presentation_bump_v1(text)"):
            schema_editor.execute("DROP FUNCTION IF EXISTS public." + function)
        schema_editor.execute("SET CONSTRAINTS ALL DEFERRED")
    # Old target grants are revoked, never widened to a Workspace authority.
    get("UserAppDelegation").filter(revoked_at__isnull=True).update(revoked_at=timezone.now())


def remove_coordination_sessions(apps, schema_editor):
    db = schema_editor.connection.alias
    with schema_editor.connection.cursor() as cursor:
        cursor.execute("SELECT id FROM retired_coordination_sessions")
        coordination = {row[0] for row in cursor.fetchall()}
    session_model = apps.get_model("app_core", "Session")
    doomed = {session_model: coordination}
    candidates = list(apps.get_app_config("app_core").get_models())
    changed = True
    while changed:
        changed = False
        for model in candidates:
            ids = set(doomed.get(model, ()))
            for field in model._meta.fields:
                target = getattr(field.remote_field, "model", None)
                if target not in doomed or not doomed[target]:
                    continue
                related = model.objects.using(db).filter(**{field.attname + "__in": doomed[target]})
                if model == session_model:
                    if related.exclude(pk__in=coordination).exists():
                        raise RuntimeError("session_ownership_cross_session_reference")
                elif any(f.name == "session" for f in model._meta.fields):
                    if related.exclude(session_id__in=coordination).exists():
                        raise RuntimeError("session_ownership_cross_session_reference")
                elif model._meta.model_name == "workspacesnapshotfilereference":
                    if related.exclude(snapshot__session_id__in=coordination).exists():
                        raise RuntimeError("session_ownership_cross_session_file_reference")
                ids.update(related.values_list("pk", flat=True))
            if ids != doomed.get(model, set()):
                doomed[model] = ids
                changed = True
    if schema_editor.connection.vendor == "postgresql":
        # The settled, captured coordinator identities are the only deletion
        # exception. All ordinary profile and release immutability stays intact.
        profile_module = import_module("app_core.migrations.0023_unified_environment_profile")
        release_module = import_module("app_core.migrations.0018_environment_release_receipts")
        profile_function = profile_module.GUARDS.split("CREATE TRIGGER", 1)[0].replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION", 1)
        release_function = release_module.IMMUTABLE.split("CREATE TRIGGER", 1)[0].replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION", 1)
        schema_editor.execute(profile_function.replace("    IF TG_OP='DELETE' THEN", "    IF TG_OP='DELETE' THEN\n        IF EXISTS(SELECT 1 FROM retired_coordination_sessions WHERE id=OLD.session_id) THEN RETURN OLD; END IF;"))
        schema_editor.execute(release_function.replace("BEGIN", "BEGIN\n    IF TG_OP='DELETE' AND EXISTS(SELECT 1 FROM retired_coordination_runs WHERE id=OLD.agent_run_id) THEN RETURN OLD; END IF;", 1))
    for model, ids in doomed.items():
        if ids:
            model.objects.using(db).filter(pk__in=ids)._raw_delete(db)
    if schema_editor.connection.vendor == "postgresql":
        schema_editor.execute(profile_function)
        schema_editor.execute(release_function)
        schema_editor.execute("SET CONSTRAINTS ALL IMMEDIATE")
    schema_editor.execute("DROP TABLE retired_coordination_runs")
    schema_editor.execute("DROP TABLE retired_coordination_sessions")


def reject_reverse(apps, schema_editor):
    raise RuntimeError("session_ownership_requires_forward_migration")


def install_session_notifications(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return
    # Freeze the post-0037 notification contract, including 0030's stored
    # isManuallyUnread rename. Historical capture field lists are not current.
    sources = {
        "Session": ("session", ["id", "workspace", "owner", "project", "current_environment", "instructions",
            "title", "origin", "status", "deletedAt", "purgedAt", "isPinned", "isManuallyUnread",
            "attentionRevision", "latestAttentionSequence", "readThroughSequence", "updatedAt"]),
        "SessionEvent": ("event", ["eventId", "workspace", "session", "agent_run", "sequence",
            "agent_run_sequence", "session_level", "projects_to_agent_run_stream", "payload"]),
        "AgentRun": ("run", ["id", "workspace", "session", "user", "status", "startedAt", "completedAt",
            "preAdmissionCancelledAt", "modelConfig", "transitionReason"]),
        "AgentRunAuthorization": ("authorization", ["agent_run", "digest", "signature", "payload"]),
        "ModelConfig": ("model", ["id", "contextTokens"]),
        "HostedOperationReceipt": ("receipt", ["id", "sessionId", "agentRunId", "turnId", "command", "operationId"]),
        "SessionProject": ("project", ["id", "workspace", "name"]),
    }
    schema_editor.execute(SESSION_FUNCTIONS)
    quote = schema_editor.quote_name
    for name, (mode, fields) in sources.items():
        model = apps.get_model("app_core", name)
        columns = [model._meta.get_field(field).column for field in fields]
        identities = {"id", "workspace_id", "session_id", "agent_run_id", "sessionId", "transitionReason"}
        changes = ",".join("CASE WHEN OLD."+quote(c)+" IS DISTINCT FROM NEW."+quote(c)+" THEN '"+c+"' END" for c in columns)
        values = ",".join("'"+c+"',R."+quote(c) for c in columns if c in identities)
        if mode == "event":
            values += ", 'eventType',R.payload->>'type','purpose',R.payload#>>'{payload,purpose}','toolName',R.payload#>>'{payload,toolName}','checkpointStatus',R.payload#>>'{payload,status}'"
        elif mode == "session":
            values += ", 'mutation',TG_OP"
        row = "jsonb_build_object("+values+")" if values else "'{}'::jsonb"
        function = quote("workspace_presentation_capture_"+model._meta.model_name+"_v1")
        all_columns = "ARRAY["+",".join("'"+c+"'" for c in columns)+"]::text[]"
        schema_editor.execute(f"""CREATE FUNCTION public.{function}() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE changed text[];
BEGIN
 IF TG_OP='UPDATE' THEN
  changed := array_remove(ARRAY[{changes}],NULL);
  IF cardinality(changed)=0 THEN RETURN NULL; END IF;
 ELSE changed := {all_columns}; END IF;
 IF TG_OP<>'INSERT' THEN PERFORM public.workspace_presentation_apply_v1('{mode}',{row.replace('R.', 'OLD.')},changed); END IF;
 IF TG_OP<>'DELETE' THEN PERFORM public.workspace_presentation_apply_v1('{mode}',{row.replace('R.', 'NEW.')},changed); END IF;
 RETURN NULL;
END; $$;
CREATE TRIGGER workspace_presentation_change_v1 AFTER INSERT OR UPDATE OR DELETE ON {quote(model._meta.db_table)}
FOR EACH ROW EXECUTE FUNCTION public.{function}();""")


SESSION_FUNCTIONS = r"""
CREATE FUNCTION public.workspace_presentation_bump_v1(change_key text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
 IF change_key IS NULL THEN RETURN; END IF;
 INSERT INTO public.app_core_presentationchangeoutbox(key,"transactionId") VALUES(change_key,txid_current())
 ON CONFLICT(key,"transactionId") DO NOTHING;
 IF FOUND THEN PERFORM pg_notify('workspace_presentation_changed_v1',change_key); END IF;
END; $$;
CREATE FUNCTION public.workspace_presentation_workspace_v1(workspace_id text,kinds text[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE kind text;
BEGIN
 IF workspace_id IS NULL THEN RETURN; END IF;
 FOREACH kind IN ARRAY kinds LOOP
  PERFORM public.workspace_presentation_bump_v1('workspace:'||workspace_id||':'||kind);
 END LOOP;
END; $$;
CREATE FUNCTION public.workspace_presentation_apply_v1(mode text,item jsonb,changed text[]) RETURNS void LANGUAGE plpgsql AS $$
DECLARE sid text; wid text; kind text;
BEGIN
 IF item IS NULL THEN RETURN; END IF;
 sid := item->>'session_id'; wid := item->>'workspace_id';
 IF mode='event' THEN
  PERFORM public.workspace_presentation_bump_v1('session:'||sid||':state');
  kind := item->>'eventType';
  IF kind='model_request_started' THEN
   PERFORM public.workspace_presentation_bump_v1('session:'||sid||':context');
   IF item->>'purpose'='main' THEN
    PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['history']);
   END IF;
  END IF;
  IF kind IN ('agent_run_started','agent_run_completed','agent_run_failed','agent_run_interrupted')
     OR (kind='checkpoint_ref' AND item->>'checkpointStatus'='paused_question') THEN
   PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['navigation','work']);
  END IF;
  RETURN;
 ELSIF mode='session' THEN
  sid := item->>'id';
  PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['navigation']);
  IF changed && ARRAY['id','status','deletedAt','purgedAt','owner_id','workspace_id'] THEN
   PERFORM public.workspace_presentation_bump_v1('session:'||sid||':state');
   -- A newly created Session has no history or work to invalidate in another
   -- Session. Existing identity/retention changes retain the established keys.
   IF item->>'mutation'<>'INSERT' THEN
    PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['history','work']);
   END IF;
  END IF;
  RETURN;
 ELSIF mode='run' THEN
  IF changed <@ ARRAY['transitionReason'] THEN
   IF item->>'transitionReason'='agent_run_lifecycle_dead_lettered' THEN
    PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['work','history']);
   END IF;
   RETURN;
  END IF;
  PERFORM public.workspace_presentation_bump_v1('session:'||sid||':state');
  PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['navigation','work']);
  IF changed && ARRAY['id','status','modelConfig_id'] THEN
   PERFORM public.workspace_presentation_bump_v1('session:'||sid||':context');
  END IF;
  RETURN;
 ELSIF mode='authorization' THEN
  SELECT workspace_id INTO wid FROM public.app_core_agentrun WHERE id=item->>'agent_run_id';
  PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['history','work']);
  RETURN;
 ELSIF mode='model' THEN
  FOR sid IN SELECT DISTINCT session_id FROM public.app_core_agentrun WHERE "modelConfig_id"=item->>'id' LOOP
   PERFORM public.workspace_presentation_bump_v1('session:'||sid||':context');
  END LOOP;
  RETURN;
 ELSIF mode='receipt' THEN
  SELECT workspace_id INTO wid FROM public.app_core_session WHERE id=item->>'sessionId';
  PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['work','history']);
  RETURN;
 ELSIF mode='project' THEN
  PERFORM public.workspace_presentation_workspace_v1(wid,ARRAY['navigation']);
  RETURN;
 END IF;
END; $$;

"""


class Migration(migrations.Migration):

    dependencies = [
        ('app_core', '0037_merge_workspace_resources_and_agents'),
    ]

    operations = [
        migrations.RemoveConstraint(model_name="userappdelegation", name="app_delegation_target_exactly_one"),
        migrations.RunPython(retire_agent_data, reject_reverse),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agent', name='definition')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agent', name='deletedBy')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agent', name='model_config')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agent', name='owner')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agent', name='workspace')]),
        migrations.RemoveField(
            model_name='sessionproject',
            name='agent',
        ),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='businessagentbranch', name='agent')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentcoordinationsession', name='agent')]),
        migrations.RemoveField(
            model_name='userappdelegation',
            name='agent',
        ),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='businessagentbranch', name='root_agent')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinput', name='agent')]),
        migrations.RemoveField(
            model_name='session',
            name='agent',
        ),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorbinding', name='approval')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorbinding', name='definition')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorbinding', name='updated_by')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorbinding', name='workspace')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorcredentialapproval', name='approved_by')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorcredentialapproval', name='credential')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorcredentialapproval', name='definition')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentconnectorcredentialapproval', name='workspace')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentcoordinationsession', name='session')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinition', name='created_by')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinition', name='model_config')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinition', name='published_version')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinition', name='workspace')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinitionmember', name='definition')]),
        migrations.RemoveField(
            model_name='workspace',
            name='default_agent_definition',
        ),
        migrations.RemoveField(
            model_name='userappdelegation',
            name='definition',
        ),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinitionversion', name='definition')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinitionmember', name='membership')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinitionversion', name='model_config')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentdefinitionversion', name='published_by')]),
        migrations.RemoveField(
            model_name='agentrun',
            name='definition_version',
        ),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinput', name='acting_app')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinput', name='app_delegation')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinput', name='session')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinputdelivery', name='input')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinputqueue', name='initial_input')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinputdelivery', name='queue')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinputqueue', name='agent_run')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentinputqueue', name='initial_host')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworkconsumeattempt', name='coordinator_run')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworkconsumeattempt', name='notice')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworkconsumeattempt', name='operation')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworkreturn', name='child_run')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworkreturn', name='work')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworksession', name='coordination_session')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworksession', name='operation')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworksession', name='session')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='agentworksession', name='source_run')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='businessagentbranch', name='app')]),
        migrations.SeparateDatabaseAndState(state_operations=[migrations.RemoveField(model_name='businessagentbranch', name='session')]),
        migrations.AddField(
            model_name='session',
            name='instructions',
            field=models.TextField(blank=True, default=''),
        ),
        migrations.DeleteModel(
            name='Agent',
        ),
        migrations.DeleteModel(
            name='AgentConnectorBinding',
        ),
        migrations.DeleteModel(
            name='AgentConnectorCredentialApproval',
        ),
        migrations.DeleteModel(
            name='AgentCoordinationSession',
        ),
        migrations.DeleteModel(
            name='AgentDefinition',
        ),
        migrations.DeleteModel(
            name='AgentDefinitionMember',
        ),
        migrations.DeleteModel(
            name='AgentDefinitionVersion',
        ),
        migrations.DeleteModel(
            name='AgentInput',
        ),
        migrations.DeleteModel(
            name='AgentInputDelivery',
        ),
        migrations.DeleteModel(
            name='AgentInputQueue',
        ),
        migrations.DeleteModel(
            name='AgentWorkConsumeAttempt',
        ),
        migrations.DeleteModel(
            name='AgentWorkReturn',
        ),
        migrations.DeleteModel(
            name='AgentWorkSession',
        ),
        migrations.DeleteModel(
            name='BusinessAgentBranch',
        ),
        migrations.RunPython(remove_coordination_sessions, reject_reverse),
        migrations.RunPython(install_session_notifications, reject_reverse),
    ]

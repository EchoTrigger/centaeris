from app_core.models import Agent, Session, new_agent_id


def create_session(*, workspace, owner, agent_id=None, agent=None, **fields):
    if agent is None:
        if agent_id is not None:
            agent = Agent.objects.filter(id=agent_id).first()
            if agent is None:
                agent = Agent.objects.create(
                    id=agent_id,
                    workspace=workspace,
                    owner=owner,
                    name="Test Agent",
                )
        else:
            agent = Agent.objects.filter(
                workspace=workspace,
                owner=owner,
                status="active",
            ).first()
            if agent is None:
                default_id = (
                    "centaeris"
                    if not Agent.objects.filter(id="centaeris").exists()
                    else new_agent_id()
                )
                agent = Agent.objects.create(
                    id=default_id,
                    workspace=workspace,
                    owner=owner,
                    name="Centaeris",
                )
    return Session.objects.create(
        workspace=workspace,
        owner=owner,
        agent=agent,
        **fields,
    )

def publish_agent_test_policy(agent, **fields):
    """Explicitly publish the selected fixture policy without inferring defaults.

    Storage-only Sessions remain personal. Admission fixtures opt into this helper
    and supply their authoritative model; accepted historical Runs are untouched.
    """
    from .agent_definitions import publish_definition
    from .models import AgentDefinition
    if agent.definition_id is None:
        definition = AgentDefinition.objects.create(workspace=agent.workspace, created_by=agent.owner,
            name=agent.name, description=agent.description, instructions=agent.instructions,
            avatar_kind=agent.avatar_kind, model_config=agent.model_config,
            thinking_mode=agent.thinking_mode, availability_scope="workspace")
        Agent.objects.filter(pk=agent.pk).update(definition=definition)
        agent.definition = definition
    else:
        definition = agent.definition
    for field, value in fields.items():
        setattr(definition, field, value)
    if fields:
        definition.save(update_fields=[*fields, "updated_at"])
    version = publish_definition(definition, agent.owner)
    agent.definition = definition
    return version


def create_shared_agent(*, workspace, owner, model_config=None, thinking_mode="", **profile):
    """Create a managed fixture with an explicit shared policy and member identity."""
    agent = Agent.objects.create(workspace=workspace, owner=owner, model_config=model_config,
        thinking_mode=thinking_mode, **profile)
    publish_agent_test_policy(agent)
    return agent


def fenced_start_fixture(agent_run, *, execution_mode='perRun'):
    """Arrange persisted controller facts for serializer/input fixtures only.

    This is not an acquisition or lease test. Real admission, contention and
    durability are exercised through environment_lifecycle on PostgreSQL.
    """
    from .environment_lifecycle import _authority
    from .models import SessionEnvironment
    from .runtime_client import build_agent_run_start
    environment = SessionEnvironment.objects.get(pk=agent_run.environment_id)
    if environment.session_id != agent_run.session_id:
        raise ValueError('fixture_environment_binding_invalid')
    if environment.controller_run_id != agent_run.pk or environment.controller_released:
        environment.controller_epoch += 1
        environment.controller_run = agent_run
        environment.controller_maintenance_operation = None
        environment.controller_lease_owner = 'fixture:' + agent_run.pk
        environment.controller_purpose = 'execute'
        environment.controller_released = False
        environment.save(update_fields=['controller_epoch', 'controller_run', 'controller_maintenance_operation',
            'controller_lease_owner', 'controller_purpose', 'controller_released'])
    agent_run.environment = environment
    return build_agent_run_start(agent_run, controller_authority=_authority(environment), execution_mode=execution_mode)


def leased_lifecycle_job_fixture(test_case, agent_run, *, isolate_schema=True):
    """Persist the worker lease used by real lifecycle HTTP/controller fixtures.

    Set isolate_schema=False only after the caller has isolated the namespace,
    so the job can share it with that fixture's other Runtime tables.
    """
    from django.db import connection
    from .runtime_schema_test_helpers import isolate_runtime_schema

    test_case.assertTrue(connection.settings_dict["NAME"].startswith("test_"))
    if isolate_schema:
        isolate_runtime_schema(test_case)
    job_id = f"agent_run.lifecycle:{agent_run.pk}"
    lease_owner = "worker:fixture:" + agent_run.pk
    with connection.cursor() as cursor:
        cursor.execute("CREATE SCHEMA IF NOT EXISTS runtime")
        cursor.execute("CREATE TABLE IF NOT EXISTS runtime.runtime_jobs("
            "job_id text PRIMARY KEY, job_kind text NOT NULL, status text NOT NULL, lease_purpose text NOT NULL DEFAULT 'execute', "
            "lease_owner text, lease_expires_at_ms bigint, idempotency_key text NOT NULL, "
            "session_id text, payload_ref text)")
        cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,"
            "lease_expires_at_ms,idempotency_key,session_id,payload_ref) "
            "VALUES(%s,'agent_run.lifecycle','running',%s,"
            "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
            [job_id, lease_owner, f"{job_id}:{agent_run.authorization.digest}",
             agent_run.session_id, f"record:agent_run:{agent_run.pk}"])

    def delete_job():
        with connection.cursor() as cursor:
            cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s", [job_id])

    test_case.addCleanup(delete_job)
    return lease_owner

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

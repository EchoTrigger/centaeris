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

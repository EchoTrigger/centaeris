from app_core.models import Agent


DEFAULT_AGENT_NAME = "Centaeris"
DEFAULT_AGENT_DESCRIPTION = "默认 Agent"


def ensure_default_agent(workspace, user) -> tuple[Agent, bool]:
    existing = Agent.objects.filter(
        workspace=workspace,
        owner=user,
        status="active",
    ).order_by("createdAt", "id").first()
    if existing is not None:
        return existing, False
    return (
        Agent.objects.create(
            workspace=workspace,
            owner=user,
            name=DEFAULT_AGENT_NAME,
            description=DEFAULT_AGENT_DESCRIPTION,
        ),
        True,
    )

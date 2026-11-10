"""Business bindings shared by grants and consumers, not physical allocation."""


def agent_run_environment_matches(agent_run, *, session=None):
    environment_id = getattr(agent_run, "environment_id", None)
    if not environment_id:
        return False
    environment = agent_run.environment
    selected_session = agent_run.session if session is None else session
    return (
        environment.id == environment_id
        and environment.session_id == agent_run.session_id
        and selected_session.id == agent_run.session_id
    )

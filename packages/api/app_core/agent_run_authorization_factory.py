from django.conf import settings

from .models import AgentRunAuthorization, AgentRun, new_agent_run_authorization_id
from .plugin_catalog import plugin_lifecycle_lock
from .runtime_contract import (
    build_agent_run_authorization_payload,
    authorization_digest,
    authorization_signature,
)
from .workspace_access import agent_run_membership_is_current


def create_agent_run_authorization(
    agent_run: AgentRun,
    message_asset_refs: list[str] | None = None,
    *,
    image_digest: str,
    declared_inputs: list[dict] | None = None,
) -> AgentRunAuthorization:
    with plugin_lifecycle_lock():
        if not agent_run_membership_is_current(agent_run):
            raise ValueError("AgentRun WorkspaceMembership is no longer current")
        authorization_id = new_agent_run_authorization_id()
        payload = build_agent_run_authorization_payload(
            agent_run,
            authorization_id,
            message_asset_refs,
            image_digest=image_digest,
            declared_inputs=declared_inputs,
        )
        return AgentRunAuthorization.objects.create(
            id=authorization_id,
            agent_run=agent_run,
            payload=payload,
            digest=authorization_digest(payload),
            signature=authorization_signature(
                payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY
            ),
        )

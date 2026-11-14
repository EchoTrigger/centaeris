"""Credential-owned conversation lifecycle for Agent API messages."""
from datetime import timedelta

from django.utils import timezone

from .agent_run_api_credentials import source_object_allowed
from .hosted_operations import HostedOperationError
from .models import AgentRunApiConversation, AgentRunApiOperation, ConversationResetPolicy, Session


RESET_DELAYS = {
    ConversationResetPolicy.ON_REOPEN: timedelta(),
    ConversationResetPolicy.AFTER_10_MINUTES: timedelta(minutes=10),
    ConversationResetPolicy.AFTER_15_MINUTES: timedelta(minutes=15),
    ConversationResetPolicy.AFTER_30_MINUTES: timedelta(minutes=30),
    ConversationResetPolicy.AFTER_60_MINUTES: timedelta(minutes=60),
}


class ConversationReset(HostedOperationError):
    def __init__(self, replacement_id):
        super().__init__(409, "conversation_reset")
        self.replacement_id = replacement_id


def owned_conversation(credential, conversation_id):
    # Submit and close hold the same credential lock for their entire transaction.
    conversation = AgentRunApiConversation.objects.filter(pk=conversation_id, credential=credential).first()
    if conversation is None:
        raise HostedOperationError(404, "conversation_not_found")
    if conversation.supersededById:
        raise ConversationReset(conversation.supersededById)
    return conversation


def close_conversation(credential, conversation_id):
    conversation = owned_conversation(credential, conversation_id)
    if conversation.closedAt is None:
        conversation.closedAt = timezone.now()
        conversation.save(update_fields=["closedAt"])
    return conversation


def message_session(credential, conversation_id, now):
    previous = owned_conversation(credential, conversation_id) if conversation_id else None
    delay = RESET_DELAYS.get(credential.resetPolicy)
    if previous is None or (previous.closedAt is not None and delay is not None and now >= previous.closedAt + delay):
        session = Session.objects.create(workspace=credential.workspace, owner=credential.user, purpose="agentRunApi")
        conversation = AgentRunApiConversation.objects.create(credential=credential, sessionId=session.pk)
        if previous is not None:
            previous.supersededById = conversation.pk
            previous.save(update_fields=["supersededById"])
            AgentRunApiOperation.objects.filter(sessionId=previous.sessionId, cleanupStatus="retained").update(cleanupStatus="pending")
        return session, conversation, None

    session = Session.objects.select_for_update().get(
        pk=previous.sessionId, workspace=credential.workspace, owner=credential.user, purpose="agentRunApi",
    )
    if session.agent_runs.filter(status__in={"queued", "running"}).exists():
        raise HostedOperationError(409, "session_has_active_agent_run")
    pending_preservation = session.agent_runs.filter(
        environment_id=session.current_environment_id,
        status__in={"completed", "failed", "cancelled"},
        preAdmissionCancelledAt__isnull=True,
        environment_release__isnull=True,
    )
    if pending_preservation.filter(transitionReason="workspace_preservation_blocked").exists():
        raise HostedOperationError(409, "session_workspace_preservation_blocked")
    if pending_preservation.exists():
        raise HostedOperationError(409, "session_workspace_preservation_pending")
    previous_scope = session.agent_runs.order_by("-createdAt", "-pk").values_list(
        "authorization__payload__pluginActivation__digest", flat=True,
    ).first()
    for link in session.assetLinks.filter(sourceObject__isnull=False).select_related("sourceObject__source"):
        if not source_object_allowed(credential, link.sourceObject):
            raise HostedOperationError(403, "conversation_authority_changed")
    if previous.closedAt is not None:
        previous.closedAt = None
        previous.save(update_fields=["closedAt"])
    return session, previous, previous_scope

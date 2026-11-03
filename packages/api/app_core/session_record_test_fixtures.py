"""Ordinary owned Session and committed event fixtures; no Agent target."""
from django.contrib.auth import get_user_model
from .models import AgentRun, ModelConfig, Session, SessionEvent, Workspace, WorkspaceMembership

class SessionRecordFixture:
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="session-owner")
        self.other = get_user_model().objects.create_user(username="session-other")
        self.workspace = Workspace.objects.create(name="Sessions", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.other, role="member")
        self.session = Session.objects.create(workspace=self.workspace, owner=self.user)
        self.client.force_login(self.user)

    def message_run(self, session):
        model = ModelConfig.objects.create(displayName="Synthetic Session model")
        return AgentRun.objects.create(workspace=self.workspace, session=session, user=self.user,
            modelConfig=model, prompt="Synthetic input", status="running")

    def commit(self, run, event_type, payload):
        sequence = SessionEvent.objects.filter(session=run.session).count() + 1
        wire = {"schemaVersion": "session.event.v1", "eventId": f"synthetic-{run.pk}-{sequence}",
            "sessionId": run.session_id, "agentRunId": run.pk, "turnId": run.turn_id,
            "eventVersion": 1, "type": event_type, "createdAtMs": sequence, "sequence": sequence,
            "payload": {"callId": "synthetic-call", **payload}}
        return SessionEvent.objects.create(eventId=wire["eventId"], workspace=self.workspace,
            session=run.session, agent_run=run, sequence=sequence, agent_run_sequence=sequence,
            projects_to_agent_run_stream=True, createdAtMs=sequence, payload=wire)

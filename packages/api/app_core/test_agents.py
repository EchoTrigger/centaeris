import json
import unicodedata
from datetime import timedelta
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.test import TestCase
from django.utils import timezone

from app_core.models import (
    Agent,
    AgentRun,
    ModelConfig,
    Session,
    SessionEvent,
    Workspace,
    WorkspaceMembership,
)


class PrivateAgentTests(TestCase):
    def test_unavailable_sessions_do_not_require_runtime(self):
        agent = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Agent")
        deleted = Session.objects.create(workspace=self.workspace, owner=self.member, agent=agent, status="deleted", deletedAt=timezone.now())
        foreign_agent = Agent.objects.create(workspace=self.workspace, owner=self.owner, name="Other")
        foreign = Session.objects.create(workspace=self.workspace, owner=self.owner, agent=foreign_agent)
        active = Session.objects.create(workspace=self.workspace, owner=self.member, agent=agent)
        model = ModelConfig.objects.create(displayName="Test model")
        self.client.force_login(self.member)
        with patch("app_core.http.workspaces.request_execution_profile", side_effect=RuntimeError("offline")) as profile:
            for session_id in (deleted.id, foreign.id, "session_missing"):
                with self.subTest(session_id=session_id):
                    response = self.client.post(
                        f"/api/workspaces/{self.workspace.id}/sessions/{session_id}/messages",
                        data=json.dumps({"operationId": "test-operation-33", "text": "hello", "modelConfigRef": model.id}),
                        content_type="application/json",
                    )
                    self.assertEqual(response.status_code, 404)
                    self.assertEqual(response.json()["error"], "session_not_found")
            profile.assert_not_called()
            response = self.client.post(
                f"/api/workspaces/{self.workspace.id}/sessions/{active.id}/messages",
                data=json.dumps({"operationId": "test-operation-41", "text": "hello", "modelConfigRef": model.id}),
                content_type="application/json",
            )
            self.assertEqual(response.status_code, 503)
            profile.assert_called_once()
        self.assertFalse(AgentRun.objects.exists())

    def setUp(self):
        User = get_user_model()
        self.owner = User.objects.create_user(username="owner@example.test")
        self.member = User.objects.create_user(username="member@example.test")
        self.other = User.objects.create_user(username="other@example.test")
        self.workspace = Workspace.objects.create(name="Default", createdBy=self.owner)
        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.owner,
            role="owner",
        )
        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )

    def create_agent(
        self,
        user,
        *,
        name="Research",
        description="研究简介",
        instructions="保持证据链清晰。",
        avatar_kind="centaeris",
    ):
        self.client.force_login(user)
        return self.client.post(
            f"/api/workspaces/{self.workspace.id}/agents",
            data=json.dumps(
                {
                    "name": name,
                    "description": description,
                    "instructions": instructions,
                    "avatarKind": avatar_kind,
                }
            ),
            content_type="application/json",
        )

    def test_agent_directory_is_private_even_from_workspace_owner(self):
        created = self.create_agent(self.member)
        self.assertEqual(created.status_code, 201, created.content)
        agent = created.json()["agent"]
        self.assertEqual(agent["workspaceId"], self.workspace.id)
        self.assertEqual(agent["instructions"], "保持证据链清晰。")
        self.assertEqual(agent["avatarKind"], "centaeris")
        self.assertEqual(agent["status"], "active")
        self.assertIsNone(agent["deletedAt"])

        listed = self.client.get(f"/api/workspaces/{self.workspace.id}/agents")
        self.assertEqual([item["id"] for item in listed.json()["agents"]], [agent["id"]])

        self.client.force_login(self.owner)
        self.assertEqual(
            self.client.get(f"/api/agents/{agent['id']}").status_code,
            404,
        )
        self.assertEqual(
            self.client.get(f"/api/workspaces/{self.workspace.id}/agents").json(),
            {"agents": []},
        )

    def test_agent_profile_is_canonical_and_strict(self):
        decomposed_name = unicodedata.normalize("NFD", "研究员")
        created = self.create_agent(
            self.member,
            name=f"  {decomposed_name}  ",
            description="  可靠简介  ",
            instructions="  第一行\r\n\r\n第二行  ",
            avatar_kind="banana",
        )
        self.assertEqual(created.status_code, 201, created.content)
        agent_id = created.json()["agent"]["id"]
        self.assertEqual(created.json()["agent"]["name"], "研究员")
        self.assertEqual(created.json()["agent"]["description"], "可靠简介")
        self.assertEqual(created.json()["agent"]["instructions"], "第一行\n\n第二行")
        self.assertEqual(created.json()["agent"]["avatarKind"], "banana")

        updated = self.client.patch(
            f"/api/agents/{agent_id}",
            data=json.dumps(
                {
                    "description": "更新简介",
                    "instructions": "优先核验一手资料。",
                    "avatarKind": "centaeris",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(updated.status_code, 200, updated.content)
        self.assertEqual(updated.json()["agent"]["description"], "更新简介")
        self.assertEqual(updated.json()["agent"]["instructions"], "优先核验一手资料。")
        self.assertEqual(updated.json()["agent"]["avatarKind"], "centaeris")
        unchanged = self.client.patch(
            f"/api/agents/{agent_id}",
            data=json.dumps({"description": "更新简介"}),
            content_type="application/json",
        )
        self.assertEqual(unchanged.status_code, 409)
        self.assertEqual(unchanged.json(), {"error": "agent_unchanged"})

        invalid = self.client.post(
            f"/api/workspaces/{self.workspace.id}/agents",
            data=json.dumps({"name": "Banana", "description": "猫" * 129}),
            content_type="application/json",
        )
        unknown = self.client.post(
            f"/api/workspaces/{self.workspace.id}/agents",
            data=json.dumps({"name": "Banana", "banana": True}),
            content_type="application/json",
        )
        invalid_avatar = self.client.post(
            f"/api/workspaces/{self.workspace.id}/agents",
            data=json.dumps({"name": "Banana", "avatarKind": "BANANA"}),
            content_type="application/json",
        )
        invalid_instructions = self.client.post(
            f"/api/workspaces/{self.workspace.id}/agents",
            data=json.dumps({"name": "Banana", "instructions": "猫" * 16_001}),
            content_type="application/json",
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json(), {"error": "agent_invalid"})
        self.assertEqual(unknown.status_code, 400)
        self.assertEqual(unknown.json(), {"error": "agent_invalid"})
        self.assertEqual(invalid_avatar.status_code, 400)
        self.assertEqual(invalid_avatar.json(), {"error": "agent_invalid"})
        self.assertEqual(invalid_instructions.status_code, 400)
        self.assertEqual(invalid_instructions.json(), {"error": "agent_invalid"})

    def test_sessions_require_an_active_agent_owned_by_the_same_user(self):
        member_agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Member Agent",
        )
        owner_agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.owner,
            name="Owner Agent",
        )
        self.client.force_login(self.member)

        created = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions",
            data=json.dumps({"operationId": "test-operation-194", "agentId": member_agent.id}),
            content_type="application/json",
        )
        denied = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions",
            data=json.dumps({"operationId": "test-operation-199", "agentId": owner_agent.id}),
            content_type="application/json",
        )
        unknown = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions",
            data=json.dumps({"operationId": "test-operation-204", "agentId": "banana"}),
            content_type="application/json",
        )

        self.assertEqual(created.status_code, 201, created.content)
        self.assertEqual(Session.objects.get(id=created.json()["sessionId"]).agent_id, member_agent.id)
        self.assertEqual(denied.status_code, 404)
        self.assertEqual(denied.json(), {"error": "agent_not_found"})
        self.assertEqual(unknown.status_code, 404)
        self.assertEqual(unknown.json(), {"error": "agent_not_found"})
        with self.assertRaisesRegex(ValueError, "ownership mismatch"):
            Session.objects.create(
                workspace=self.workspace,
                owner=self.member,
                agent=owner_agent,
            )

    def test_agent_delete_tombstones_parent_and_preserves_sessions(self):
        agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Disposable",
        )
        active_session = Session.objects.create(
            workspace=self.workspace,
            owner=self.member,
            agent=agent,
            title="Active child",
            isPinned=True,
        )
        deleted_session = Session.objects.create(
            workspace=self.workspace,
            owner=self.member,
            agent=agent,
            title="Deleted child",
            status="deleted",
            deletedAt=timezone.now(),
        )
        model = ModelConfig.objects.create(displayName="Test model")
        self.client.force_login(self.member)

        self.assertEqual(
            self.client.get(f"/api/agents/{agent.id}/trash/sessions").json(),
            {"error": "agent_not_deleted"},
        )
        self.assertEqual(
            self.client.patch(
                f"/api/sessions/{deleted_session.id}",
                data=json.dumps({"title": "must not change"}),
                content_type="application/json",
            ).status_code,
            404,
        )
        self.assertEqual(
            self.client.post(
                f"/api/workspaces/{self.workspace.id}/sessions/{deleted_session.id}/messages",
                data=json.dumps({"operationId": "test-operation-260", "text": "must not run", "modelConfigRef": model.id}),
                content_type="application/json",
            ).status_code,
            404,
        )
        deleted = self.client.delete(f"/api/agents/{agent.id}")
        self.assertEqual(deleted.status_code, 200, deleted.content)
        agent.refresh_from_db()
        self.assertEqual(agent.status, "deleted")
        self.assertIsNotNone(agent.deletedAt)
        self.assertEqual(
            self.client.get(f"/api/workspaces/{self.workspace.id}/sessions").json(),
            {"sessions": []},
        )
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash",
                {"kind": "session"},
            ).json()["items"],
            [],
        )
        self.assertEqual(
            [
                item["id"]
                for item in self.client.get(
                    f"/api/workspaces/{self.workspace.id}/trash",
                    {"kind": "agent"},
                ).json()["items"]
            ],
            [agent.id],
        )
        nested = self.client.get(f"/api/agents/{agent.id}/trash/sessions")
        self.assertEqual(nested.status_code, 200, nested.content)
        self.assertEqual(
            [(item["id"], item["status"]) for item in nested.json()["sessions"]],
            [(active_session.id, "active"), (deleted_session.id, "deleted")],
        )
        self.assertIsNone(nested.json()["sessions"][0]["deletedAt"])
        self.assertIsNotNone(nested.json()["sessions"][1]["deletedAt"])
        self.assertEqual(
            self.client.post(f"/api/sessions/{deleted_session.id}/restore").json(),
            {"error": "agent_deleted"},
        )
        self.assertEqual(
            self.client.get(f"/api/sessions/{active_session.id}").status_code,
            200,
        )
        message = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions/{active_session.id}/messages",
            data=json.dumps({"operationId": "test-operation-309", "text": "continue", "modelConfigRef": model.id}),
            content_type="application/json",
        )
        self.assertEqual(message.status_code, 410)
        self.assertEqual(message.json(), {"error": "agent_deleted"})
        repeated = self.client.delete(f"/api/agents/{agent.id}")
        self.assertEqual(repeated.status_code, 410)
        self.assertEqual(repeated.json(), {"error": "agent_deleted"})

        restored_agent = self.client.post(f"/api/agents/{agent.id}/restore")
        self.assertEqual(restored_agent.status_code, 200, restored_agent.content)
        self.assertEqual(restored_agent.json()["agent"]["id"], agent.id)
        self.assertIsNone(restored_agent.json()["agent"]["deletedAt"])
        self.assertEqual(
            [
                item["id"]
                for item in self.client.get(
                    f"/api/workspaces/{self.workspace.id}/sessions"
                ).json()["sessions"]
            ],
            [active_session.id],
        )
        self.assertEqual(
            [
                item["id"]
                for item in self.client.get(
                    f"/api/workspaces/{self.workspace.id}/trash",
                    {"kind": "session"},
                ).json()["items"]
            ],
            [deleted_session.id],
        )
        restored_session = self.client.post(
            f"/api/sessions/{deleted_session.id}/restore"
        )
        self.assertEqual(restored_session.status_code, 200, restored_session.content)
        self.assertEqual(restored_session.json()["session"]["id"], deleted_session.id)
        self.assertIsNone(restored_session.json()["session"]["deletedAt"])
        self.assertEqual(
            self.client.post(f"/api/agents/{agent.id}/restore").json(),
            {"error": "agent_not_deleted"},
        )
        self.assertEqual(
            self.client.post(f"/api/sessions/{deleted_session.id}/restore").json(),
            {"error": "session_not_deleted"},
        )
        self.assertEqual(SessionEvent.objects.count(), 0)

    def test_agent_trash_remains_private_from_workspace_admin_and_superuser(self):
        agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Private trash",
        )
        Session.objects.create(
            workspace=self.workspace,
            owner=self.member,
            agent=agent,
        )
        active_agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Active parent",
        )
        deleted_session = Session.objects.create(
            workspace=self.workspace,
            owner=self.member,
            agent=active_agent,
            status="deleted",
            deletedAt=timezone.now(),
        )
        self.client.force_login(self.member)
        self.assertEqual(self.client.delete(f"/api/agents/{agent.id}").status_code, 200)

        for user in (self.owner, self.other):
            if user == self.other:
                user.is_staff = True
                user.is_superuser = True
                user.save(update_fields=["is_staff", "is_superuser"])
                WorkspaceMembership.objects.create(
                    workspace=self.workspace,
                    user=user,
                    role="admin",
                )
            self.client.force_login(user)
            self.assertEqual(
                self.client.get(
                    f"/api/workspaces/{self.workspace.id}/trash",
                    {"kind": "agent"},
                ).json()["items"],
                [],
            )
            self.assertEqual(
                self.client.get(
                    f"/api/workspaces/{self.workspace.id}/trash",
                    {"kind": "session"},
                ).json()["items"],
                [],
            )
            self.assertEqual(
                self.client.get(f"/api/agents/{agent.id}/trash/sessions").status_code,
                404,
            )
            self.assertEqual(
                self.client.post(f"/api/agents/{agent.id}/restore").status_code,
                404,
            )
            self.assertEqual(
                self.client.post(
                    f"/api/sessions/{deleted_session.id}/restore"
                ).status_code,
                404,
            )

    def test_agent_and_session_trash_expire_after_thirty_days(self):
        agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Expired Agent",
        )
        session = Session.objects.create(
            workspace=self.workspace,
            owner=self.member,
            agent=agent,
            title="Expired Session",
            status="deleted",
            deletedAt=timezone.now(),
        )
        self.client.force_login(self.member)
        Session.objects.filter(id=session.id).update(
            deletedAt=timezone.now() - timedelta(days=31),
        )
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash",
                {"kind": "session"},
            ).json()["items"],
            [],
        )
        expired_session = self.client.post(f"/api/sessions/{session.id}/restore")
        self.assertEqual(expired_session.status_code, 410)
        self.assertEqual(expired_session.json(), {"error": "session_expired"})
        self.assertEqual(self.client.delete(f"/api/sessions/{session.id}/trash").status_code, 200)

        self.assertEqual(self.client.delete(f"/api/agents/{agent.id}").status_code, 200)
        Agent.objects.filter(id=agent.id).update(
            deletedAt=timezone.now() - timedelta(days=31),
        )
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash",
                {"kind": "agent"},
            ).json()["items"],
            [],
        )
        expired_agent = self.client.post(f"/api/agents/{agent.id}/restore")
        self.assertEqual(expired_agent.status_code, 410)
        self.assertEqual(expired_agent.json(), {"error": "agent_expired"})
        self.assertEqual(self.client.delete(f"/api/agents/{agent.id}/trash").status_code, 200)
        agent.refresh_from_db()
        session.refresh_from_db()
        self.assertIsNotNone(agent.purgedAt)
        self.assertIsNotNone(session.purgedAt)

    def test_agent_and_session_trash_use_fixed_cursor_pages(self):
        self.client.force_login(self.member)
        deleted_at = timezone.now()
        deleted_agents = [
            Agent.objects.create(
                workspace=self.workspace,
                owner=self.member,
                name=f"Deleted {index}",
                status="deleted",
                deletedAt=deleted_at,
            )
            for index in range(51)
        ]
        first_agents = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"kind": "agent"},
        ).json()
        self.assertEqual(len(first_agents["items"]), 50)
        self.assertTrue(first_agents["hasMore"])
        second_agents = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": first_agents["nextCursor"], "kind": "agent"},
        ).json()
        self.assertEqual(len(second_agents["items"]), 1)
        self.assertFalse(second_agents["hasMore"])
        self.assertEqual(
            {item["id"] for item in first_agents["items"]}
            | {item["id"] for item in second_agents["items"]},
            {agent.id for agent in deleted_agents},
        )

        active_agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Active parent",
        )
        deleted_sessions = [
            Session.objects.create(
                workspace=self.workspace,
                owner=self.member,
                agent=active_agent,
                title=f"Deleted session {index}",
                status="deleted",
                deletedAt=deleted_at,
            )
            for index in range(51)
        ]
        first_sessions = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"kind": "session"},
        ).json()
        second_sessions = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": first_sessions["nextCursor"], "kind": "session"},
        ).json()
        self.assertEqual(len(first_sessions["items"]), 50)
        self.assertEqual(len(second_sessions["items"]), 1)
        self.assertEqual(
            {item["id"] for item in first_sessions["items"]}
            | {item["id"] for item in second_sessions["items"]},
            {session.id for session in deleted_sessions},
        )

        nested_sessions = [
            Session.objects.create(
                workspace=self.workspace,
                owner=self.member,
                agent=deleted_agents[0],
                title=f"Nested session {index}",
            )
            for index in range(51)
        ]
        first_nested = self.client.get(
            f"/api/agents/{deleted_agents[0].id}/trash/sessions"
        ).json()
        second_nested = self.client.get(
            f"/api/agents/{deleted_agents[0].id}/trash/sessions",
            {"cursor": first_nested["nextCursor"]},
        ).json()
        self.assertEqual(len(first_nested["sessions"]), 50)
        self.assertEqual(len(second_nested["sessions"]), 1)
        self.assertEqual(
            {item["id"] for item in first_nested["sessions"]}
            | {item["id"] for item in second_nested["sessions"]},
            {session.id for session in nested_sessions},
        )
        invalid = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": first_agents["nextCursor"], "kind": "session"},
        )
        unknown = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"banana": "1"},
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json(), {"error": "trash_cursor_invalid"})
        self.assertEqual(unknown.status_code, 400)
        self.assertEqual(unknown.json(), {"error": "trash_query_invalid"})

    def test_agent_and_session_restore_roll_back_on_database_failure(self):
        agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Rollback",
        )
        session = Session.objects.create(
            workspace=self.workspace,
            owner=self.member,
            agent=agent,
            status="deleted",
            deletedAt=timezone.now(),
        )
        self.client.force_login(self.member)
        original_session_save = Session.save

        def fail_session_restore(value, *args, **kwargs):
            if value.id == session.id and value.status == "active":
                raise RuntimeError("forced_session_restore_failure")
            return original_session_save(value, *args, **kwargs)

        with patch.object(Session, "save", new=fail_session_restore):
            restored = self.client.post(f"/api/sessions/{session.id}/restore")
        self.assertEqual(restored.status_code, 500)
        session.refresh_from_db()
        self.assertEqual(session.status, "deleted")
        self.assertIsNotNone(session.deletedAt)

        self.assertEqual(self.client.delete(f"/api/agents/{agent.id}").status_code, 200)
        original_agent_save = Agent.save

        def fail_agent_restore(value, *args, **kwargs):
            if value.id == agent.id and value.status == "active":
                raise RuntimeError("forced_agent_restore_failure")
            return original_agent_save(value, *args, **kwargs)

        with patch.object(Agent, "save", new=fail_agent_restore):
            restored = self.client.post(f"/api/agents/{agent.id}/restore")
        self.assertEqual(restored.status_code, 500)
        agent.refresh_from_db()
        self.assertEqual(agent.status, "deleted")
        self.assertIsNotNone(agent.deletedAt)

    def test_agent_delete_rejects_an_active_child_run(self):
        agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Running",
        )
        session = Session.objects.create(
            workspace=self.workspace,
            owner=self.member,
            agent=agent,
        )
        model = ModelConfig.objects.create(displayName="Test model")
        AgentRun.objects.create(
            workspace=self.workspace,
            session=session,
            user=self.member,
            modelConfig=model,
            prompt="running",
        )
        self.client.force_login(self.member)

        response = self.client.delete(f"/api/agents/{agent.id}")

        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json(), {"error": "agent_has_active_agent_run"})
        agent.refresh_from_db()
        self.assertEqual(agent.status, "active")

    def test_membership_removal_hides_but_does_not_delete_private_agent(self):
        agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Persistent",
        )
        membership = WorkspaceMembership.objects.get(
            workspace=self.workspace,
            user=self.member,
        )
        membership.delete()
        self.client.force_login(self.member)

        self.assertEqual(self.client.get(f"/api/agents/{agent.id}").status_code, 404)
        self.assertTrue(Agent.objects.filter(id=agent.id, status="active").exists())

        rejoined = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )
        self.assertEqual(self.client.get(f"/api/agents/{agent.id}").status_code, 200)
        self.assertEqual(self.client.delete(f"/api/agents/{agent.id}").status_code, 200)
        rejoined.delete()
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash"
            ).status_code,
            404,
        )
        self.assertEqual(
            self.client.post(f"/api/agents/{agent.id}/restore").status_code,
            404,
        )

        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )
        self.assertEqual(
            [
                item["id"]
                for item in self.client.get(
                    f"/api/workspaces/{self.workspace.id}/trash",
                    {"kind": "agent"},
                ).json()["items"]
            ],
            [agent.id],
        )
        self.assertEqual(
            self.client.post(f"/api/agents/{agent.id}/restore").status_code,
            200,
        )

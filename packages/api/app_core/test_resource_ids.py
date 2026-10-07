import base64
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.db import IntegrityError, transaction
from django.test import TestCase

from .models import (
    Agent,
    Session,
    Workspace,
    WorkspaceMembership,
    new_agent_id,
    new_session_id,
    new_turn_id,
    new_workspace_id,
)


class ResourceIdTests(TestCase):
    @classmethod
    def setUpTestData(cls):
        cls.owner = get_user_model().objects.create_user(username="id-owner")
        cls.workspace = Workspace.objects.create(name="Original", createdBy=cls.owner)
        cls.agent = Agent.objects.create(workspace=cls.workspace, owner=cls.owner, name="Original")
        cls.session = Session.objects.create(workspace=cls.workspace, owner=cls.owner, agent=cls.agent)

    def resources(self):
        return (
            (Workspace, self.workspace, {"name": "New", "createdBy": self.owner}),
            (Agent, self.agent, {"workspace": self.workspace, "owner": self.owner, "name": "New"}),
            (Session, self.session, {"workspace": self.workspace, "owner": self.owner, "agent": self.agent}),
        )

    def test_generators_encode_twelve_random_bytes_without_truncation(self):
        suffix = "AbCdEf0123-_xyZ9"
        with patch("secrets.token_bytes", return_value=base64.urlsafe_b64decode(suffix)) as random_bytes:
            for generator, prefix in ((new_workspace_id, "ws"), (new_agent_id, "agent"), (new_session_id, "session")):
                with self.subTest(prefix=prefix):
                    self.assertEqual(generator(), f"{prefix}_{suffix}")
            self.assertEqual(random_bytes.call_count, 3)
            self.assertTrue(all(call.args == (12,) for call in random_bytes.call_args_list))
        self.assertRegex(new_turn_id(), r"^turn_[0-9a-f]{32}$")

    def test_generated_primary_key_collision_retries_without_overwriting(self):
        for model, original, fields in self.resources():
            with self.subTest(model=model.__name__), transaction.atomic():
                before = model.objects.filter(pk=original.pk).values().get()
                suffix = original.pk.split("_", 1)[1]
                with patch("app_core.models.secrets.token_urlsafe", side_effect=[suffix, "AbCdEf0123-_xyZ9"]) as generate:
                    created = model.objects.create(**fields)
                self.assertEqual(generate.call_count, 2)
                self.assertNotEqual(created.pk, original.pk)
                self.assertEqual(model.objects.filter(pk=original.pk).values().get(), before)
                self.assertTrue(model.objects.filter(pk=created.pk).exists())

    def test_repeated_collisions_stop_after_three_attempts_and_keep_transaction_usable(self):
        for model, original, fields in self.resources():
            with self.subTest(model=model.__name__), transaction.atomic():
                before = model.objects.count()
                with patch("app_core.models.secrets.token_urlsafe", return_value=original.pk.split("_", 1)[1]) as generate:
                    with self.assertRaises(IntegrityError):
                        model.objects.create(**fields)
                self.assertEqual(generate.call_count, 3)
                self.assertEqual(model.objects.count(), before)

    def test_explicit_primary_keys_and_other_integrity_errors_are_not_retried(self):
        for key in ("id", "pk"):
            with patch("app_core.models.secrets.token_urlsafe", return_value="AbCdEf0123-_xyZ9") as generate:
                with self.subTest(key=key), self.assertRaises(IntegrityError), transaction.atomic():
                    Workspace.objects.create(**{key: self.workspace.pk}, name="Replacement", createdBy=self.owner)
                # Django initializes the default before applying the `pk` alias.
                self.assertEqual(generate.call_count, 1 if key == "pk" else 0)
        with patch("app_core.models.secrets.token_urlsafe", return_value="AbCdEf0123-_xyZ9") as generate:
            with self.assertRaises(IntegrityError):
                Workspace.objects.create(name="Missing owner", createdBy=None)
            generate.assert_called_once_with(12)
        self.workspace.refresh_from_db()
        self.assertEqual(self.workspace.name, "Original")

    def test_short_ids_round_trip_through_owned_api_and_do_not_grant_access(self):
        with patch("app_core.models.secrets.token_urlsafe", return_value="AbCdEf0123-_xyZ9"):
            workspace = Workspace.objects.create(name="Short IDs", createdBy=self.owner)
            agent = Agent.objects.create(workspace=workspace, owner=self.owner, name="Short IDs")
            session = Session.objects.create(workspace=workspace, owner=self.owner, agent=agent)
        WorkspaceMembership.objects.create(workspace=workspace, user=self.owner, role="owner")
        self.client.force_login(self.owner)
        response = self.client.get(f"/api/workspaces/{workspace.pk}/agents")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["agents"][0]["id"], agent.pk)
        for path, envelope, resource in (
            (f"/api/agents/{agent.pk}", "agent", agent),
            (f"/api/sessions/{session.pk}", "session", session),
        ):
            response = self.client.get(path)
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response.json()[envelope]["id"], resource.pk)
        self.assertEqual(
            self.client.get(
                f"/api/sessions/{session.pk}/transcript/active-agent-run",
                {"sourceHighWater": "0"},
            ).status_code,
            200,
        )
        self.assertEqual(self.client.get(f"/api/sessions/{session.pk.lower()}").status_code, 404)

        other = get_user_model().objects.create_user(username="id-other-member")
        WorkspaceMembership.objects.create(workspace=workspace, user=other, role="member")
        self.client.force_login(other)
        for path in (
            f"/api/agents/{agent.pk}",
            f"/api/sessions/{session.pk}",
            f"/api/sessions/{session.pk}/transcript/active-agent-run?sourceHighWater=0",
        ):
            self.assertEqual(self.client.get(path).status_code, 404)
        self.client.logout()
        self.assertEqual(self.client.get(f"/api/sessions/{session.pk}").status_code, 401)

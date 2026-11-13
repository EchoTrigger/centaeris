import asyncio
from datetime import timedelta
from unittest.mock import AsyncMock, patch

from django.contrib.auth import get_user_model
from django.db.models import F
from django.test import SimpleTestCase, TestCase, override_settings
from django.utils import timezone

from .models import AgentRun, ModelConfig, ModelProvider, Session, SessionTitleJob, Workspace, WorkspaceMembership, WorkspaceModelDefaults
from .session_titles import enqueue_session_title, generate_title, process_session_title
from .testing import create_session


class TitleDeadlineTests(SimpleTestCase):
    def test_tools_absent_and_original_script_returned(self):
        from types import SimpleNamespace
        from .session_titles import TITLE_INSTRUCTIONS

        model = SimpleNamespace(resolvedApi="openai-responses", maxOutputTokens=256,
            modelName="test", thinkingModes=["low"])
        for title in ("繁體中文分析", "简体中文分析", "日本語の分析", "한국어 분석"):
            client = AsyncMock()
            client.responses.create.return_value = SimpleNamespace(status="completed", output_text=title)
            with patch("app_core.model_adapter.openai_responses.async_open_ai_responses_client", return_value=client):
                result = asyncio.run(generate_title(model, "請分析這份資料", "low", 1))
            self.assertEqual(result, title)
            payload = client.responses.create.call_args.kwargs
            self.assertNotIn("tools", payload)
            self.assertEqual(payload["instructions"], TITLE_INSTRUCTIONS)
            self.assertEqual(payload["input"][0]["content"], "請分析這份資料")

    def test_request_deadline_cancels_slow_client(self):
        from types import SimpleNamespace
        model = SimpleNamespace(resolvedApi="openai-responses", maxOutputTokens=100)

        async def slow(*args):
            await asyncio.sleep(1)

        with patch("app_core.model_adapter.openai_responses.async_open_ai_responses_client", side_effect=slow):
            with self.assertRaises(TimeoutError):
                asyncio.run(generate_title(model, "原文", "", 0.01))


@override_settings(SESSION_TITLE_TIMEOUT_SECONDS=3)
class SessionTitleTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="title-owner")
        self.workspace = Workspace.objects.create(name="Titles", createdBy=self.user)
        self.member = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.provider = ModelProvider.objects.create(workspace=self.workspace, displayName="Title model",
            api="openai-responses", apiBase="https://example.invalid/v1")
        self.model = ModelConfig.objects.create(provider=self.provider, modelName="title-model",
            thinkingModes=["low", "high"], thinkingMode="high")
        WorkspaceModelDefaults.objects.create(workspace=self.workspace, titleModelConfig=self.model)
        self.session = create_session(workspace=self.workspace, owner=self.user, title="初始标题")
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="請分析繁體中文資料")

    def job(self):
        return enqueue_session_title(self.session, self.run)

    def test_same_source_enqueued_once_after_commit(self):
        with patch("app_core.session_titles._publish") as publish, self.captureOnCommitCallbacks(execute=True):
            job = self.job()
            self.assertEqual(self.job().pk, job.pk)
        publish.assert_called_once_with(job.pk)
        second = AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="later prompt")
        self.assertIsNone(enqueue_session_title(self.session, second))

    def test_cjk_output_and_session_order_are_preserved(self):
        job = self.job()
        before = self.session.updatedAt
        with patch("app_core.session_titles.generate_title", new=AsyncMock(return_value="繁體中文資料分析")) as generate:
            process_session_title(job.pk)
        self.session.refresh_from_db()
        self.assertEqual(self.session.title, "繁體中文資料分析")
        self.assertEqual(self.session.updatedAt, before)
        self.assertEqual(generate.call_args.args[1:3], (self.run.prompt, "low"))
        with patch("app_core.session_titles.generate_title") as repeated:
            process_session_title(job.pk)
        repeated.assert_not_called()

    def test_manual_rename_wins_even_when_identical_to_initial_title(self):
        job = self.job()

        def rename(*args):
            Session.objects.filter(pk=self.session.pk).update(
                title="初始标题", titleRevision=F("titleRevision") + 1)

            async def result():
                return "模型标题"

            return result()

        with patch("app_core.session_titles.generate_title", side_effect=rename):
            process_session_title(job.pk)
        self.session.refresh_from_db()
        self.assertEqual(self.session.title, "初始标题")

    def test_timeout_keeps_initial_title(self):
        job = self.job()
        with patch("app_core.session_titles.generate_title", new=AsyncMock(side_effect=TimeoutError)):
            process_session_title(job.pk)
        self.session.refresh_from_db()
        job.refresh_from_db()
        self.assertEqual(self.session.title, "初始标题")
        self.assertEqual(job.status, "failed")

    def test_manual_title_before_first_run_is_preserved(self):
        Session.objects.filter(pk=self.session.pk).update(titleRevision=1, title="手動命名")
        self.session.refresh_from_db()
        with self.captureOnCommitCallbacks(execute=True), patch("app_core.session_titles._publish") as publish:
            job = self.job()
        self.assertIsNone(job)
        publish.assert_not_called()

    def test_empty_default_creates_no_job_and_clearing_stops_queued_generation(self):
        WorkspaceModelDefaults.objects.filter(workspace=self.workspace).update(titleModelConfig=None)
        with self.captureOnCommitCallbacks(execute=True), patch("app_core.session_titles._publish") as publish:
            self.assertIsNone(self.job())
        self.assertFalse(SessionTitleJob.objects.exists())
        publish.assert_not_called()
        WorkspaceModelDefaults.objects.filter(workspace=self.workspace).update(titleModelConfig=self.model)
        job = self.job()
        WorkspaceModelDefaults.objects.filter(workspace=self.workspace).update(titleModelConfig=None)
        with patch("app_core.session_titles.generate_title") as generate:
            process_session_title(job.pk)
        generate.assert_not_called()

    def test_expired_queue_job_never_dispatches(self):
        job = self.job()
        SessionTitleJob.objects.filter(pk=job.pk).update(createdAt=timezone.now() - timedelta(seconds=4))
        with patch("app_core.session_titles.generate_title") as generate:
            process_session_title(job.pk)
        generate.assert_not_called()

    def test_revoked_membership_unavailable_model_and_cancelled_run_do_not_dispatch(self):
        job = self.job()
        for change in ("membership", "model", "cancelled", "deleted"):
            with self.subTest(change=change):
                SessionTitleJob.objects.filter(pk=job.pk).update(status="pending")
                with patch("app_core.session_titles.generate_title") as generate:
                    if change == "membership":
                        self.member.delete()
                    elif change == "model":
                        ModelConfig.objects.filter(pk=self.model.pk).update(enabled=False)
                    elif change == "cancelled":
                        AgentRun.objects.filter(pk=self.run.pk).update(status="cancelled")
                    else:
                        Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=timezone.now())
                    process_session_title(job.pk)
                generate.assert_not_called()
                if change == "membership":
                    self.member = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
                    AgentRun.objects.filter(pk=self.run.pk).update(membership_ref=self.member.pk)
                ModelConfig.objects.filter(pk=self.model.pk).update(enabled=True)
                AgentRun.objects.filter(pk=self.run.pk).update(status="queued")

    def test_family_revision_used_for_historical_job(self):
        job = self.job()
        ModelConfig.objects.filter(pk=self.model.pk).update(isCurrent=False)
        revision = ModelConfig.objects.create(provider=self.provider, modelName=self.model.modelName,
            familyId=self.model.familyId, revision=2, thinkingMode="low", thinkingModes=["low"])
        with patch("app_core.session_titles.generate_title", new=AsyncMock(return_value="資料分析")) as generate:
            process_session_title(job.pk)
        self.assertEqual(generate.call_args.args[0].pk, revision.pk)

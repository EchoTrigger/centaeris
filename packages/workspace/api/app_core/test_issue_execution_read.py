from django.test import TestCase
from django.utils import timezone

from .models import Artifact, Issue, IssueExecution, Session, SessionEvent, Workspace
from .session_record_test_fixtures import SessionRecordFixture


class IssueExecutionReadTests(SessionRecordFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.issue = Issue.objects.create(workspace=self.workspace, creator=self.user, title="Inspect")
        self.run = self.message_run(self.session)
        self.execution = IssueExecution.objects.create(issue=self.issue, issuer=self.user,
            session=self.session, agent_run=self.run, issueVersion=1,
            issueTitle=self.issue.title, issueDescription="")
        self.url = (f"/api/workspaces/{self.workspace.id}/issues/{self.issue.id}"
            f"/executions/{self.execution.pk}")

    def test_continuations_are_newest_first_paginated_and_do_not_complete_issue(self):
        continuation = self.message_run(self.session)
        continuation.status = "completed"
        continuation.save(update_fields=["status"])
        self.commit(continuation, "assistant_message", {"modelMarkdown": "result"})
        response = self.client.get(self.url + "?limit=1&offset=0")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["total"], 2)
        self.assertEqual(response.json()["runs"][0]["id"], continuation.id)
        self.assertEqual(response.json()["runs"][0]["status"], "completed")
        second = self.client.get(self.url + "?limit=1&offset=1").json()
        self.assertEqual(second["runs"][0]["id"], self.run.id)
        self.issue.refresh_from_db()
        self.assertEqual(self.issue.status, "todo")

    def test_only_current_visible_results_and_phase_summaries_are_exposed(self):
        stale = self.commit(self.run, "assistant_message", {"modelMarkdown": "superseded answer"})
        old_stage = self.commit(self.run, "phase_event",
            {"stage": "model_process_summary", "message": "superseded progress"})
        SessionEvent.objects.filter(pk__in=[stale.pk, old_stage.pk]).update(projects_to_agent_run_stream=False)
        self.commit(self.run, "phase_event", {"stage": "reasoning", "message": "private reasoning"})
        phase = self.commit(self.run, "phase_event", {"stage": "model_process_summary", "message": "visible progress"})
        self.commit(self.run, "assistant_message", {"modelMarkdown": "x" * 4001})
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 200, response.content)
        row = response.json()["runs"][0]
        self.assertEqual(row["summary"], "x" * 4000)
        self.assertTrue(row["summaryTruncated"])
        self.assertEqual([(item["id"], item["text"]) for item in row["activity"]], [(phase.pk, "visible progress")])
        self.assertNotIn("private reasoning", response.content.decode())
        self.assertNotIn("superseded", response.content.decode())

    def test_published_artifacts_are_run_scoped_and_deleted_or_staging_are_hidden(self):
        def artifact(run, **changes):
            return Artifact.objects.create(workspace=self.workspace, session=run.session, agent_run=run,
                createdBy=self.user, displayName="result.txt", safeFilename="result.txt", sizeBytes=1,
                sha256="sha256:" + "a" * 64, storageKey="synthetic-result", **changes)
        published = artifact(self.run, status="published", publishedAt=timezone.now())
        artifact(self.run)
        artifact(self.run, status="published", publishedAt=timezone.now(), deletedAt=timezone.now())
        another_session = Session.objects.create(workspace=self.workspace, owner=self.user)
        artifact(self.message_run(another_session), status="published", publishedAt=timezone.now())
        rows = self.client.get(self.url).json()["runs"][0]["artifacts"]
        self.assertEqual(rows, [{"id": published.id, "displayName": "result.txt",
            "downloadUrl": f"/api/artifacts/{published.id}/download"}])

    def test_workspace_membership_does_not_grant_other_callers_results(self):
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.client.force_login(self.user)
        another = Workspace.objects.create(name="Elsewhere", createdBy=self.user)
        self.assertEqual(self.client.get(self.url.replace(self.workspace.id, another.id)).status_code, 404)
        self.membership.delete()
        self.assertEqual(self.client.get(self.url).status_code, 404)

    def test_session_owner_archive_purge_and_purpose_restrict_access(self):
        for changes in ({"owner": self.other}, {"archivedAt": timezone.now()},
                {"purgeRequestedAt": timezone.now()}, {"purpose": "agentRunApi"},
                {"status": "deleted", "deletedAt": timezone.now(), "deletedBy": self.user,
                 "purgedAt": timezone.now()}):
            with self.subTest(changes=changes):
                original = {key: getattr(self.session, key) for key in changes}
                Session.objects.filter(pk=self.session.pk).update(**changes)
                self.assertEqual(self.client.get(self.url).status_code, 404)
                Session.objects.filter(pk=self.session.pk).update(**original)

    def test_strict_pagination_and_safe_get(self):
        for query in ("limit=0", "limit=101", "offset=-1", "limit=1&limit=2", "unknown=1"):
            with self.subTest(query=query):
                self.assertEqual(self.client.get(self.url + "?" + query).status_code, 400)
        self.assertEqual(self.client.get(self.url + "?limit=20&offset=999").json(), {"runs": [], "total": 1})
        self.client.handler.enforce_csrf_checks = True
        self.assertEqual(self.client.get(self.url).status_code, 200)

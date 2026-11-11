from datetime import timedelta
from urllib.parse import urlencode

from django.db import connection
from django.db.migrations.executor import MigrationExecutor
from django.test import TestCase, TransactionTestCase
from django.utils import timezone

from .models import AgentRun, ModelConfig, Session, Workspace, WorkspaceMembership
from .session_record_test_fixtures import SessionRecordFixture


class SessionSearchTests(SessionRecordFixture, TestCase):
    def message_run(self, session):
        model = ModelConfig.objects.create(displayName="Search fixture")
        return AgentRun.objects.create(workspace=session.workspace, session=session, user=session.owner,
            modelConfig=model, prompt="Search input", status="running")

    def search(self, query, cursor=None):
        params = {"query": query}
        if cursor is not None:
            params["cursor"] = cursor
        return self.client.get(f"/api/workspaces/{self.workspace.id}/sessions/search?{urlencode(params)}")

    def test_all_three_visible_message_types_and_titles_are_searchable(self):
        for event_type, payload, kind in (
            ("user_message", {"text": "请核对管理员权限", "messageId": "input"}, "user"),
            ("phase_event", {"stage": "model_process_summary", "message": "正在核对管理员权限"}, "stage"),
            ("assistant_message", {"modelMarkdown": "管理员权限核对完成", "messageId": "answer"}, "final"),
        ):
            with self.subTest(kind=kind):
                session = Session.objects.create(workspace=self.workspace, owner=self.user, title=kind)
                run = self.message_run(session)
                event = self.commit(run, event_type, payload)
                response = self.search("权限")
                self.assertEqual(response.status_code, 200, response.content)
                result = next(row for row in response.json()["matches"] if row["sessionId"] == session.pk)
                self.assertEqual(result["match"]["kind"], kind)
                self.assertIn("权限", result["match"]["snippet"])
                self.assertEqual(result["match"]["eventId"], event.pk)
                self.assertEqual(result["match"]["sourceSequence"], str(event.sequence))
        self.session.title = "单独的标题匹配"
        self.session.save(update_fields=["title"])
        result = self.search("标题").json()["matches"]
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]["match"]["kind"], "title")

    def test_literal_case_insensitive_matching_and_bounded_long_body_snippets(self):
        run = self.message_run(self.session)
        self.commit(run, "assistant_message", {"modelMarkdown": "前文" * 5000 + "ApiToken_100%" + "Q" * 256 + "后文" * 5000})
        response = self.search("apitoken_100%")
        hit = response.json()["matches"][0]["match"]
        self.assertIn("ApiToken_100%", hit["snippet"])
        self.assertLessEqual(len(hit["snippet"]), 222)
        self.assertEqual(self.search("ApiToken_100_").json()["matches"], [])
        self.assertEqual(self.search("' OR 1=1 --").json()["matches"], [])
        self.assertEqual(len(self.search("前").json()["matches"]), 1)
        long_hit = self.search("q" * 256).json()["matches"][0]["match"]
        self.assertIn("Q" * 256, long_hit["snippet"])
        self.assertLessEqual(len(long_hit["snippet"]), 398)

    def test_results_group_messages_by_session_and_use_the_latest_matching_message(self):
        run = self.message_run(self.session)
        self.commit(run, "user_message", {"text": "用户的权限问题"})
        final = self.commit(run, "assistant_message", {"modelMarkdown": "权限最终答复"})
        rows = self.search("权限").json()["matches"]
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["match"]["eventId"], final.pk)

    def test_other_users_archived_deleted_and_noninteractive_sessions_are_excluded(self):
        for fields in (
            {"owner": self.other},
            {"archivedAt": timezone.now()},
            {"status": "deleted", "deletedAt": timezone.now(), "deletedBy": self.user},
            {"purpose": "agentRunApi"},
        ):
            session = Session.objects.create(**{"workspace": self.workspace, "owner": self.user, "title": "私密权限", **fields})
            self.commit(self.message_run(session), "user_message", {"text": "私密权限"})
        self.assertEqual(self.search("权限").json()["matches"], [])
        other_workspace = Workspace.objects.create(name="Another", createdBy=self.user)
        other_session = Session.objects.create(workspace=other_workspace, owner=self.user, title="私密权限")
        self.assertEqual(self.search("权限").json()["matches"], [])
        WorkspaceMembership.objects.filter(pk=self.membership.pk).delete()
        self.assertEqual(self.search("权限").status_code, 404)

    def test_tools_reasoning_and_status_records_do_not_match(self):
        run = self.message_run(self.session)
        for event_type, payload in (
            ("tool_call", {"text": "隐藏权限", "arguments": {"query": "隐藏权限"}}),
            ("phase_event", {"stage": "model_process_state", "message": "隐藏权限"}),
            ("model_reasoning", {"text": "隐藏权限"}),
        ):
            self.commit(run, event_type, payload)
        self.assertEqual(self.search("隐藏权限").json()["matches"], [])

    def test_keyset_pagination_and_cursor_binding(self):
        timestamp = timezone.now() - timedelta(days=1)
        for index in range(23):
            session = Session.objects.create(workspace=self.workspace, owner=self.user, title="分页权限")
            Session.objects.filter(pk=session.pk).update(updatedAt=timestamp)
        first = self.search("分页").json()
        self.assertEqual(len(first["matches"]), 20)
        second = self.search("分页", first["nextCursor"]).json()
        self.assertEqual(len(second["matches"]), 3)
        self.assertIsNone(second["nextCursor"])
        self.assertFalse({row["sessionId"] for row in first["matches"]} & {row["sessionId"] for row in second["matches"]})
        self.assertEqual(self.search("different", first["nextCursor"]).status_code, 400)
        self.assertEqual(self.search("分页", first["nextCursor"] + "tampered").status_code, 400)
        self.client.force_login(self.other)
        self.assertEqual(self.search("分页", first["nextCursor"]).status_code, 400)

    def test_malformed_queries_fail_and_navigation_still_rejects_query_parameters(self):
        path = f"/api/workspaces/{self.workspace.id}/sessions/search"
        for query in ("", "?query=", "?query=a&query=b", "?query=x&unknown=1", "?query=" + "x" * 257, "?query=%00"):
            self.assertEqual(self.client.get(path + query).status_code, 400)
        response = self.client.get(f"/api/workspaces/{self.workspace.id}/sessions?query=x")
        self.assertEqual(response.status_code, 400)


class SessionSearchMigrationTests(SessionRecordFixture, TransactionTestCase):
    def test_existing_committed_text_is_indexed_and_future_text_is_maintained(self):
        executor = MigrationExecutor(connection)
        executor.migrate([("app_core", "0053_validate_library_note_body_constraint")])
        try:
            run = self.message_run(self.session)
            old = self.commit(run, "user_message", {"text": "历史权限"})
            executor = MigrationExecutor(connection)
            executor.migrate([("app_core", "0054_session_body_search")])
            path = f"/api/workspaces/{self.workspace.id}/sessions/search"
            self.assertEqual(self.client.get(path, {"query": "权限"}).json()["matches"][0]["match"]["eventId"], old.pk)
            new = self.commit(run, "assistant_message", {"modelMarkdown": "新的权限答复"})
            self.assertEqual(self.client.get(path, {"query": "权限"}).json()["matches"][0]["match"]["eventId"], new.pk)
        finally:
            MigrationExecutor(connection).migrate([("app_core", "0054_session_body_search")])

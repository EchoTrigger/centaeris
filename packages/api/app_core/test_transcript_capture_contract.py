"""Rust capture writer and Django reader share a real migrated PostgreSQL DB."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import quote

from django.db import connection
from django.test import TransactionTestCase

from . import test_transcript_content_identity as identity
from .models import SessionEvent, TranscriptOutputCapture


class TranscriptCaptureContractTests(TransactionTestCase):
    # Match the suite's migration-seeded data lifecycle. A plain flush would
    # recreate ContentTypes and collide with the following serialized fixture.
    serialized_rollback = True

    setUp = identity.TranscriptContentIdentityTests.setUp
    replace_snapshot = identity.TranscriptContentIdentityTests.replace_snapshot

    def commit_output(self, content, *, spilled, model_content=None):
        query = identity.TranscriptContentIdentityTests.commit_output(self, content, spilled=spilled,
                                                                    sequence=2, model_content=model_content)
        SessionEvent.objects.create(
            eventId=f"execution-start:{self.run.id}", workspace=self.workspace,
            session=self.session, agent_run=self.run, sequence=1, agent_run_sequence=1,
            projects_to_agent_run_stream=True, createdAtMs=1,
            payload={"type": "agent_run_execution_started", "payload": {
                "executionId": "execution-original", "authorizationDigest": "sha256:" + "a" * 64}},
        )
        return query


    def publish(self, text, *, succeeds=True, event_id=None, execution_id="execution-original"):
        event = SessionEvent.objects.get(session=self.session, payload__type="tool_result")
        wire = {key: value for key, value in event.payload.items() if key != "sequence"}
        if event_id:
            wire["eventId"] = event_id
        db = connection.settings_dict
        self.assertTrue(db["NAME"].startswith("test_"))
        host = db["HOST"]
        if ":" in host:
            host = f"[{host}]"
        url = f"postgresql://{quote(db['USER'], safe='')}:{quote(db['PASSWORD'], safe='')}@{host}:{db['PORT']}/{db['NAME']}"
        with tempfile.TemporaryDirectory(prefix="capture-contract-") as directory:
            fixture = Path(directory) / "fixture.json"
            fixture.write_text(json.dumps({"record": {"sequence": event.sequence, "event": wire},
                "text": text, "prefix": self.prefix.decode(), "executionId": execution_id,
                "succeeds": succeeds}), encoding="utf-8")
            result = subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
                "transcript_capture::tests::transcript_capture_django_contract", "--", "--exact",
                "--ignored", "--nocapture"], cwd=Path(__file__).resolve().parents[3],
                env={**os.environ, "CENTAERIS_CAPTURE_TEST_FIXTURE": str(fixture),
                    "CENTAERIS_CAPTURE_TEST_DATABASE": url, "CARGO_BUILD_JOBS": "1"},
                capture_output=True, text=True, encoding="utf-8", timeout=240)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("transcript-capture-django-contract-ok", result.stdout)

    def test_committed_capture_is_immutable_and_deleted_owner_cannot_be_resurrected(self):
        original = "a" * 65534 + "世界" + "a" * 70000
        query = self.commit_output(original, spilled=True)
        self.publish(original, event_id="not-committed", succeeds=False)
        self.assertFalse(TranscriptOutputCapture.objects.exists())
        self.publish(original, execution_id="wrong-first-execution", succeeds=False)
        self.assertFalse(TranscriptOutputCapture.objects.exists())
        self.publish(original)
        self.publish(original)  # retry is idempotent
        self.publish(original, execution_id="stale-execution", succeeds=False)
        self.publish("b" * len(original.encode()), succeeds=False)
        self.replace_snapshot("b" * len(original.encode()))
        parts = []
        while True:
            response = self.client.get(self.url, query)
            self.assertEqual(response.status_code, 200, response.content[:200])
            parts.append(response.json()["content"])
            if not response.json()["hasMore"]:
                break
            query["offset"] = response.json()["endOffset"]
        self.assertEqual("".join(parts), original)
        self.run.status = "completed"
        self.run.save(update_fields=["status"])
        self.assertEqual(self.client.delete(f"/api/sessions/{self.session.id}").status_code, 200)
        self.publish(original, succeeds=False)
        capture = TranscriptOutputCapture.objects.get()
        self.assertIsNotNone(capture.purgedAt)
        self.assertFalse(capture.chunks.exists())

    def test_chunk_failure_rolls_back_capture_without_changing_tool_result(self):
        original = "a" * 70000
        self.commit_output(original, spilled=True)
        committed = SessionEvent.objects.get(payload__type="tool_result").payload
        with connection.cursor() as cursor:
            cursor.execute("""CREATE FUNCTION capture_test_failure() RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN IF NEW.index = 1 THEN RAISE EXCEPTION 'injected capture failure'; END IF;
                RETURN NEW; END $$""")
            cursor.execute("""CREATE TRIGGER capture_test_failure BEFORE INSERT ON app_core_transcriptoutputchunk
                FOR EACH ROW EXECUTE FUNCTION capture_test_failure()""")
        try:
            self.publish(original, succeeds=False)
            self.assertFalse(TranscriptOutputCapture.objects.exists())
            self.assertEqual(SessionEvent.objects.get(payload__type="tool_result").payload, committed)
        finally:
            with connection.cursor() as cursor:
                cursor.execute("DROP TRIGGER capture_test_failure ON app_core_transcriptoutputchunk")
                cursor.execute("DROP FUNCTION capture_test_failure()")

    def test_control_byte_capture_restores_the_exact_committed_preview_and_full_output(self):
        original = "kernel\0 literal \\u0000 " + "a" * 70000 + "中文"
        query = self.commit_output(original, spilled=True, model_content="kernel\0")
        event = SessionEvent.objects.get(payload__type="tool_result")
        self.publish(original)
        parts = []
        while True:
            response = self.client.get(self.url, query)
            self.assertEqual(response.status_code, 200, response.content[:200])
            parts.append(response.json()["content"])
            if not response.json()["hasMore"]:
                break
            query["offset"] = response.json()["endOffset"]
        self.assertEqual("".join(parts), original)
        self.assertEqual(SessionEvent.objects.get(pk=event.pk).payload["payload"]["modelContent"], "kernel\0")

    def test_delete_wins_while_chunk_upload_is_blocked(self):
        import time
        from concurrent.futures import ThreadPoolExecutor
        from django.db import connections
        # Compilation is outside the concurrency window; the barrier measures
        # database behavior, never the machine's Rust build time.
        subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server", "--no-run"],
            cwd=Path(__file__).resolve().parents[3], capture_output=True, check=True, timeout=240)
        original = "a" * 70000
        self.commit_output(original, spilled=True)
        self.run.status = "completed"
        self.run.save(update_fields=["status"])
        with connection.cursor() as cursor:
            cursor.execute("SELECT pg_advisory_lock(71001)")
            cursor.execute("""CREATE FUNCTION capture_test_wait() RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN IF NEW.index = 1 THEN PERFORM pg_advisory_xact_lock(71001); END IF;
                RETURN NEW; END $$""")
            cursor.execute("""CREATE TRIGGER capture_test_wait BEFORE INSERT ON app_core_transcriptoutputchunk
                FOR EACH ROW EXECUTE FUNCTION capture_test_wait()""")

        def upload():
            try:
                self.publish(original, succeeds=False)
            finally:
                connections.close_all()

        with ThreadPoolExecutor(max_workers=1) as executor:
            future = executor.submit(upload)
            try:
                deadline = time.monotonic() + 15
                while True:
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory'")
                        waiting = cursor.fetchone()[0]
                    if waiting:
                        break
                    if future.done():
                        future.result()
                        self.fail("publisher never reached the blocked chunk")
                    self.assertLess(time.monotonic(), deadline, "publisher never reached chunk barrier")
                    time.sleep(0.02)
                with connection.cursor() as cursor:
                    cursor.execute("SET statement_timeout='1s'")
                self.assertEqual(self.client.delete(f"/api/sessions/{self.session.id}").status_code, 200)
            finally:
                with connection.cursor() as cursor:
                    cursor.execute("SET statement_timeout=0")
                    cursor.execute("SELECT pg_advisory_unlock(71001)")
                try:
                    future.result(timeout=30)
                finally:
                    with connection.cursor() as cursor:
                        cursor.execute("DROP TRIGGER capture_test_wait ON app_core_transcriptoutputchunk")
                        cursor.execute("DROP FUNCTION capture_test_wait()")
        self.assertFalse(TranscriptOutputCapture.objects.exists())

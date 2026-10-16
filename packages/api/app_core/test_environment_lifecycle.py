"""Controller authority is serialized on PostgreSQL, independent of residency."""
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier, Event

from django.db import close_old_connections, connection, transaction
from django.test import SimpleTestCase, TransactionTestCase

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, WorkspaceMembership
from .test_execution_publication_fencing import ExecutionPublicationFixture


class EnvironmentControllerTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True
    def acquire(self, run=None, owner=None):
        from .environment_lifecycle import acquire_environment_controller
        return acquire_environment_controller((run or self.run).pk, owner or self.owner)

    def release(self, authority, run=None, owner=None):
        from .environment_lifecycle import release_environment_controller
        return release_environment_controller((run or self.run).pk,
            authority["controllerEpoch"], owner or self.owner)

    def cleanup(self, run=None, owner=None):
        from .environment_lifecycle import acquire_environment_cleanup_controller
        return acquire_environment_cleanup_controller((run or self.run).pk, owner or self.owner)

    def test_authority_mutations_force_durable_commit_without_changing_session_policy(self):
        with connection.cursor() as cursor:
            cursor.execute("SHOW fsync")
            self.assertEqual(cursor.fetchone()[0], "on")
            cursor.execute("CREATE TABLE public.environment_controller_commit_policy(epoch bigint, released boolean, policy text)")
            cursor.execute("CREATE FUNCTION public.observe_environment_controller_commit_policy() RETURNS trigger "
                "LANGUAGE plpgsql AS $$ BEGIN INSERT INTO public.environment_controller_commit_policy "
                "VALUES(NEW.controller_epoch,NEW.controller_released,current_setting('synchronous_commit')); "
                "RETURN NEW; END $$")
            cursor.execute("CREATE TRIGGER observe_environment_controller_commit_policy AFTER UPDATE "
                "ON public.app_core_sessionenvironment FOR EACH ROW "
                "EXECUTE FUNCTION public.observe_environment_controller_commit_policy()")
        def drop_audit():
            with connection.cursor() as cursor:
                cursor.execute("RESET synchronous_commit")
                cursor.execute("DROP TRIGGER observe_environment_controller_commit_policy ON public.app_core_sessionenvironment")
                cursor.execute("DROP FUNCTION public.observe_environment_controller_commit_policy()")
                cursor.execute("DROP TABLE public.environment_controller_commit_policy")
        self.addCleanup(drop_audit)
        with connection.cursor() as cursor:
            cursor.execute("SET synchronous_commit=off")
        execute = self.acquire()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        cleanup = self.cleanup()
        self.release(cleanup)
        with connection.cursor() as cursor:
            cursor.execute("SHOW synchronous_commit")
            self.assertEqual(cursor.fetchone()[0], "off")
        observer = connection.copy(alias="controller-policy-observer")
        try:
            with observer.cursor() as cursor:
                cursor.execute("SELECT epoch,released,policy FROM public.environment_controller_commit_policy ORDER BY epoch,released")
                self.assertEqual(cursor.fetchall(), [(execute["controllerEpoch"], False, "on"),
                    (cleanup["controllerEpoch"], False, "on"), (cleanup["controllerEpoch"], True, "on")])
        finally:
            observer.close()

    def test_authority_receipt_cannot_escape_an_uncommitted_outer_transaction(self):
        receipt = None
        observer = connection.copy(alias="controller-rollback-observer")
        try:
            with transaction.atomic():
                try:
                    receipt = self.acquire()
                except RuntimeError as error:
                    self.assertEqual(str(error), "environment_controller_requires_outermost_transaction")
                with observer.cursor() as cursor:
                    cursor.execute("SELECT controller_epoch FROM public.app_core_sessionenvironment WHERE id=%s",
                        [self.run.environment_id])
                    self.assertEqual(cursor.fetchone()[0], 0)
                transaction.set_rollback(True)
        finally:
            observer.close()
        self.session.current_environment.refresh_from_db()
        self.assertEqual(self.session.current_environment.controller_epoch, 0)
        self.assertIsNone(receipt, "the service returned controller authority that its caller could roll back")

    def test_disabled_fsync_observation_refuses_authority_before_epoch_mutation(self):
        def disabled_fsync(execute, sql, params, many, context):
            if sql.strip().upper() == "SHOW FSYNC":
                return execute("SELECT 'off'", None, many, context)
            return execute(sql, params, many, context)
        with connection.execute_wrapper(disabled_fsync):
            with self.assertRaisesRegex(ValueError, "environment_controller_durability_required"):
                self.acquire()
        self.session.current_environment.refresh_from_db()
        self.assertEqual(self.session.current_environment.controller_epoch, 0)

    def test_terminal_cleanup_takeover_has_separate_authority_and_cannot_revive_a_run(self):
        execution = self.acquire()
        with self.assertRaises(ValueError):
            self.cleanup()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        self._lease(self.replacement)
        cleanup = self.cleanup(owner=self.replacement)
        self.assertEqual(cleanup["controllerPurpose"], "cleanup")
        self.assertGreater(cleanup["controllerEpoch"], execution["controllerEpoch"])
        self.assertEqual(cleanup, self.cleanup(owner=self.replacement))
        with self.assertRaises(ValueError):
            self.acquire(owner=self.replacement)
        receipt = self.release(cleanup, owner=self.replacement)
        self.assertEqual(receipt, self.release(cleanup, owner=self.replacement))
        from .environment_lifecycle import get_environment_release_receipt
        self.assertEqual(receipt, get_environment_release_receipt(self.run.pk, self.replacement))
        with self.assertRaisesRegex(ValueError, "environment_controller_already_released"):
            self.cleanup(owner=self.replacement)
        self.session.current_environment.refresh_from_db()
        self.assertTrue(self.session.current_environment.controller_released)

    def second_run(self):
        run = AgentRun.objects.create(workspace=self.run.workspace, session=self.session,
            user=self.run.user, modelConfig=self.run.modelConfig, prompt="next", status="running")
        authorization = create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        job = f"agent_run.lifecycle:{run.pk}"
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,"
                "lease_expires_at_ms,idempotency_key,session_id,payload_ref) "
                "VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                [job, self.owner, f"{job}:{authorization.digest}", self.session.pk,
                 f"record:agent_run:{run.pk}"])
        def cleanup():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s", [job])
        self.addCleanup(cleanup)
        return run

    def test_same_lease_replays_and_verified_same_run_takeover_fences_old_owner(self):
        first = self.acquire()
        self.assertEqual(first, self.acquire())
        self.assertEqual(first["controllerId"], self.run.pk)
        self.assertEqual(first["controllerEpoch"], 1)
        with self.assertRaises(ValueError):
            self.acquire(owner=self.replacement)
        self._lease(self.replacement)
        replacement = self.acquire(owner=self.replacement)
        self.assertEqual(replacement["controllerEpoch"], 2)
        self.assertEqual(replacement, self.acquire(owner=self.replacement))
        self.assertNotEqual(first["controllerLeaseDigest"], replacement["controllerLeaseDigest"])
        with self.assertRaises(ValueError):
            self.acquire()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        with self.assertRaises(ValueError):
            self.release(first)
        cleanup = self.cleanup(owner=self.replacement)
        self.assertEqual(cleanup["controllerEpoch"], replacement["controllerEpoch"] + 1)
        self.release(cleanup, owner=self.replacement)

    def test_terminal_state_does_not_release_controller_but_explicit_release_allows_next_run(self):
        first = self.acquire()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        second = self.second_run()
        with self.assertRaises(ValueError):
            self.acquire(second)
        cleanup = self.cleanup()
        self.assertEqual(cleanup, self.cleanup())
        receipt = self.release(cleanup)
        self.assertEqual(self.release(cleanup), receipt)
        next_authority = self.acquire(second)
        self.assertEqual(next_authority["controllerEpoch"], cleanup["controllerEpoch"] + 1)
        self.assertEqual(self.release(cleanup), receipt)
        self.assertEqual(self.acquire(second), next_authority)

    def test_lost_release_ack_replays_original_receipt_after_next_run_takeover(self):
        from .models import SessionEnvironment
        self.acquire()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        cleanup = self.cleanup()
        receipt = self.release(cleanup)
        second = self.second_run()
        authority = self.acquire(second)
        before = SessionEnvironment.objects.get(pk=self.run.environment_id).__dict__.copy()
        self._lease(self.replacement)
        self.assertEqual(self.release(cleanup, owner=self.replacement), receipt)
        after = SessionEnvironment.objects.get(pk=self.run.environment_id).__dict__.copy()
        before.pop("_state")
        after.pop("_state")
        self.assertEqual(after, before)
        self.assertEqual(self.acquire(second), authority)
        with self.assertRaises(ValueError):
            self.release(cleanup)
        with self.assertRaises(ValueError):
            self.release(authority, run=second, owner=self.replacement)

    def test_release_history_blocks_cleanup_and_execute_revival_after_next_run_releases(self):
        from .models import SessionEnvironment
        self.acquire()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        self.release(self.cleanup())
        second = self.second_run()
        self.acquire(second)
        AgentRun.objects.filter(pk=second.pk).update(status="completed")
        self.release(self.cleanup(second), run=second)
        before = SessionEnvironment.objects.filter(pk=self.run.environment_id).values().get()
        self._lease(self.replacement)
        with self.assertRaisesRegex(ValueError, "environment_controller_already_released"):
            self.cleanup(owner=self.replacement)
        AgentRun.objects.filter(pk=self.run.pk).update(status="running")
        with self.assertRaisesRegex(ValueError, "environment_controller_already_released"):
            self.acquire(owner=self.replacement)
        self.assertEqual(SessionEnvironment.objects.filter(pk=self.run.environment_id).values().get(), before)

    def test_release_receipt_failure_rolls_back_released_flag(self):
        from unittest.mock import patch
        from .models import AgentRunEnvironmentRelease
        self.acquire()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        cleanup = self.cleanup()
        with patch.object(AgentRunEnvironmentRelease, "save", side_effect=RuntimeError("injected receipt failure")):
            with self.assertRaisesRegex(RuntimeError, "injected receipt failure"):
                self.release(cleanup)
        observer = connection.copy(alias="release-atomicity-observer")
        try:
            with observer.cursor() as cursor:
                cursor.execute("SELECT controller_released FROM app_core_sessionenvironment WHERE id=%s", [self.run.environment_id])
                self.assertFalse(cursor.fetchone()[0])
                cursor.execute("SELECT count(*) FROM app_core_agentrunenvironmentrelease WHERE agent_run_id=%s", [self.run.pk])
                self.assertEqual(cursor.fetchone()[0], 0)
        finally:
            observer.close()
        self.assertTrue(self.release(cleanup)["released"])

    def test_receipt_is_immutable_and_reads_require_current_run_lease_and_original_signature(self):
        from django.db import IntegrityError
        from .environment_lifecycle import get_environment_release_receipt
        from .models import AgentRunEnvironmentRelease
        self.acquire()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        cleanup = self.cleanup()
        receipt = self.release(cleanup)
        self.assertEqual(get_environment_release_receipt(self.run.pk, self.owner), receipt)
        with self.assertRaises(ValueError):
            get_environment_release_receipt(self.run.pk, self.replacement)
        with self.assertRaises(IntegrityError), transaction.atomic():
            AgentRunEnvironmentRelease.objects.filter(agent_run=self.run).update(controller_epoch=99)
        with self.assertRaises(IntegrityError), transaction.atomic():
            AgentRunEnvironmentRelease.objects.filter(agent_run=self.run).delete()
        type(self.authorization).objects.filter(pk=self.authorization.pk).update(signature="hmac-sha256:" + "0" * 64)
        with self.assertRaises(ValueError):
            get_environment_release_receipt(self.run.pk, self.owner)

    def test_foreground_run_cannot_release_and_revoked_original_membership_cannot_acquire(self):
        first = self.acquire()
        with self.assertRaises(ValueError):
            self.release(first)
        WorkspaceMembership.objects.filter(pk=self.run.membership_ref).delete()
        self.run.workspace.members.add(self.run.user)
        with self.assertRaises(ValueError):
            self.acquire()

    def test_distinct_runs_race_on_independent_connections_and_only_one_gets_authority(self):
        second = self.second_run()
        barrier = Barrier(2)
        def contender(identity):
            close_old_connections()
            try:
                with connection.cursor() as cursor:
                    cursor.execute("SELECT pg_backend_pid()")
                    pid = cursor.fetchone()[0]
                barrier.wait(timeout=10)
                from .environment_lifecycle import acquire_environment_controller
                try:
                    return pid, acquire_environment_controller(identity, self.owner)
                except ValueError:
                    return pid, None
            finally:
                connection.close()
        with ThreadPoolExecutor(max_workers=2) as pool:
            outcomes = list(pool.map(contender, [self.run.pk, second.pk]))
        self.assertNotEqual(outcomes[0][0], outcomes[1][0])
        self.assertEqual(sum(authority is not None for _, authority in outcomes), 1)

    def test_forged_or_expired_lease_and_tampered_original_grant_do_not_allocate_epoch(self):
        with self.assertRaises(ValueError):
            self.acquire(owner="forged")
        self._lease(self.owner, expired=True)
        with self.assertRaises(ValueError):
            self.acquire()
        self._lease(self.owner)
        type(self.authorization).objects.filter(pk=self.authorization.pk).update(signature="hmac-sha256:" + "0" * 64)
        with self.assertRaises(ValueError):
            self.acquire()
        self.session.current_environment.refresh_from_db()
        self.assertEqual(self.session.current_environment.controller_epoch, 0)

    def test_controller_cannot_launder_original_run_effort_binding(self):
        AgentRun.objects.filter(pk=self.run.pk).update(thinkingMode="high")
        with self.assertRaises(ValueError):
            self.acquire()

    def test_job_contention_is_retryable_without_committing_an_epoch(self):
        locked, release = Event(), Event()
        def hold_job():
            close_old_connections()
            try:
                with transaction.atomic(), connection.cursor() as cursor:
                    cursor.execute("SELECT job_id FROM runtime.runtime_jobs WHERE job_id=%s FOR UPDATE", [self.job_id])
                    locked.set()
                    self.assertTrue(release.wait(10))
            finally:
                connection.close()
        with ThreadPoolExecutor(max_workers=1) as pool:
            holder = pool.submit(hold_job)
            try:
                self.assertTrue(locked.wait(10))
                from .environment_lifecycle import EnvironmentControllerBusy
                with self.assertRaises(EnvironmentControllerBusy):
                    self.acquire()
                self.session.current_environment.refresh_from_db()
                self.assertEqual(self.session.current_environment.controller_epoch, 0)
            finally:
                release.set()
                holder.result(timeout=10)
        self.assertEqual(self.acquire()["controllerEpoch"], 1)

    def test_session_and_run_lock_contention_is_bounded_and_cannot_allocate_authority(self):
        from .environment_lifecycle import EnvironmentControllerBusy
        def contend():
            close_old_connections()
            try:
                with connection.cursor() as cursor:
                    cursor.execute("SET lock_timeout='300ms'")
                try:
                    self.acquire()
                    return "acquired"
                except EnvironmentControllerBusy:
                    return "blocked"
            finally:
                connection.close()
        with ThreadPoolExecutor(max_workers=1) as pool:
            for table, identity in [("app_core_session", self.session.pk), ("app_core_agentrun", self.run.pk)]:
                with self.subTest(table=table), transaction.atomic(), connection.cursor() as cursor:
                    cursor.execute(f"SELECT id FROM {table} WHERE id=%s FOR UPDATE", [identity])
                    self.assertEqual(pool.submit(contend).result(timeout=5), "blocked")
        self.session.current_environment.refresh_from_db()
        self.assertEqual(self.session.current_environment.controller_epoch, 0)
        self.assertEqual(self.acquire()["controllerEpoch"], 1)


class EnvironmentControllerGrantTests(SimpleTestCase):
    def test_v2_exact_fields_integer_range_and_shared_signature(self):
        import json
        from pathlib import Path
        from .environment_authorization import sign_environment_controller_authorization
        vector = json.loads((Path(__file__).resolve().parents[3] /
            "tests/workspace/fixtures/environment_authorization/v2/valid.json").read_text())
        payload = vector["payload"]
        self.assertEqual(sign_environment_controller_authorization(payload, "test-key"), vector["signature"])
        for mutation in ({"controllerEpoch": True}, {"controllerEpoch": 1.0},
                         {"controllerEpoch": 0}, {"controllerEpoch": 2**63},
                         {"controllerId": "foreign"}, {"controllerLeaseDigest": "unknown"},
                         {"controllerPurpose": "unknown"},
                         {"instanceId": "caller-selected"}, {"executionMode": "perRun"}):
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                sign_environment_controller_authorization(payload | mutation, "test-key")
        for field in payload:
            missing = payload.copy()
            del missing[field]
            with self.subTest(missing=field), self.assertRaises(ValueError):
                sign_environment_controller_authorization(missing, "test-key")

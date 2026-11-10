from datetime import timedelta
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier

from django.db import connections
from django.test import TestCase, TransactionTestCase
from django.db.models.functions import Now
from django.utils import timezone

from . import material_leases as leases
from .models import MaterialProcessingTask


class MaterialLeaseTests(TestCase):
    def setUp(self):
        self.task = MaterialProcessingTask.objects.create(
            representationId="representation:sha256:" + "a" * 64,
            processingSpecification={}, payload={}, executionBackend="platform",
        )

    def expire(self):
        MaterialProcessingTask.objects.filter(pk=self.task.pk).update(
            leaseExpiresAt=Now() - timedelta(seconds=1))

    def test_legacy_task_cannot_be_claimed(self):
        self.task.executionBackend = "runtime"
        self.task.save()
        self.assertIsNone(leases.claim(self.task.pk, "worker-a", 30))

    def test_live_lease_excludes_another_worker_and_same_owner_replay(self):
        first = leases.claim(self.task.pk, "worker-a", 30)
        self.assertIsNotNone(first)
        self.assertIsNone(leases.claim(self.task.pk, "worker-b", 30))
        self.assertIsNone(leases.claim(self.task.pk, "worker-a", 30))

    def test_expiry_reclaims_with_new_fence_even_for_same_worker(self):
        first = leases.claim(self.task.pk, "worker-a", 30)
        self.expire()
        second = leases.claim(self.task.pk, "worker-a", 30)
        self.assertGreater(second.epoch, first.epoch)
        with self.assertRaises(leases.LeaseLost):
            leases.renew(first, 30)
        with self.assertRaises(leases.LeaseLost):
            with leases.fenced_task(first):
                self.fail("Stale worker entered publication transaction")

    def test_expired_lease_cannot_be_renewed_before_reassignment(self):
        lease = leases.claim(self.task.pk, "worker-a", 30)
        self.expire()
        with self.assertRaises(leases.LeaseLost):
            leases.renew(lease, 30)

    def test_renew_preserves_fence_and_attempt(self):
        lease = leases.claim(self.task.pk, "worker-a", 30)
        leases.renew(lease, 60)
        with leases.fenced_task(lease) as task:
            self.assertEqual(task.attemptCount, 1)
            self.assertGreater(task.leaseExpiresAt, timezone.now() + timedelta(seconds=50))

    def test_failed_publication_rolls_back_in_same_transaction(self):
        lease = leases.claim(self.task.pk, "worker-a", 30)
        with self.assertRaisesRegex(RuntimeError, "publication_failed"):
            with leases.fenced_task(lease) as task:
                task.status = "completed"
                task.save()
                raise RuntimeError("publication_failed")
        self.task.refresh_from_db()
        self.assertEqual(self.task.status, "running")

    def test_retry_budget_includes_crashed_attempts(self):
        for _ in range(3):
            self.assertIsNotNone(leases.claim(self.task.pk, "worker-a", 30))
            self.expire()
        self.assertIsNone(leases.claim(self.task.pk, "worker-b", 30))
        self.task.refresh_from_db()
        self.assertEqual(self.task.status, "failed")
        self.assertEqual(self.task.errorCode, "material_processing_attempts_exhausted")
        self.assertEqual(self.task.attemptCount, 3)

    def test_terminal_task_cannot_be_claimed(self):
        self.task.status = "completed"
        self.task.save()
        self.assertIsNone(leases.claim(self.task.pk, "worker-a", 30))

    def test_invalid_lease_arguments_fail_before_claim(self):
        for owner, seconds in [("", 30), ("x" * 129, 30), ("worker", True), ("worker", 0), ("worker", 301)]:
            with self.assertRaises(ValueError):
                leases.claim(self.task.pk, owner, seconds)


class MaterialLeaseConcurrencyTests(TransactionTestCase):
    # Match the suite's transaction fixtures; post-migrate repopulation would
    # otherwise conflict with the following class's serialized restoration.
    serialized_rollback = True

    def test_independent_database_connections_have_one_winner(self):
        task = MaterialProcessingTask.objects.create(
            representationId="representation:sha256:" + "b" * 64,
            processingSpecification={}, payload={}, executionBackend="platform",
        )
        start = Barrier(2)

        def attempt(owner):
            try:
                start.wait(timeout=10)
                return leases.claim(task.pk, owner, 30)
            finally:
                connections.close_all()

        with ThreadPoolExecutor(max_workers=2) as workers:
            claims = list(workers.map(attempt, ["worker-a", "worker-b"]))
        self.assertEqual(sum(claim is not None for claim in claims), 1)
        task.refresh_from_db()
        self.assertEqual(task.attemptCount, 1)
        # A new connection recovers the durable fence after a worker disappears.
        old = next(claim for claim in claims if claim is not None)
        MaterialProcessingTask.objects.filter(pk=task.pk).update(
            leaseExpiresAt=Now() - timedelta(seconds=1))
        connections.close_all()
        recovered = leases.claim(task.pk, "restarted-worker", 30)
        self.assertGreater(recovered.epoch, old.epoch)
        with self.assertRaises(leases.LeaseLost):
            with leases.fenced_task(old):
                self.fail("Prior process retained publication authority")

"""Focused ownership/uncertainty tests; no API discovery or real services."""
import hashlib
import io
import json
import threading
import unittest
from unittest.mock import patch

from test_worker import worker


class LeaseHeartbeatTests(unittest.TestCase):
    def test_ack_after_previous_confirmed_deadline_is_lost_without_another_caller(self):
        clock = [100.0]
        finished = threading.Event()
        count = 0
        def renew(*args, **kwargs):
            nonlocal count
            count += 1
            if count == 2:
                clock[0] = 101.1
        def observe(phase, started, **fields):
            if fields.get("attempt") == 2:
                finished.set()
        with patch.object(worker, "LEASE_MS", 1000), patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(worker, "heartbeat", side_effect=renew), \
                patch.object(worker.observations, "emit", side_effect=observe):
            with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                clock[0] = 100.5
                self.assertTrue(finished.wait(1))
                with self.assertRaises(worker.RuntimeLeaseLost):
                    healthy()

    def test_truncated_http_error_body_preserves_received_heartbeat_status(self):
        class TruncatedBody(io.BytesIO):
            def read(self, *args, **kwargs):
                raise worker.http.client.IncompleteRead(b"partial", 10)
        for status, expected in [(503, worker.DependencyUnavailable), (409, worker.RuntimeLeaseLost)]:
            with self.subTest(status=status):
                response = worker.urllib.error.HTTPError(
                    "http://runtime.invalid/internal/jobs/heartbeat", status, "fixture", {}, TruncatedBody())
                with patch.object(worker.urllib.request, "urlopen", side_effect=response):
                    with self.assertRaises(expected) as caught:
                        worker.heartbeat("job-1", "synthetic-owner")
                if status == 503:
                    self.assertEqual(caught.exception.http_status, 503)

    def test_late_inflight_ack_cannot_revive_an_owner_already_marked_lost(self):
        clock = [100.0]
        entered = threading.Event()
        release = threading.Event()
        count = 0
        def renew(*args, **kwargs):
            nonlocal count
            count += 1
            if count == 2:
                entered.set()
                self.assertTrue(release.wait(2))
                clock[0] = 101.1
        with patch.object(worker, "LEASE_MS", 1000), patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(worker, "heartbeat", side_effect=renew), patch.object(worker, "runtime_request") as mutate:
            with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                clock[0] = 100.5
                self.assertTrue(entered.wait(1))
                clock[0] = 101.0
                with self.assertRaises(worker.RuntimeLeaseLost):
                    healthy()
                release.set()
                with self.assertRaises(worker.RuntimeLeaseLost):
                    worker.complete_job("job-1", "synthetic-owner", [])
            mutate.assert_not_called()

    def test_terminal_handoff_verifies_unconfirmed_lease_before_stopping_only_renewer(self):
        unconfirmed = threading.Event()
        calls = []
        terminal = []
        def renew(*args, **kwargs):
            calls.append(1)
            if len(calls) == 2:
                raise worker.DependencyUnavailable("lost_ack", http_status=503)
        def observe(phase, started, **fields):
            if fields.get("outcome") == "unconfirmed":
                unconfirmed.set()
        def request(path, body=None, **kwargs):
            terminal.append(path)
            self.assertGreaterEqual(len(calls), 3, "uncertain renewal must be verified before terminal dispatch")
            return {"ok": True}
        with patch.object(worker, "LEASE_MS", 1500), patch.object(worker, "heartbeat", side_effect=renew), \
                patch.object(worker.observations, "emit", side_effect=observe), \
                patch.object(worker, "runtime_request", side_effect=request):
            with worker.lease_heartbeats("job-1", "synthetic-owner"):
                self.assertTrue(unconfirmed.wait(2))
                worker.complete_job("job-1", "synthetic-owner", [])
        self.assertEqual(terminal, ["/internal/jobs/complete"])

    def test_transient_failure_recovers_under_original_confirmed_deadline(self):
        for status in (503, None):
            with self.subTest(http_status=status):
                recovered = threading.Event()
                calls = []
                timeouts = []

                def renew(job_id, owner, **kwargs):
                    calls.append((job_id, owner))
                    timeouts.append(kwargs["timeout"])
                    if len(calls) == 2:
                        raise worker.DependencyUnavailable("synthetic-secret", http_status=status)
                    if len(calls) == 3:
                        recovered.set()

                with patch.object(worker, "LEASE_MS", 1000), patch.object(worker, "heartbeat", side_effect=renew):
                    with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                        self.assertTrue(recovered.wait(2), "transient failure must not stop renewal permanently")
                        healthy()
                self.assertEqual(set(calls), {("job-1", "synthetic-owner")})
                self.assertTrue(all(0 < timeout <= 0.9 for timeout in timeouts))

    def test_expiry_is_measured_from_request_start_not_response_arrival(self):
        clock = [100.0]
        def renew(*args, **kwargs):
            clock[0] += 1.1
        with patch.object(worker, "LEASE_MS", 1000), patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(worker, "heartbeat", side_effect=renew):
            with self.assertRaisesRegex(RuntimeError, "lease_lost"):
                with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                    healthy()

    def test_missing_ack_does_not_grant_a_new_deadline(self):
        clock = [100.0]
        with patch.object(worker, "LEASE_MS", 1000), patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(worker, "heartbeat") as renew:
            with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                clock[0] = 101.1
                with self.assertRaisesRegex(RuntimeError, "lease_lost"):
                    healthy()
            self.assertEqual(renew.call_count, 1, "expired owner cannot renew or reclaim")

    def test_terminal_handoff_stops_renewal_before_complete_or_yield(self):
        for operation in ("complete", "yield"):
            with self.subTest(operation=operation):
                terminal_started = threading.Event()
                renew_during_terminal = threading.Event()
                def request(path, body=None, **kwargs):
                    if path.endswith("heartbeat"):
                        if terminal_started.is_set():
                            renew_during_terminal.set()
                        return {"ok": True}
                    terminal_started.set()
                    self.assertFalse(renew_during_terminal.wait(1.1), "terminal handoff cannot overlap heartbeat")
                    if path.endswith("/yield"):
                        return {"ok": True, "runAtMs": body["runAtMs"], "transitionReason": body["transitionReason"]}
                    return {"ok": True}
                with patch.object(worker, "LEASE_MS", 1000), patch.object(worker, "runtime_request", side_effect=request):
                    with worker.lease_heartbeats("job-1", "synthetic-owner"):
                        if operation == "complete":
                            worker.complete_job("job-1", "synthetic-owner", [])
                        else:
                            worker.yield_job("job-1", "synthetic-owner", 1234, "runtime_job_wait")

    def test_heartbeat_rejects_unknown_success_fields(self):
        for payload in ({"ok": False}, {"ok": True, "leaseOwner": "extra"}, None):
            with self.subTest(payload=payload), patch.object(worker, "runtime_request", return_value=payload):
                with self.assertRaisesRegex(RuntimeError, "heartbeat_response_invalid"):
                    worker.heartbeat("job-1", "synthetic-owner")

    def test_confirmed_409_is_not_downgraded_by_an_unavailable_query(self):
        error = RuntimeError("job_heartbeat_rejected")
        error.http_status = 409
        with patch.object(worker, "runtime_request", side_effect=[error, worker.DependencyUnavailable("secret")]) as request:
            with self.assertRaisesRegex(RuntimeError, "lease_lost"):
                worker.heartbeat("job-1", "synthetic-owner")
        self.assertEqual(request.call_count, 1)

    def test_observation_redacts_owner_and_exception_text(self):
        output = io.StringIO()
        recovered = threading.Event()
        calls = []
        def renew(*args, **kwargs):
            calls.append(1)
            if len(calls) == 2:
                raise worker.DependencyUnavailable("private-provider-token", http_status=503)
            if len(calls) == 3:
                recovered.set()
        with patch.object(worker, "LEASE_MS", 1000), patch.object(worker, "heartbeat", side_effect=renew), \
                patch.object(worker.observations, "ENABLED", True), patch("sys.stderr", output):
            with worker.lease_heartbeats("job-1", "private-lease-owner") as healthy:
                self.assertTrue(recovered.wait(2))
                healthy()
        events = [json.loads(line) for line in output.getvalue().splitlines()]
        lease_events = [event for event in events if event.get("phase") == "leaseHeartbeat"]
        self.assertTrue(any(event.get("outcome") == "unconfirmed" for event in lease_events))
        self.assertTrue(any(event.get("outcome") == "renewed" for event in lease_events))
        self.assertNotIn("private-provider-token", output.getvalue())
        self.assertNotIn("private-lease-owner", output.getvalue())
        digest = hashlib.sha256(b"private-lease-owner").hexdigest()
        self.assertTrue(all(event["leaseOwnerSha256"] == digest for event in lease_events))

    def test_uncertain_renewal_blocks_dispatch_until_one_verification_finishes(self):
        verifying = threading.Event()
        release = threading.Event()
        dispatch = threading.Event()
        healthy_called = threading.Event()
        active = 0
        maximum = 0
        count = 0
        lock = threading.Lock()

        def renew(*args, **kwargs):
            nonlocal active, maximum, count
            with lock:
                active += 1
                maximum = max(maximum, active)
                count += 1
                attempt = count
            try:
                if attempt == 2:
                    raise worker.DependencyUnavailable("lost_ack")
                if attempt == 3:
                    verifying.set()
                    self.assertTrue(release.wait(1))
            finally:
                with lock:
                    active -= 1

        with patch.object(worker, "LEASE_MS", 3000), patch.object(worker, "heartbeat", side_effect=renew):
            with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                self.assertTrue(verifying.wait(2))
                def caller():
                    healthy_called.set()
                    healthy()
                    dispatch.set()
                caller_thread = threading.Thread(target=caller)
                caller_thread.start()
                self.assertTrue(healthy_called.wait(1))
                self.assertFalse(dispatch.is_set())
                release.set()
                self.assertTrue(dispatch.wait(1))
                caller_thread.join(1)
        self.assertEqual(maximum, 1)

    def test_confirmed_background_fence_loss_stops_all_old_owner_mutations(self):
        lost = threading.Event()
        calls = []
        def renew(*args, **kwargs):
            calls.append(args)
            if len(calls) == 2:
                raise worker.RuntimeLeaseLost()
        def observe(phase, started, **fields):
            if fields.get("outcome") == "lost":
                lost.set()
        with patch.object(worker, "LEASE_MS", 1000), patch.object(worker, "heartbeat", side_effect=renew), \
                patch.object(worker.observations, "emit", side_effect=observe), \
                patch.object(worker, "runtime_request") as mutation:
            with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                self.assertTrue(lost.wait(1))
                with self.assertRaises(worker.RuntimeLeaseLost):
                    healthy()
                with self.assertRaises(worker.RuntimeLeaseLost):
                    worker.complete_job("job-1", "synthetic-owner", [])
            mutation.assert_not_called()
        self.assertEqual(len(calls), 2, "no retry or claim after a confirmed conflict")

    def test_terminal_handoff_rechecks_deadline_after_inflight_heartbeat(self):
        clock = [100.0]
        entered = threading.Event()
        release = threading.Event()
        count = 0
        def renew(*args, **kwargs):
            nonlocal count
            count += 1
            if count == 2:
                entered.set()
                self.assertTrue(release.wait(2))
                clock[0] = 102.0
        with patch.object(worker, "LEASE_MS", 1000), patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(worker, "heartbeat", side_effect=renew), patch.object(worker, "runtime_request") as mutate:
            with worker.lease_heartbeats("job-1", "synthetic-owner"):
                self.assertTrue(entered.wait(1))
                release.set()
                with self.assertRaises(worker.RuntimeLeaseLost):
                    worker.yield_job("job-1", "synthetic-owner", 1000, "runtime_job_wait")
            mutate.assert_not_called()

    def test_failed_terminal_rpc_does_not_restart_old_guard(self):
        requests = []
        def request(path, body=None, **kwargs):
            requests.append(path)
            if path.endswith("complete"):
                raise worker.DependencyUnavailable("terminal_ack_unknown")
            return {"ok": True}
        with patch.object(worker, "runtime_request", side_effect=request):
            with self.assertRaises(worker.DependencyUnavailable):
                with worker.lease_heartbeats("job-1", "synthetic-owner"):
                    worker.complete_job("job-1", "synthetic-owner", [])
        self.assertEqual(requests, ["/internal/jobs/heartbeat", "/internal/jobs/complete"])

    def test_unknown_terminal_result_cannot_resume_dispatch_in_the_same_guard(self):
        def request(path, body=None, **kwargs):
            if path.endswith("complete"):
                raise worker.DependencyUnavailable("terminal_ack_unknown")
            return {"ok": True}
        with patch.object(worker, "runtime_request", side_effect=request) as requests:
            with worker.lease_heartbeats("job-1", "synthetic-owner") as healthy:
                with self.assertRaises(worker.DependencyUnavailable):
                    worker.complete_job("job-1", "synthetic-owner", [])
                with self.assertRaises(worker.RuntimeLeaseLost):
                    healthy()
                with self.assertRaises(worker.RuntimeLeaseLost):
                    worker.complete_job("job-1", "synthetic-owner", [])
        self.assertEqual(requests.call_count, 2)

    def test_initial_uncertainty_never_dispatches_and_does_not_retry_without_confirmation(self):
        dispatched = False
        with patch.object(worker, "heartbeat", side_effect=worker.DependencyUnavailable("private-error")) as renew:
            with self.assertRaisesRegex(worker.DependencyUnavailable, "worker_heartbeat_unconfirmed"):
                with worker.lease_heartbeats("job-1", "synthetic-owner"):
                    dispatched = True
        self.assertFalse(dispatched)
        self.assertEqual(renew.call_count, 1)


if __name__ == "__main__":
    unittest.main()

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / "harness" / "restart_matrix.py"
spec = importlib.util.spec_from_file_location("restart_matrix", SOURCE)
restart_matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restart_matrix)
MOCK_SOURCE = Path(__file__).resolve().parents[1] / "mock-model" / "server.py"
mock_spec = importlib.util.spec_from_file_location("restart_matrix_mock", MOCK_SOURCE)
mock_server = importlib.util.module_from_spec(mock_spec)
mock_spec.loader.exec_module(mock_server)


class RestartMatrixTests(unittest.TestCase):
    def test_build_targets_are_unique_candidates_and_never_local_tags(self):
        tags = {restart_matrix.candidate_image(service, "rm-20260906-a")
                for service in restart_matrix.CANDIDATE_SERVICES}
        self.assertEqual(len(tags), 3)
        self.assertTrue(all(tag.endswith(":restart-matrix-rm-20260906-a") for tag in tags))
        self.assertTrue(all(not tag.endswith(":local") for tag in tags))

    def test_candidate_build_uses_third_overlay_and_never_targets_local(self):
        valid = {
            "name": "centaeris-perf",
            "volumes": {"data": {"name": "centaeris-perf_data"}},
            "networks": {"default": {"name": "centaeris-perf_default"}},
            "services": {
                "api": {"image": restart_matrix.candidate_image("api", "candidate-1"),
                        "environment": {"SSL_CERT_FILE": "/perf-certs/mock-ca.crt"},
                        "volumes": [{"type": "bind", "source": restart_matrix.ROOT.joinpath(
                            "perf/certs/out/isolated/mock-ca.crt").as_posix(),
                            "target": "/perf-certs/mock-ca.crt", "read_only": True}]},
                "runtime": {"environment": {"SSL_CERT_FILE": "/perf-certs/mock-ca.crt"},
                            "volumes": [{"type": "bind", "source": restart_matrix.ROOT.joinpath(
                                "perf/certs/out/isolated/mock-ca.crt").as_posix(),
                                "target": "/perf-certs/mock-ca.crt", "read_only": True}]},
                "worker": {"image": restart_matrix.candidate_image("worker", "candidate-1")},
                "mock-model": {"image": restart_matrix.candidate_image("mock-model", "candidate-1")},
            },
        }
        stack = unittest.mock.Mock(env_file=Path("C:/perf/test.env"))
        with tempfile.TemporaryDirectory() as tmp, patch.object(
                restart_matrix.control, "run", return_value=json.dumps(valid)) as invoke:
            output = Path(tmp)
            restart_matrix.build_candidates(stack, output, "candidate-1")
            overlay = json.loads((output / "candidate.compose.json").read_text())
        self.assertEqual(overlay, restart_matrix.candidate_overlay("candidate-1"))
        calls = [call.args[0] for call in invoke.call_args_list]
        self.assertEqual(len(calls), 4)
        for command in calls:
            self.assertEqual(command.count("-f"), 3)
            self.assertIn("candidate.compose.json", " ".join(command))
        self.assertEqual([command[-2:] for command in calls[1:]],
                         [["build", "api"], ["build", "worker"], ["build", "mock-model"]])
        self.assertNotIn(":local", json.dumps(overlay))

    def test_runtime_resource_constraints_survive_candidate_overlay(self):
        host = {"CpusetCpus": "8-15", "NanoCpus": 2_000_000_000,
                "Memory": 1_073_741_824, "MemorySwap": 2_147_483_648,
                "PidsLimit": 64, "CpuPeriod": 100000, "CpuQuota": 10000}
        constraints = restart_matrix.compose_resource_constraints(host)
        self.assertEqual(constraints, {"cpuset": "8-15", "cpus": "2",
                                       "mem_limit": 1_073_741_824,
                                       "memswap_limit": 2_147_483_648,
                                       "pids_limit": 64})
        overlay = restart_matrix.candidate_overlay("candidate-2", {"api": constraints})
        self.assertEqual(overlay["services"]["api"]["cpuset"], "8-15")
        self.assertEqual(overlay["services"]["api"]["cpus"], "2")

    def test_queued_fixture_releases_with_start_and_preserves_container(self):
        experiment = object.__new__(restart_matrix.Experiment)
        experiment.stack = unittest.mock.Mock()
        experiment.record = unittest.mock.Mock()
        running = {"Id": "worker-id", "Image": "candidate-image",
                   "HostConfig": {"CpusetCpus": "8-15"}, "State": {"Running": True}}
        stopped = {**running, "State": {"Running": False, "ExitCode": 0}}
        experiment.stack.container.return_value = running
        with patch.object(restart_matrix.control, "run", return_value=json.dumps([stopped])):
            facts = experiment.stop_worker_fixture()
        experiment.stack.compose.assert_called_once_with(["stop", "--timeout", "10", "worker"])
        experiment.stack.compose.reset_mock()
        experiment.start_worker_fixture(facts)
        experiment.stack.compose.assert_called_once_with(["start", "worker"])
        self.assertNotIn("up", experiment.stack.compose.call_args.args[0])

    def test_candidate_deployment_consumes_resource_overlay_and_is_explicit(self):
        def container(service):
            return {"Id": f"old-{service}", "Image": f"old-image-{service}",
                    "Config": {"Image": f"old-ref-{service}"},
                    "HostConfig": {"CpusetCpus": "8-15", "NanoCpus": 2_000_000_000,
                                   "Memory": 1_073_741_824, "MemorySwap": 2_147_483_648,
                                   "PidsLimit": 64}}
        stack = unittest.mock.Mock(env_file=Path("C:/perf/test.env"))
        stack.quiet.return_value = True
        after = {name: container(name) for name in restart_matrix.CANDIDATE_SERVICES}
        for name in ("api", "worker"):
            after[name] = {**after[name], "Id": f"new-{name}",
                           "Config": {"Image": restart_matrix.candidate_image(name, "deploy-1")}}
        stack.container.side_effect = [container(name) for name in restart_matrix.CANDIDATE_SERVICES] + \
                                      [after[name] for name in restart_matrix.CANDIDATE_SERVICES]
        rendered = {
            "name": "centaeris-perf", "volumes": {"v": {"name": "centaeris-perf_v"}},
            "networks": {"default": {"name": "centaeris-perf_default"}},
            "services": {
                "api": {"image": restart_matrix.candidate_image("api", "deploy-1"),
                        "cpuset": "8-15", "cpus": "2", "mem_limit": 1_073_741_824,
                        "memswap_limit": 2_147_483_648, "pids_limit": 64,
                        "environment": {"SSL_CERT_FILE": "/perf-certs/mock-ca.crt"},
                        "volumes": [{"type": "bind", "source": restart_matrix.ROOT.joinpath(
                            "perf/certs/out/isolated/mock-ca.crt").as_posix(), "target": "/perf-certs/mock-ca.crt", "read_only": True}]},
                "runtime": {"environment": {"SSL_CERT_FILE": "/perf-certs/mock-ca.crt"},
                            "volumes": [{"type": "bind", "source": restart_matrix.ROOT.joinpath(
                                "perf/certs/out/isolated/mock-ca.crt").as_posix(), "target": "/perf-certs/mock-ca.crt", "read_only": True}]},
                "worker": {"image": restart_matrix.candidate_image("worker", "deploy-1"),
                           "cpuset": "8-15", "cpus": "2", "mem_limit": 1_073_741_824,
                           "memswap_limit": 2_147_483_648, "pids_limit": 64},
                "mock-model": {"image": restart_matrix.candidate_image("mock-model", "deploy-1")},
            }}
        with tempfile.TemporaryDirectory() as tmp, patch.object(
                restart_matrix.control, "run", return_value=json.dumps(rendered)) as invoke:
            restart_matrix.deploy_candidates(stack, Path(tmp), "deploy-1", ("api", "worker"))
        commands = [call.args[0] for call in invoke.call_args_list]
        self.assertEqual([command[-1] for command in commands[1:]], ["api", "worker"])
        self.assertTrue(all("--force-recreate" in command for command in commands[1:]))

    def test_waiting_profile_extracts_canonical_agent_output_ref(self):
        output_ref = {"schema": "task_output_ref_v1", "kind": "agent",
                      "runtimeJobId": "subagent.run:abc", "childSessionId": "session-child",
                      "resultRef": "external_context:result"}
        request = {"input": [
            {"type": "function_call", "call_id": "call-agent", "name": "agent"},
            {"type": "function_call_output", "call_id": "call-agent", "output": json.dumps({
            "status": "ok", "details": {"schema": "agent_tool_result_v1",
                                            "outputRef": output_ref},
        })}]}
        canonical_ref = {"schema": "task_output_ref_v1", "kind": "agent",
                         "runtime_job_id": "subagent.run:abc", "child_session_id": "session-child",
                         "result_ref": "external_context:result"}
        self.assertEqual(mock_server.Handler.waiting_phase(request), ("task_output", canonical_ref))

        completed = {"input": [*request["input"],
            {"type": "function_call", "call_id": "call-output", "name": "task_output"},
            {"type": "function_call_output", "call_id": "call-output", "output": "done"}]}
        self.assertEqual(mock_server.Handler.waiting_phase(completed), ("complete", None))

    def test_mock_stream_mode_is_explicit_true_only(self):
        self.assertFalse(mock_server.Handler.streams({}))
        self.assertFalse(mock_server.Handler.streams({"stream": False}))
        self.assertTrue(mock_server.Handler.streams({"stream": True}))

    def test_waiting_protocol_ignores_unrelated_and_child_markers_are_more_specific(self):
        unrelated = {"input": [
            {"type": "function_call", "call_id": "call-bash", "name": "bash"},
            {"type": "function_call_output", "call_id": "call-bash",
             "output": json.dumps({"outputRef": {"runtimeJobId": "wrong"}})}]}
        self.assertEqual(mock_server.Handler.waiting_phase(unrelated), ("agent", None))
        parent = {"input": [
            {"role": "user", "content": [
                {"type": "input_text", "text": mock_server.WAITING_MARKER}]},
            {"type": "function_call", "call_id": "agent-1", "name": "agent",
             "arguments": json.dumps({"prompt": mock_server.WAITING_CHILD_MARKER})}]}
        self.assertEqual(mock_server.Handler.waiting_role(parent, "perf-waiting"), "parent")
        self.assertEqual(mock_server.Handler.waiting_role({"input": [
            {"role": "user", "content": mock_server.WAITING_MARKER}]}, "perf-waiting"), "parent")
        child = {"input": [{"type": "message", "role": "user", "content": [
            {"type": "input_text", "text": mock_server.WAITING_CHILD_MARKER + ": ready"}]}]}
        self.assertEqual(mock_server.Handler.waiting_role(child, "perf-waiting"), "child")
        self.assertIsNone(mock_server.Handler.waiting_role(unrelated, "perf-waiting"))

    def test_matrix_is_explicit_and_does_not_conflate_sandbox_loss(self):
        self.assertEqual(len(restart_matrix.MATRIX), 12)
        applicable = {(row.state, row.fault) for row in restart_matrix.MATRIX if row.applicable}
        self.assertEqual(applicable, {
            ("queued", "api"), ("queued", "worker"), ("queued", "runtime"),
            ("running", "api"), ("running", "worker"), ("running", "runtime"),
            ("running", "sandbox"),
            ("waiting", "api"), ("waiting", "worker"), ("waiting", "runtime"),
        })
        waiting_sandbox = next(row for row in restart_matrix.MATRIX
                               if row.state == "waiting" and row.fault == "sandbox")
        self.assertIsNone(waiting_sandbox.applicable)

    def test_fault_target_requires_perf_ownership_and_synthetic_run(self):
        restart_matrix.validate_fault_target("centaeris-perf", "run_exp123_01", {"run_exp123_01"})
        for project, run_id in (("centaeris-workspace", "run_exp123_01"),
                                ("centaeris-perf", "run_unrelated")):
            with self.assertRaises(ValueError):
                restart_matrix.validate_fault_target(project, run_id, {"run_exp123_01"})

    def test_terminal_accounting_rejects_duplicates_and_noncompletion(self):
        before = {"committedToolFacts": {"call-a:result-a": 1}, "terminalEvents": 0}
        after = {"committedToolFacts": {"call-a:result-a": 1, "call-b:result-b": 1},
                 "terminalEvents": 1, "status": "completed"}
        self.assertEqual(restart_matrix.verify_conservation(before, after), "completed")
        for invalid in ({**after, "committedToolFacts": {"call-a:result-a": 2}},
                        {**after, "terminalEvents": 2},
                        {**after, "status": "cancelled"}):
            with self.assertRaises(RuntimeError):
                restart_matrix.verify_conservation(before, invalid)

    def test_snapshot_conservation_allows_new_events_but_preserves_old_facts(self):
        call = {"eventId": "event-1", "payload": {
            "schemaVersion": "session.event.v1", "type": "tool_call",
            "payload": {"callId": "call-1", "toolName": "bash"}}}
        result = {"eventId": "event-2", "payload": {
            "schemaVersion": "session.event.v1", "type": "tool_result",
            "payload": {"callId": "call-1", "toolName": "bash", "resultState": "success"}}}
        terminal = {"eventId": "event-3", "payload": {
            "schemaVersion": "session.event.v1", "type": "agent_run_completed", "payload": {}}}
        before = {"events": [call], "run": {"status": "running"}}
        after = {"events": [call, result, terminal], "run": {"status": "completed"}}
        conservation = restart_matrix.snapshot_conservation(before, after)
        self.assertEqual(conservation["preFaultEventsPreserved"], 1)
        self.assertEqual(conservation["toolEventFacts"], {"tool_call:call-1": 1,
                                                          "tool_result:call-1": 1})
        self.assertFalse(conservation["externalSideEffectExactlyOnceEstablished"])
        with self.assertRaises(RuntimeError):
            restart_matrix.snapshot_conservation(before, {**after, "events": [call, call, result, terminal]})

    def test_running_boundary_requires_model_request_and_committed_recovery_checkpoint(self):
        run_id = "agent_run_test"
        base = {
            "run": {"status": "running"},
            "jobs": [{"job_id": f"agent_run.lifecycle:{run_id}", "status": "running"}],
            "events": [{"payload": {"type": "agent_run_execution_started"}}],
            "checkpoints": [],
        }
        self.assertFalse(restart_matrix.boundary_reached(base, "running", run_id))
        with_model = {**base, "events": base["events"] + [
            {"payload": {"type": "model_request_started"}}]}
        self.assertFalse(restart_matrix.boundary_reached(with_model, "running", run_id))
        ready = {**with_model, "checkpoints": [
            {"kind": "recovery", "status": "committed"}]}
        self.assertTrue(restart_matrix.boundary_reached(ready, "running", run_id))
        terminal = {**ready, "run": {"status": "completed"}}
        self.assertFalse(restart_matrix.boundary_reached(terminal, "running", run_id))

    def test_explicit_failure_still_requires_one_terminal_and_preserves_ledger(self):
        old = {"eventId": "event-1", "payload": {"type": "agent_run_started", "payload": {}}}
        failed = {"eventId": "event-2", "payload": {"type": "agent_run_failed", "payload": {}}}
        result = restart_matrix.terminal_integrity(
            {"events": [old]}, {"events": [old, failed], "run": {"status": "failed"}})
        self.assertEqual(result["terminalType"], "agent_run_failed")


if __name__ == "__main__":
    unittest.main()

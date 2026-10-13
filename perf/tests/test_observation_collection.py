import json
import logging
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from perf.harness import collect_observations
from perf.harness.collect_observations import records, observation_services


def protection_record(operation="successful-attempt", metric="apiWrittenBytes"):
    return {"schema": "workspace.perf.v1", "source": "api", "phase": "workspaceProtection",
            "operationId": operation, "metric": metric, "value": 1, "outcome": "ok"}


def container_identities():
    return {name: {"id": character * 64} for name, character in
            (("api", "a"), ("runtime", "b"), ("worker", "c"), ("gc", "d"))}


class ObservationCollectionTests(unittest.TestCase):
    def test_non_observation_logs_are_not_copied_to_evidence(self):
        text = 'raw private log\n{"other":"private"}\n[]\n{"schema":"workspace.perf.v1","phase":"rpc"}\n'
        self.assertEqual(list(records(text)), [{'schema': 'workspace.perf.v1', 'phase': 'rpc'}])

    def test_api_and_gc_must_participate_in_protection_collection(self):
        identities = container_identities()
        self.assertEqual(dict(observation_services({"containers": identities})),
                         {name: row["id"] for name, row in identities.items()})
        for service in ("api", "gc"):
            with self.subTest(service=service):
                with self.assertRaises(ValueError):
                    list(observation_services({"containers": {
                        name: row for name, row in identities.items() if name != service}}))

    def test_protection_records_reject_unbounded_payload_fields(self):
        text = '{"schema":"workspace.perf.v1","phase":"workspaceProtection","markdown":"private"}'
        with self.assertRaises(ValueError):
            list(records(text))

    def test_bad_protection_json_cannot_hide_behind_another_successful_operation(self):
        success = json.dumps(protection_record())
        for broken in (
            '{"schema":"workspace.perf.v1","phase":"workspaceProtection","value":',
            '{"source":"api","phase":"workspaceProtection",',
            json.dumps({**protection_record("bad-schema"), "schema": "workspace.perf.v0"}),
        ):
            for text in (success + "\n" + broken, broken + "\n" + success):
                with self.subTest(text=text), self.assertRaises(ValueError):
                    list(records(text))

    def test_missing_observations_fail_with_actual_python_warning_formats(self):
        warning = logging.LogRecord("app_core.workspace_observations", logging.WARNING, "", 0,
                                    "Workspace protection observation unavailable: %s",
                                    ("eventSink",), None)
        # Current API/GC use lastResort's message-only handler. Also exercise
        # Python's standard basicConfig format without scraping arbitrary text.
        for formatter in (logging.Formatter(), logging.Formatter(logging.BASIC_FORMAT)):
            text = json.dumps(protection_record()) + "\n" + formatter.format(warning)
            with self.subTest(format=formatter._fmt), self.assertRaises(ValueError):
                list(records(text))

    def test_unrelated_logs_that_discuss_observations_remain_allowed(self):
        lines = [
            'normal malformed {json',
            'request mentions {"schema":"workspace.perf.v1","phase":"workspaceProtection"',
            'INFO:other:Workspace protection observation unavailable: eventSink',
            'example: Workspace protection observation unavailable: eventSink',
            json.dumps({"message": 'Workspace protection observation unavailable: eventSink'}),
            json.dumps({"message": '{"schema":"workspace.perf.v1","phase":"workspaceProtection"'})[:-1],
            json.dumps(protection_record()),
        ]
        self.assertEqual(list(records("\n".join(lines))), [protection_record()])

    def run_collection(self, directory, identities, logs):
        output = directory / "centaeris-perf-evidence" / "synthetic-collection"
        output.mkdir(parents=True)
        (output / "manifest.json").write_text(json.dumps({"containers": identities}), encoding="utf-8")

        def docker_logs(command, **_options):
            self.assertEqual(command[:6], ["docker", "logs", "--since", "start", "--until", "end"])
            return SimpleNamespace(stdout="ordinary service log\n", stderr=logs[command[6]])

        with patch.object(collect_observations, "ROOT", directory / "perf"), \
                patch("sys.argv", ["collect", "--experiment-id", "synthetic-collection",
                                   "--since", "start", "--until", "end"]), \
                patch.object(collect_observations.subprocess, "run", side_effect=docker_logs) as run:
            collect_observations.main()
        return output, run

    def test_collection_reads_gc_logs_from_the_manifest_container_identity(self):
        identities = container_identities()
        logs = {row["id"]: json.dumps(protection_record(service,
                "gcElapsedMs" if service == "gc" else "apiWrittenBytes"))
                for service, row in identities.items()}
        with tempfile.TemporaryDirectory() as directory:
            output, run = self.run_collection(Path(directory), identities, logs)
            self.assertEqual({call.args[0][6] for call in run.call_args_list}, set(logs))
            self.assertEqual(json.loads((output / "gc-observations.jsonl").read_text(encoding="utf-8")),
                             protection_record("gc", "gcElapsedMs"))

    def test_invalid_collection_does_not_leave_a_success_only_file(self):
        identities = container_identities()
        logs = {row["id"]: json.dumps(protection_record()) for row in identities.values()}
        logs[identities["worker"]["id"]] += '\n{"phase":"workspaceProtection",'
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            with self.assertRaises(ValueError):
                self.run_collection(directory, identities, logs)
            self.assertFalse((directory / "centaeris-perf-evidence" / "synthetic-collection" /
                              "worker-observations.jsonl").exists())

    def test_gc_with_no_observations_fails_even_when_other_services_succeed(self):
        identities = container_identities()
        logs = {row["id"]: json.dumps(protection_record()) for row in identities.values()}
        logs[identities["gc"]["id"]] = "GC completed without a protection sample"
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(SystemExit, "gc: no observations"):
                self.run_collection(Path(directory), identities, logs)

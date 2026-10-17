"""Optional, independent observer of public Workspace output.

Only authentication and the bound run's GET event stream are permitted. No
provider, Runtime, Docker, verifier, or execution authority belongs here.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import stat
import tempfile
import time

from workspace_client import WorkspaceClient, WorkspaceError, _segment
from workspace_monitor import JobProjection, redact_output

OUTPUT_SCHEMA = "centaeris.workspaceBenchmarkOutput.v1"
ENTRY_LIMIT = 40
BYTE_LIMIT = 64 * 1024
ENTRY_BYTES = 8 * 1024


def identity(value):
    if not isinstance(value, str) or not value or len(value) > 256:
        raise ValueError("outputIdentityInvalid")
    return value


def clipped(value):
    # Sanitize before truncation so a credential spanning the cutoff is removed.
    if not isinstance(value, str):
        return "Body not available in this output view."
    return redact_output(value).encode("utf-8")[-ENTRY_BYTES:].decode("utf-8", errors="ignore")


class OutputWindow:
    def __init__(self, trial_id, session_id, run_id):
        self.trial_id, self.session_id, self.run_id = map(identity, (trial_id, session_id, run_id))
        self.entries = []
        self.sequence = 0
        self.cursor = "0-0"
        self.live = None
        self.barrier = 0
        self.truncated = False
        self.terminal = False
        self.error = None

    def _entry(self, key, kind, value):
        rendered = clipped(value)
        self.truncated |= isinstance(value, str) and len(value.encode("utf-8")) > ENTRY_BYTES
        return {"id": identity(key), "kind": kind, "text": rendered}

    def _bound(self):
        while len(self.entries) > ENTRY_LIMIT - int(self.live is not None) or sum(
                len(e["text"].encode("utf-8")) for e in self.entries) + (
                len(self.live["entry"]["text"].encode("utf-8")) if self.live else 0) > BYTE_LIMIT:
            self.entries.pop(0)
            self.truncated = True

    def apply(self, item):
        if (not isinstance(item, dict) or item.get("schema") != "session.stream.item.v1"
                or item.get("agentRunId") != self.run_id):
            raise ValueError("outputIdentityInvalid")
        if item.get("kind") == "live":
            required = {"schema", "kind", "agentRunId", "messageId", "turnId", "afterSequence", "revision", "text"}
            if set(item) - (required | {"reasoning"}) or required - set(item):
                raise ValueError("outputLiveInvalid")
            message, turn = identity(item["messageId"]), identity(item["turnId"])
            revision, after = item["revision"], item["afterSequence"]
            if type(revision) is not int or revision < 1 or type(after) is not int or after < 0 or not isinstance(item["text"], str):
                raise ValueError("outputLiveInvalid")
            if self.terminal or after < self.barrier:
                return
            if self.live and self.live["message"] == message and revision <= self.live["revision"]:
                return
            self.live = {"message": message, "turn": turn, "revision": revision,
                "entry": self._entry("live:" + message, "Assistant · streaming", item["text"])}
            self._bound()
            return
        if item.get("kind") != "committed" or set(item) != {"schema", "kind", "agentRunId", "sourceSequence", "event"}:
            raise ValueError("outputEventInvalid")
        event, sequence = item["event"], item["sourceSequence"]
        if (type(sequence) is not int or sequence < 1 or not isinstance(event, dict)
                or event.get("sequence") != sequence or event.get("sessionId") != self.session_id
                or event.get("agentRunId") != self.run_id or event.get("schemaVersion") != "session.event.v1"
                or event.get("eventVersion") != 1 or not isinstance(event.get("payload"), dict)):
            raise ValueError("outputEventInvalid")
        key, kind, payload = identity(event.get("eventId")), event.get("type"), event["payload"]
        if sequence <= self.sequence:
            return  # Committed cursor is the deduplication boundary, not an unbounded ID set.
        entry = None
        if kind == "assistant_message":
            entry = self._entry(key, "Assistant", payload.get("text"))
        elif kind == "tool_call":
            tool = identity(payload.get("toolName"))
            entry = self._entry(key, "Tool · " + tool, json.dumps(payload.get("normalizedInput"), ensure_ascii=False))
        elif kind == "tool_result":
            tool = identity(payload.get("toolName"))
            entry = self._entry(key, "Result · " + tool, payload.get("modelContent"))
        elif kind == "phase_event":
            entry = self._entry(key, "Phase", payload.get("message"))
        if kind in {"assistant_message", "phase_event"}:
            self.live = None
            self.barrier = sequence
        if kind in {"agent_run_completed", "agent_run_failed", "agent_run_interrupted"}:
            self.terminal, self.live = True, None
        self.sequence = sequence
        if entry:
            self.entries.append(entry)
        self._bound()

    def snapshot(self):
        return {"trialId": self.trial_id, "sessionId": self.session_id, "agentRunId": self.run_id,
            "sourceSequence": self.sequence, "entries": self.entries + ([self.live["entry"]] if self.live else []),
            "truncated": self.truncated, "error": self.error}


class OutputClient(WorkspaceClient):
    def __init__(self, origin, window, changed=lambda: None):
        super().__init__(origin, request_timeout=3, max_event_bytes=256 * 1024)
        self.window, self.changed = window, changed

    def _request(self, method, path, payload=None, *, headers=None):
        events = f"/api/sessions/{_segment(self.window.session_id)}/agent-runs/{_segment(self.window.run_id)}/events"
        if (method, path) not in {("GET", "/api/csrf"), ("POST", "/api/login"), ("GET", events)}:
            raise WorkspaceError("output_request_forbidden")
        return super()._request(method, path, payload, headers=headers)

    def _apply_stream_item(self, result, item, cursor, session_id, agent_run_id):
        if (session_id, agent_run_id) != (self.window.session_id, self.window.run_id):
            raise WorkspaceError("output_identity_invalid")
        next_result = dict(result)
        if isinstance(item, dict) and item.get("kind") == "committed" and type(item.get("sourceSequence")) is int and item["sourceSequence"] > result["sourceSequence"]:
            super()._apply_stream_item(next_result, item, cursor, session_id, agent_run_id)
        try:
            self.window.apply(item)
        except ValueError:
            raise WorkspaceError("output_event_invalid") from None
        result.update(next_result)
        self.window.cursor = result["cursor"]
        self.changed()


def publish(root, job_id, windows, *, error=None):
    value = {"schema": OUTPUT_SCHEMA, "jobId": job_id,
        "observedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "error": error, "trials": [window.snapshot() for window in windows]}
    if len(value["trials"]) > 2:
        raise ValueError("outputSlotLimit")
    # Atomic replacement, bounded single file. The observer is the only writer.
    descriptor, incoming = tempfile.mkstemp(prefix=".output-", dir=root)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(value, handle, ensure_ascii=True, allow_nan=False)
        os.chmod(incoming, 0o644)
        os.replace(incoming, Path(root) / "output.json")
    finally:
        if os.path.exists(incoming): os.unlink(incoming)


def observe(client, seed):
    window = client.window
    try:
        if not client.csrf:
            client.login(seed["email"], seed["password"])
        result = client.read_run_events(window.session_id, window.run_id, window.cursor,
            after_sequence=window.sequence)
        window.cursor = result["cursor"]
        window.error = None
    except (WorkspaceError, OSError, ValueError):
        # Neither arbitrary exception text nor server bodies enter the public file.
        window.error = "outputUnavailable"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--job-dir", type=Path, required=True)
    parser.add_argument("--seed-path", type=Path, required=True)
    parser.add_argument("--api-origin", required=True)
    args = parser.parse_args()
    if args.seed_path.is_symlink() or args.seed_path.resolve().is_relative_to(args.job_dir.resolve()):
        parser.error("seed must be a private file outside the public projection")
    info = args.seed_path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > 65536 or (os.name != "nt" and info.st_mode & 0o077):
        parser.error("seed permissions or size invalid")
    seed = json.loads(args.seed_path.read_text(encoding="utf-8"))
    if not all(isinstance(seed.get(k), str) and seed[k] for k in ("email", "password")):
        parser.error("seed account invalid")
    projection, clients = JobProjection(args.job_dir), {}
    with ThreadPoolExecutor(max_workers=2) as executor:
        while True:
            state = projection.snapshot()
            if state["job"] and state["monitorStatus"] != "invalid":
                latest = {}
                for trial in state["trials"]:
                    if trial.get("slot") in (1, 2): latest[trial["slot"]] = trial
                selected = [t for t in latest.values() if t.get("sessionId") and t.get("agentRunId")]
                keys = {(state["job"]["jobId"], t["trialId"], t["sessionId"], t["agentRunId"]) for t in selected}
                clients = {k: v for k, v in clients.items() if k in keys}
                for key in keys:
                    if key not in clients:
                        clients[key] = OutputClient(args.api_origin, OutputWindow(*key[1:]))
                futures = [executor.submit(observe, client, seed) for client in clients.values() if not client.window.terminal]
                for future in futures: future.result()
                publish(args.job_dir, state["job"]["jobId"], [c.window for c in clients.values()])
            time.sleep(2)


if __name__ == "__main__": main()

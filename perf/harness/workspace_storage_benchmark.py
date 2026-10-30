"""Measure workspace API/storage/PG costs on one explicit source revision.

This is a storage-special benchmark, not a model, helper, network or k6 workload.
It uses the repository's existing authenticated HTTP fixtures and a disposable
Django test database. Runtime root selection is supplied by the existing GC
fixture seam; Core root-selection and helper-capture costs are not measured.
Run the same script/interpreter/host for baseline and candidate comparisons.
"""
import argparse
from contextlib import contextmanager, redirect_stdout
from dataclasses import dataclass
from datetime import timedelta
import hashlib
import json
import os
from math import ceil
import statistics
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import uuid

SCENARIOS = ("firstsave", "unchanged", "onefilechanged", "versions", "allchanged", "smallfiles", "restore", "gc")
TABLES = ("WorkspaceSnapshotObject", "WorkspaceSnapshotUpload", "WorkspaceFileObject",
          "WorkspaceFileReservation", "WorkspaceSnapshotFileReference", "StorageCleanupRetry")
IO_BYTES = 64 * 1024
MAX_ITERATION_BYTES = 1536 * 1024**2


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def sha256(content):
    return "sha256:" + hashlib.sha256(content).hexdigest()


@dataclass(frozen=True)
class Entry:
    path: str
    content: bytes
    digest: str

    @classmethod
    def make(cls, index, size, kind, version=0):
        seed = f"centaeris-storage:{index}:{version}".encode("ascii")
        if kind == "random":
            content = hashlib.shake_256(seed).digest(size)
        else:
            line = seed + b": deterministic workspace text line.\n"
            content = (line * ((size + len(line) - 1) // len(line)))[:size]
        return cls(f"files/{index:06d}.bin", content, sha256(content))

    def descriptor(self):
        return {"path": self.path, "sizeBytes": len(self.content), "sha256": self.digest, "executable": False}


def document(entries):
    return {"schema": "workspace.snapshot.v1", "files": [entry.descriptor() for entry in entries]}


def rss_high_water():
    try:
        import resource
    except ImportError:
        return None
    value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return int(value if sys.platform == "darwin" else value * 1024)


def disk_usage(root):
    sizes = {"snapshotBytes": 0, "fileObjectBytes": 0, "otherBytes": 0, "fileCount": 0}
    allocated = 0
    have_allocated = True
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        value = path.stat()
        sizes["fileCount"] += 1
        key = "snapshotBytes" if path.suffix == ".snapshot" else "fileObjectBytes" if path.suffix == ".file" else "otherBytes"
        sizes[key] += value.st_size
        if hasattr(value, "st_blocks"):
            allocated += value.st_blocks * 512
        else:
            have_allocated = False
    sizes["logicalBytes"] = sizes["snapshotBytes"] + sizes["fileObjectBytes"] + sizes["otherBytes"]
    sizes["allocatedBytes"] = allocated if have_allocated else None
    return sizes


def response_chunks(response):
    if not response.streaming:
        yield response.content
        return
    if not response.is_async:
        yield from response.streaming_content
        return
    # Django's download view is async. Keep its fixture adapter streaming and
    # bounded rather than materializing a complete baseline frame in memory.
    from asgiref.sync import async_to_sync
    from queue import Full, Queue
    from threading import Event, Thread
    pending, stopped = Queue(maxsize=2), Event()
    def send(value):
        while not stopped.is_set():
            try:
                pending.put(value, timeout=0.1)
                return
            except Full:
                pass
    async def consume():
        try:
            async for chunk in response.streaming_content:
                if stopped.is_set():
                    break
                send(chunk)
        except Exception as error:
            send(error)
        finally:
            send(None)
    worker = Thread(target=lambda: async_to_sync(consume)(), daemon=True)
    worker.start()
    try:
        while True:
            value = pending.get()
            if value is None:
                break
            if isinstance(value, Exception):
                raise value
            yield value
    finally:
        stopped.set()
        worker.join(timeout=5)
        if worker.is_alive():
            raise RuntimeError("benchmark streaming reader did not finish")


class StreamReader:
    def __init__(self, chunks):
        self.chunks = iter(chunks)
        self.pending = b""
        self.bytes = 0
        self.digest = hashlib.sha256()

    def read(self, count):
        values = []
        remaining = count
        while remaining:
            if not self.pending:
                self.pending = next(self.chunks, b"")
                if not self.pending:
                    break
            part, self.pending = self.pending[:remaining], self.pending[remaining:]
            values.append(part)
            remaining -= len(part)
        value = b"".join(values)
        self.bytes += len(value)
        self.digest.update(value)
        return value

    def exact(self, count):
        value = self.read(count)
        if len(value) != count:
            raise AssertionError("download was truncated")
        return value


def build_case(options, results):
    from django.apps import apps
    from django.conf import settings
    from django.db import connection
    from django.db.backends.utils import CursorWrapper
    from django.test import TransactionTestCase, override_settings
    from django.utils import timezone
    from app_core.agent_run_authorization_factory import create_agent_run_authorization
    from app_core.models import AgentRun, Session, WorkspaceSnapshotObject, WorkspaceSnapshotUpload
    from app_core.test_execution_publication_fencing import ExecutionPublicationFixture
    from app_core.workspace_snapshot_gc import collect_workspace_snapshot_gc
    if options.encoding == "fileManifestV1":
        from app_core.test_workspace_file_http import WorkspaceFileHttpFixture
        fixture = WorkspaceFileHttpFixture
    else:
        fixture = ExecutionPublicationFixture

    class StorageCase(fixture, TransactionTestCase):
        serialized_rollback = True

        def setUp(self):
            budget_setting = "SANDBOX_DATA_TMPFS_BYTES" if options.encoding == "frameV1" else "SANDBOX_WORKSPACE_BYTES"
            self.enterContext(override_settings(**{budget_setting: options.workspace_budget_bytes}))
            super().setUp()
            self.records = []
            self.post_bytes = 0
            self.file_body_bytes = 0
            self.download_bytes = 0
            self.created_entries = {}
            self.saved = []
            self.previous_document = None
            self.current_entries = []
            self.jobs = {self.job_id}
            self.addCleanup(self.clear_jobs)
            self.addCleanup(self.release_payloads)
            self.enterContext(patch("app_core.workspace_snapshot_roots.current_witness", return_value=(0, "sha256:" + "a" * 64)))
            self.root_plan = {"schema": "runtime.workspace_snapshot.roots.v1", "sessionId": self.session.pk,
                "sessionSequence": 0, "authorityFingerprint": "sha256:" + "a" * 64,
                "roots": [], "protectedAgentRunIds": []}
            self.enterContext(patch("app_core.workspace_snapshot_reachability_gc.request_workspace_snapshot_roots",
                side_effect=lambda session: self.root_plan | {"sessionId": session.pk}))

        def release_payloads(self):
            # unittest keeps case instances alive for the result summary. File
            # bodies must not accumulate across diagnostic samples.
            self.current_entries = []
            self.previous_document = None
            self.created_entries.clear()
            self.saved.clear()

        def clear_jobs(self):
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=ANY(%s)", [list(self.jobs)])

        @contextmanager
        def phase(self, name):
            counts = {"sqlCalls": 0, "rootMetadataSelects": 0, "referenceSelects": 0, "referenceRowsReturned": 0}
            execute, executemany = CursorWrapper.execute, CursorWrapper.executemany
            def count(sql):
                counts["sqlCalls"] += 1
                text = str(sql).lstrip().lower()
                if text.startswith("select") and "app_core_sessionworkspacepreservation" in text:
                    counts["rootMetadataSelects"] += 1
            def one(cursor, sql, params=None):
                count(sql)
                result = execute(cursor, sql, params)
                text = str(sql).lstrip().lower()
                if text.startswith("select") and "app_core_workspacesnapshotfilereference" in text:
                    counts["referenceSelects"] += 1
                    counts["referenceRowsReturned"] += max(0, cursor.rowcount)
                return result
            def many(cursor, sql, params):
                count(sql)
                return executemany(cursor, sql, params)
            start, cpu = time.perf_counter(), time.process_time()
            wire, bodies, downloads = self.post_bytes, self.file_body_bytes, self.download_bytes
            with patch.object(CursorWrapper, "execute", one), patch.object(CursorWrapper, "executemany", many):
                yield
            self.records.append({"phase": name, "wallMs": (time.perf_counter() - start) * 1000,
                "processCpuMs": (time.process_time() - cpu) * 1000, **counts,
                "requestBodyBytes": self.post_bytes - wire,
                "uploadedFileBodyBytes": self.file_body_bytes - bodies,
                "downloadedStorageBytes": self.download_bytes - downloads,
                "processRssHighWaterBytes": rss_high_water()})

        def post(self, endpoint, body, *, binary=False):
            encoded = body if binary else canonical(body)
            self.post_bytes += len(encoded)
            response = self.client.post("/internal/agent-runs/" + endpoint, data=encoded,
                content_type="application/octet-stream" if binary else "application/json",
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
            if response.status_code not in (200, 201):
                self.fail(f"{endpoint} returned {response.status_code}: {response.content[:1000]!r}")
            if response.streaming:
                self.addCleanup(response.close)
            return response

        def identity(self, schema, **values):
            return {"schema": schema, "jobId": self.job_id, "leaseOwner": self.owner,
                "agentRunId": self.run.pk, "authorizationDigest": self.authorization.digest, **values}

        def advance(self):
            with connection.cursor() as cursor:
                cursor.execute("UPDATE runtime.runtime_jobs SET status='succeeded',lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=%s", [self.job_id])
            AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
            self.session.refresh_from_db()
            self.run = AgentRun.objects.create(workspace=self.run.workspace, session=self.session,
                user=self.run.user, modelConfig=self.run.modelConfig, prompt="next storage sample", status="running")
            self.authorization = create_agent_run_authorization(self.run, image_digest="sha256:" + "a" * 64)
            self.job_id = "agent_run.lifecycle:" + self.run.pk
            self.owner = "worker:storage-benchmark"
            self.jobs.add(self.job_id)
            with connection.cursor() as cursor:
                cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,lease_expires_at_ms,"
                    "idempotency_key,session_id,payload_ref) VALUES(%s,'agent_run.lifecycle','running',%s,"
                    "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                    [self.job_id, self.owner, self.job_id + ":" + self.authorization.digest,
                     self.session.pk, "record:agent_run:" + self.run.pk])

        def save(self, entries, name):
            with self.phase(name + ".manifest"):
                value = document(entries)
            if value == self.previous_document:
                with self.phase(name + ".unchanged"):
                    self.assertEqual(value, self.previous_document)
                return
            if self.saved:
                self.advance()
            self._lease(self.owner)
            self.current_entries = entries
            for entry in entries:
                self.created_entries[entry.digest] = len(entry.content)
            with self.phase(name + ".prepare"):
                manifest = canonical(value)
                if options.encoding == "frameV1":
                    snapshot = len(manifest).to_bytes(4, "big") + manifest + b"".join(entry.content for entry in entries)
                else:
                    snapshot = manifest
                descriptor = {"snapshotSha256": sha256(snapshot), "snapshotSizeBytes": len(snapshot),
                    "expandedSizeBytes": sum(len(entry.content) for entry in entries), "fileCount": len(entries)}
            if options.encoding == "fileManifestV1":
                by_hash = {entry.digest: entry for entry in entries}
                with self.phase(name + ".missing"):
                    response = self.post("workspace-files/missing", self.identity("runtime.workspace_files.missing.v1",
                        files=[{"sha256": key, "sizeBytes": len(entry.content)} for key, entry in by_hash.items()]))
                    missing = response.json()["missing"]
                with self.phase(name + ".files"):
                    for key in missing:
                        entry = by_hash[key]
                        metadata = canonical(self.identity("runtime.workspace_files.upload.v1", sha256=key, sizeBytes=len(entry.content)))
                        body = len(metadata).to_bytes(4, "big") + metadata + entry.content
                        self.file_body_bytes += len(entry.content)
                        self.post("workspace-files/upload", body, binary=True)
                schema = "runtime.session_workspace.commit.v2"
            else:
                schema = "runtime.session_workspace.commit.v1"
            metadata = self.identity(schema, **descriptor)
            if options.encoding == "fileManifestV1":
                metadata["snapshotEncoding"] = options.encoding
            with self.phase(name + ".requestEncode"):
                encoded = canonical(metadata)
                body = len(encoded).to_bytes(4, "big") + encoded + snapshot
            with self.phase(name + ".commit"):
                if options.encoding == "frameV1":
                    self.file_body_bytes += sum(len(entry.content) for entry in entries)
                self.post("session-workspace/commit", body, binary=True)
            self.session.refresh_from_db()
            self.saved.append({"key": self.session.workspaceStorageKey, "descriptor": descriptor,
                "runId": self.run.pk, "authorizationDigest": self.authorization.digest})
            self.previous_document = value

        def storage(self):
            by_table = {}
            for name in TABLES:
                model = apps.all_models["app_core"].get(name.lower())
                if model is None:
                    continue
                with connection.cursor() as cursor:
                    cursor.execute("SELECT pg_relation_size(%s),pg_indexes_size(%s),pg_total_relation_size(%s)", [model._meta.db_table] * 3)
                    heap, indexes, total = cursor.fetchone()
                by_table[name] = {"heapBytes": heap, "indexBytes": indexes, "totalBytes": total,
                                 "rows": model.objects.count()}
            return {"files": disk_usage(self.storage_root), "postgres": by_table,
                    "postgresTotalBytes": sum(row["totalBytes"] for row in by_table.values())}

        def restore(self, name):
            self.advance()
            snapshot = self.saved[-1]["descriptor"]
            with tempfile.TemporaryDirectory(prefix="workspace-storage-restore-") as directory, self.phase(name):
                response = self.post("session-workspace/download", self.identity("runtime.session_workspace.download.v1"))
                reader = StreamReader(response_chunks(response))
                if options.encoding == "frameV1":
                    count = int.from_bytes(reader.exact(4), "big")
                    restored_manifest = json.loads(reader.exact(count))
                else:
                    restored_manifest = json.loads(reader.exact(snapshot["snapshotSizeBytes"]))
                self.assertEqual(restored_manifest, document(self.current_entries))
                for entry in self.current_entries:
                    target = Path(directory) / entry.path
                    target.parent.mkdir(parents=True, exist_ok=True)
                    if options.encoding == "fileManifestV1":
                        response = self.post("workspace-files/download", self.identity("runtime.workspace_files.download.v1",
                            checkpointId=None, snapshot=snapshot, sha256=entry.digest, sizeBytes=len(entry.content)))
                        body_reader = StreamReader(response_chunks(response))
                    else:
                        body_reader = reader
                    remaining, actual = len(entry.content), hashlib.sha256()
                    with target.open("wb") as handle:
                        while remaining:
                            chunk = body_reader.exact(min(remaining, IO_BYTES))
                            actual.update(chunk)
                            handle.write(chunk)
                            remaining -= len(chunk)
                    self.assertEqual("sha256:" + actual.hexdigest(), entry.digest)
                    if options.encoding == "fileManifestV1":
                        self.assertEqual(body_reader.read(1), b"")
                        self.download_bytes += body_reader.bytes
                self.assertEqual(reader.read(1), b"")
                self.download_bytes += reader.bytes
                self.assertEqual("sha256:" + reader.digest.hexdigest(), snapshot["snapshotSha256"])
                self.restore_file_bytes = sum(len(entry.content) for entry in self.current_entries)

        def collect(self):
            for job in self.jobs:
                with connection.cursor() as cursor:
                    cursor.execute("UPDATE runtime.runtime_jobs SET status='succeeded',lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=%s", [job])
            AgentRun.objects.filter(session=self.session).update(status="completed")
            first = self.saved[0]
            self.root_plan["roots"] = [{"objectRef": first["key"], "agentRunId": first["runId"],
                "authorizationDigest": first["authorizationDigest"], "checkpointId": "benchmark-protected-root",
                **first["descriptor"]}]
            now = timezone.now()
            old = now - timedelta(days=35)
            WorkspaceSnapshotObject.objects.update(createdAt=old, updatedAt=old)
            WorkspaceSnapshotUpload.objects.update(createdAt=old, updatedAt=old)
            for path in self.storage_root.rglob("*"):
                if path.is_file():
                    os.utime(path, (old.timestamp(), old.timestamp()))
            before = self.storage()
            with self.phase("gc"):
                report = collect_workspace_snapshot_gc(now - timedelta(days=30), False)
            self.assertEqual(report.failures, [])
            after = self.storage()
            self.assertTrue((self.storage_root / first["key"]).is_file())
            self.assertTrue((self.storage_root / self.saved[-1]["key"]).is_file())
            return {"before": before, "after": after, "reclaimedBytes": before["files"]["logicalBytes"] - after["files"]["logicalBytes"],
                "cleanedObjects": len(report.cleaned), "blockedObjects": len(report.blocked), "failures": report.failures,
                "protectedSnapshots": 2}

        def test_measure(self):
            count = options.small_files if self.scenario == "smallfiles" else options.files
            size = options.small_file_bytes if self.scenario == "smallfiles" else options.file_bytes
            generated = time.perf_counter()
            entries = [Entry.make(index, size, self.kind) for index in range(count)]
            generation_ms = (time.perf_counter() - generated) * 1000
            before = self.storage()
            self.save(entries, "save0")
            gc_result = None
            if self.scenario == "unchanged":
                self.save(entries, "unchanged")
            elif self.scenario in ("onefilechanged", "allchanged"):
                changed = list(entries)
                for index in range(count if self.scenario == "allchanged" else 1):
                    changed[index] = Entry.make(index, size, self.kind, version=1)
                self.save(changed, "save1")
            elif self.scenario in ("versions", "gc"):
                for version in range(1, options.versions):
                    entries = list(entries)
                    entries[0] = Entry.make(0, size, self.kind, version=version)
                    self.save(entries, "save" + str(version))
            if self.scenario in ("restore", "gc"):
                self.restore("restore")
            if self.scenario == "gc":
                gc_result = self.collect()
            after = self.storage()
            unique = sum(self.created_entries.values())
            results.append({"scenario": self.scenario, "kind": self.kind, "sample": self.sample,
                "files": count, "fileBytes": size, "versionsSaved": len(self.saved),
                "fixtureGenerationMs": generation_ms, "phases": self.records,
                "uploadedFileBodyBytes": self.file_body_bytes, "requestBodyBytes": self.post_bytes,
                "uniqueFileBodyBytes": unique,
                "theoreticalFullSnapshotFileBodyBytes": sum(saved["descriptor"]["expandedSizeBytes"] for saved in self.saved),
                "downloadedStorageBytes": self.download_bytes,
                "storageBefore": before, "storageAfter": after,
                "postgresGrowthBytes": after["postgresTotalBytes"] - before["postgresTotalBytes"],
                "gc": gc_result, "restoredFileBytes": getattr(self, "restore_file_bytes", None),
                "unchangedCapture": "descriptor equality only; helper verification is outside this benchmark"})

    return StorageCase


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--encoding", choices=("frameV1", "fileManifestV1"), required=True)
    parser.add_argument("--source-sha", help="explicit 40-character SHA for an exported source tree")
    parser.add_argument("--source-dirty", action=argparse.BooleanOptionalAction, default=None)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--scenario", action="append", choices=SCENARIOS)
    parser.add_argument("--kind", action="append", choices=("random", "text"))
    parser.add_argument("--files", type=int, default=100)
    parser.add_argument("--file-bytes", type=int, default=1024**2)
    parser.add_argument("--versions", type=int, default=10)
    parser.add_argument("--workspace-budget-bytes", type=int, default=512 * 1024**2)
    parser.add_argument("--small-files", type=int, default=1000)
    parser.add_argument("--small-file-bytes", type=int, default=1024)
    parser.add_argument("--db-host", default="127.0.0.1")
    parser.add_argument("--db-port", default="55432")
    parser.add_argument("--db-user", default="postgres")
    parser.add_argument("--smoke", action="store_true", help="tiny correctness run, not a performance result")
    result = parser.parse_args()
    result.source_root = result.source_root.resolve()
    if result.smoke:
        result.samples, result.files, result.file_bytes, result.versions = 1, 3, 4096, 3
        result.small_files, result.small_file_bytes = 8, 128
    if min(result.samples, result.files, result.file_bytes, result.versions, result.small_files, result.small_file_bytes) < 1:
        parser.error("sample, file and version values must be positive")
    if result.versions < 2:
        parser.error("at least two versions are required for the GC scenario")
    # Includes one temporary full upload alongside all stored baseline versions.
    if max(result.files * result.file_bytes * (result.versions + 2), result.small_files * result.small_file_bytes * 3) > MAX_ITERATION_BYTES:
        parser.error("configured workload exceeds the 1.5 GiB iteration storage budget")
    if result.source_sha is not None:
        if len(result.source_sha) != 40 or any(char not in "0123456789abcdef" for char in result.source_sha):
            parser.error("source-sha must contain exactly 40 lowercase hexadecimal characters")
        if result.source_dirty is None:
            parser.error("explicit source-sha requires --source-dirty or --no-source-dirty")
    if not (result.source_root / "packages/api/manage.py").is_file():
        parser.error("source-root must name a Centaeris checkout")
    return result


def run(options):
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(options.source_root / "packages/api"))
    os.environ["DJANGO_SETTINGS_MODULE"] = "api.test_settings"
    os.environ["TEST_POSTGRES_DB"] = "workspace_storage_" + uuid.uuid4().hex
    os.environ["TEST_POSTGRES_USER"] = options.db_user
    os.environ["TEST_POSTGRES_HOST"] = options.db_host
    os.environ["TEST_POSTGRES_PORT"] = options.db_port
    os.environ["TEST_POSTGRES_PASSWORD"] = os.environ.get("WORKSPACE_BENCHMARK_DB_PASSWORD", "")
    import django
    django.setup()
    from django.test.runner import DiscoverRunner
    output = []
    cls = build_case(options, output)
    cases = []
    for scenario in options.scenario or SCENARIOS:
        for kind in options.kind or ("random", "text"):
            for sample in range(options.samples):
                case = cls("test_measure")
                case.scenario, case.kind, case.sample = scenario, kind, sample
                cases.append(case)
    runner = DiscoverRunner(verbosity=1, interactive=False)
    with redirect_stdout(sys.stderr):
        runner.setup_test_environment()
        old_config = None
        try:
            old_config = runner.setup_databases()
            test_result = runner.run_suite(unittest.TestSuite(cases))
        finally:
            if old_config is not None:
                runner.teardown_databases(old_config)
            else:
                # A failed migration can leave the database created by setup.
                # Only this invocation's generated name is eligible for cleanup.
                from django.db import connection, connections
                owned_name = "test_" + os.environ["TEST_POSTGRES_DB"]
                connections.close_all()
                with connection._nodb_cursor() as cursor:
                    cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=%s)", [owned_name])
                    if cursor.fetchone()[0]:
                        cursor.execute("DROP DATABASE " + connection.ops.quote_name(owned_name))
            runner.teardown_test_environment()
    if options.source_sha is not None:
        sha, dirty, identity = options.source_sha, options.source_dirty, "explicit"
    else:
        sha = subprocess.check_output(["git", "-C", str(options.source_root), "rev-parse", "HEAD"], text=True).strip()
        dirty = bool(subprocess.check_output(["git", "-C", str(options.source_root), "status", "--porcelain"], text=True).strip())
        identity = "git"
    groups = {}
    for sample in output:
        for row in sample["phases"]:
            groups.setdefault((sample["scenario"], sample["kind"], row["phase"]), []).append(row["wallMs"])
    summary = [{"scenario": key[0], "kind": key[1], "phase": key[2], "samples": len(values),
        "p50WallMs": statistics.median(values), "sampleP95WallMs": sorted(values)[ceil(0.95 * len(values)) - 1],
        "maxWallMs": max(values)} for key, values in groups.items()]
    report = {"schema": "workspace.storage_benchmark.v1", "sourceSha": sha, "sourceDirty": dirty, "sourceIdentity": identity,
        "encoding": options.encoding, "smoke": options.smoke, "workspaceBudgetBytes": options.workspace_budget_bytes,
        "gcScalingCaveat": "Session-wide recent/reference sets may be reread per batch; query counts alone do not establish linear complexity",
        "summary": summary,
        "host": {"system": platform.system(), "release": platform.release(), "machine": platform.machine(), "cpuCount": os.cpu_count()},
        "scope": "Django HTTP adapter + filesystem + PostgreSQL; excludes real network/helper/Core root selection",
        "rssScope": "process lifetime high-water, unavailable where resource.getrusage is unsupported",
        "samples": output, "successful": test_result.wasSuccessful(),
        "testCount": test_result.testsRun, "failures": len(test_result.failures), "errors": len(test_result.errors)}
    print(json.dumps(report, ensure_ascii=False))
    return 0 if test_result.wasSuccessful() else 1


def main():
    options = arguments()
    previous_temp_root = tempfile.tempdir
    with tempfile.TemporaryDirectory(prefix="workspace-storage-benchmark-") as root:
        # Keep fixture storage, restore files and settings-created temporary
        # directories inside this invocation's sole disposable parent.
        tempfile.tempdir = root
        os.environ["STORAGE_ROOT"] = str(Path(root) / "api-storage")
        try:
            return run(options)
        finally:
            tempfile.tempdir = previous_temp_root


if __name__ == "__main__":
    raise SystemExit(main())

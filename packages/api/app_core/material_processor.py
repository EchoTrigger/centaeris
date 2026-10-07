"""Platform document Worker. Only this service receives Docker/storage authority."""

import hashlib
import io
import json
import os
from pathlib import Path
import re
import tarfile
import tempfile
import time
import uuid

import docker
from django.core.files.storage import default_storage
from django.db import close_old_connections, transaction
from django.db.models import Q
from django.db.models.functions import Now
from django.utils import timezone
from django.utils.dateparse import parse_datetime

from .material_identity import processing_spec_digest
from .material_leases import claim, fenced_task, renew, LeaseLost
from .material_task_source import resolve_task_source
from .models import MaterialProcessor, MaterialProcessingTask, ProcessingSpecification
from .platform_material_commit import ProcessingOutput, commit_processing_output
from .material_staging import recover_staging


LEASE_SECONDS = 300
PROCESS_SECONDS = 1200


def processor_mounts():
    return [docker.types.Mount(target=path, source=None, type="volume") for path in ["/data/input", "/data/output"]]


def unpack_outputs(chunks, root, maximum):
    with tempfile.TemporaryFile() as stream:
        total = 0
        for chunk in chunks:
            total += len(chunk)
            if total > maximum + 64 * 1024 * 1024 + 1024 * 1024:
                raise ValueError("material_archive_too_large")
            stream.write(chunk)
        stream.seek(0)
        seen, total = set(), 0
        with tarfile.open(fileobj=stream, mode="r:") as archive:
            for entry in archive:
                if entry.name == "output" and entry.isdir():
                    continue
                if (entry.name not in {"output/canonical.md", "output/manifest.json", "output/preview.pdf", "output/workbook.json"}
                        or not entry.isfile() or entry.name in seen):
                    raise ValueError("material_archive_invalid")
                seen.add(entry.name)
                limit = (
                    64 * 1024 * 1024 if entry.name.endswith("manifest.json")
                    else 16 * 1024 * 1024 if entry.name.endswith("workbook.json")
                    else maximum
                )
                if entry.size < 0 or entry.size > limit:
                    raise ValueError("material_archive_too_large")
                if not entry.name.endswith("manifest.json"):
                    total += entry.size
                    if total > maximum:
                        raise ValueError("material_archive_too_large")
                with archive.extractfile(entry) as source, (root / entry.name.split("/")[-1]).open("wb") as target:
                    while chunk := source.read(64 * 1024):
                        target.write(chunk)


def container_options(runtime, device):
    if runtime not in {"runc", "runsc"}:
        raise ValueError("material_processor_runtime_invalid")
    if device not in {"cpu", "gpu:0"}:
        raise ValueError("material_processor_device_invalid")
    options = dict(user="10001:10001", network_mode="none", read_only=True,
                   tmpfs={"/tmp": "rw,nosuid,nodev,mode=1777"}, mem_limit=4294967296,
                   nano_cpus=4_000_000_000, pids_limit=64, cap_drop=["ALL"],
                   security_opt=["no-new-privileges"], runtime=runtime,
                   log_config=docker.types.LogConfig(type="json-file", config={"max-size": "1m", "max-file": "1"}))
    if device == "gpu:0":
        options["device_requests"] = [docker.types.DeviceRequest(device_ids=["0"], capabilities=[["gpu"]])]
    return options


class MaterialWorker:
    def __init__(self, client=None):
        self.client = client or docker.from_env(timeout=60)
        self.namespace = os.environ["MATERIAL_PROCESSOR_NAMESPACE"]
        if not re.fullmatch(r"[a-zA-Z0-9_.-]{1,64}", self.namespace):
            raise ValueError("material_processor_namespace_invalid")
        self.owner = "material-worker:" + uuid.uuid4().hex
        self.options = container_options(os.environ.get("OCI_RUNTIME", "runsc"), os.environ.get("MATERIAL_PROCESSOR_DEVICE", "cpu"))
        self.image = self.client.images.get(os.environ["MATERIAL_PROCESSOR_IMAGE"]).id
        self.cleanup_specifications()
        self.spec = self.load_specification()
        self.digest = processing_spec_digest(self.spec)
        specification, _ = ProcessingSpecification.objects.get_or_create(specDigest=self.digest, defaults={"payload": self.spec})
        if specification.payload != self.spec:
            raise ValueError("material_specification_conflict")
        MaterialProcessor.objects.update_or_create(name="document", defaults={"specification": specification})

    def cleanup_specifications(self):
        # A kill/lost create reply during startup has no task lease. Retain recent
        # creates; only an exact owned specification container older than both
        # the Engine request and specification deadlines can be reclaimed.
        for container in self.client.containers.list(all=True, filters={"label": [
                "workspace.material.namespace=" + self.namespace, "workspace.material.spec=true"]}):
            if (container.labels.get("workspace.material.namespace") != self.namespace
                    or container.labels.get("workspace.material.spec") != "true"
                    or not re.fullmatch(r"material-spec-[0-9a-f]{32}", container.name)):
                continue
            created = parse_datetime(container.attrs["Created"])
            if created is not None and (timezone.now() - created).total_seconds() > 120:
                container.remove(force=True, v=True)

    def load_specification(self):
        name = "material-spec-" + uuid.uuid4().hex
        container = self.client.containers.create(self.image, ["spec"], name=name,
            labels={"workspace.material.namespace": self.namespace, "workspace.material.spec": "true"}, **self.options)
        try:
            container.start()
            if container.wait(timeout=30)["StatusCode"] != 0:
                raise ValueError("material_processor_spec_failed")
            raw = container.logs(stdout=True, stderr=False)
            if len(raw) > 65536:
                raise ValueError("material_processor_spec_invalid")
            value = json.loads(raw)
            if set(value) != {"schema", "processorId", "processorVersion", "modelDigests", "options"} or value.pop("schema") != "knowledge.processor_spec.v1":
                raise ValueError("material_processor_spec_invalid")
            value.update(schema="knowledge.processing_specification.v1", executionImageDigest=self.image)
            processing_spec_digest(value)
            expected = "centaeris.document.cpu" if os.environ.get("MATERIAL_PROCESSOR_DEVICE", "cpu") == "cpu" else "centaeris.document.cuda.gpu0"
            if value["processorId"] != expected:
                raise ValueError("material_processor_device_mismatch")
            return value
        finally:
            container.remove(force=True, v=True)

    def cleanup_expired(self):
        self.cleanup_specifications()
        with transaction.atomic():
            for task in MaterialProcessingTask.objects.select_for_update(skip_locked=True).filter(
                executionBackend="platform", materialstagedobject__isnull=False).exclude(
                status="running", leaseExpiresAt__gt=Now()).order_by("pk")[:10]:
                recover_staging(task, discard=True)
        for container in self.client.containers.list(all=True, filters={"label": "workspace.material.namespace=" + self.namespace}):
            labels = container.labels
            identity, epoch = labels.get("workspace.material.task"), labels.get("workspace.material.epoch")
            if not identity or not epoch or not epoch.isdigit():
                continue
            expected = self.container_name(identity, int(epoch))
            if container.name != expected:
                raise ValueError("material_container_identity_mismatch")
            live = MaterialProcessingTask.objects.filter(pk=identity, executionBackend="platform", status="running",
                leaseEpoch=int(epoch), leaseExpiresAt__gt=Now()).exists()
            if not live:
                container.remove(force=True, v=True)

    def container_name(self, identity, epoch):
        return "material-" + hashlib.sha256((self.namespace + identity).encode()).hexdigest()[:32] + "-" + str(epoch)

    def run_once(self, stopped):
        close_old_connections()
        self.cleanup_expired()
        identities = list(MaterialProcessingTask.objects.filter(executionBackend="platform", status__in={"pending", "running"},
            payload__specDigest=self.digest).filter(Q(leaseExpiresAt__isnull=True) | Q(leaseExpiresAt__lte=Now()))
            .order_by("updatedAt").values_list("pk", flat=True)[:10])
        for identity in identities:
            lease = claim(identity, self.owner, LEASE_SECONDS)
            if lease is None:
                continue
            try:
                self.process(lease, stopped)
            except Exception:
                # Unknown processor outcomes are read-only and fenced; never
                # acknowledge success without the atomic representation commit.
                try:
                    with fenced_task(lease) as task:
                        task.status, task.errorCode = "pending", "material_processor_attempt_failed"
                        task.leaseOwner, task.leaseExpiresAt = "", None
                        task.save(update_fields=["status", "errorCode", "leaseOwner", "leaseExpiresAt", "updatedAt"])
                except LeaseLost:
                    pass
                raise
            return True
        return False

    def process(self, lease, stopped):
        task = MaterialProcessingTask.objects.get(pk=lease.representation_id)
        source = resolve_task_source(task)
        name = self.container_name(task.pk, lease.epoch)
        labels = {"workspace.material.namespace": self.namespace, "workspace.material.task": task.pk,
                  "workspace.material.epoch": str(lease.epoch)}
        container = None
        with tempfile.TemporaryDirectory(prefix="platform-material-") as directory:
            root = Path(directory)
            with tempfile.TemporaryFile() as archive:
                with tarfile.open(fileobj=archive, mode="w") as tar:
                    with tempfile.TemporaryFile() as original, default_storage.open(source.storage_key, "rb") as stored:
                        size, digest = 0, hashlib.sha256()
                        while chunk := stored.read(65536):
                            size += len(chunk)
                            if size > source.size_bytes:
                                raise ValueError("material_source_integrity_mismatch")
                            digest.update(chunk)
                            original.write(chunk)
                        if size != source.size_bytes or "sha256:" + digest.hexdigest() != source.input_identity["sha256"]:
                            raise ValueError("material_source_integrity_mismatch")
                        original.seek(0)
                        entry = tarfile.TarInfo("source")
                        entry.size, entry.mode = size, 0o444
                        tar.addfile(entry, original)
                    request = json.dumps({"schema": "knowledge.processing.request.v1", "inputPath": "/data/input/source",
                        "displayName": source.owner.displayName, "contentType": source.owner.contentType,
                        "outputDirectory": "/data/output"}).encode()
                    entry = tarfile.TarInfo("request.json")
                    entry.size, entry.mode = len(request), 0o444
                    tar.addfile(entry, io.BytesIO(request))
                archive.seek(0)
                try:
                    container = self.client.containers.create(self.image, ["process", "/data/input/request.json"],
                        name=name, labels=labels, mounts=processor_mounts(), **self.options)
                    container.put_archive("/data/input", archive)
                    container.start()
                    deadline = time.monotonic() + PROCESS_SECONDS
                    while True:
                        if stopped.is_set() or time.monotonic() >= deadline:
                            raise RuntimeError("material_processor_interrupted")
                        renew(lease, LEASE_SECONDS)
                        resolve_task_source(task)
                        container.reload()
                        if container.status == "exited":
                            if container.attrs["State"]["ExitCode"] != 0:
                                raise ValueError("material_processor_failed")
                            break
                        stopped.wait(2)
                    chunks, _ = container.get_archive("/data/output")
                    unpack_outputs(chunks, root, self.spec["options"]["maxOutputBytes"])
                    self.publish(lease, root)
                finally:
                    if container is not None:
                        container.remove(force=True, v=True)

    def publish(self, lease, root):
        canonical = (root / "canonical.md").read_bytes()
        preview_path = root / "preview.pdf"
        preview = preview_path.read_bytes() if preview_path.exists() else b""
        workbook_path = root / "workbook.json"
        workbook = workbook_path.read_bytes() if workbook_path.exists() else b""
        manifest = json.loads((root / "manifest.json").read_bytes())
        output = ProcessingOutput(lease.representation_id, len(canonical), "sha256:" + hashlib.sha256(canonical).hexdigest(),
            len(preview), "sha256:" + hashlib.sha256(preview).hexdigest() if preview else None, manifest,
            len(workbook), "sha256:" + hashlib.sha256(workbook).hexdigest() if workbook else None)
        commit_processing_output(lease, output, io.BytesIO(canonical + preview + workbook))

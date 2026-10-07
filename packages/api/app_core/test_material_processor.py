import io
import tarfile
from tempfile import TemporaryDirectory
from pathlib import Path
from datetime import timedelta
from unittest.mock import Mock, patch

from django.test import SimpleTestCase
from django.utils import timezone

from . import material_processor


class ProcessorArchiveTests(SimpleTestCase):
    def test_processor_rejects_noncanonical_container_runtimes(self):
        for runtime in ("", "RUNSC", " runsc ", "unknown", "runc,runsc"):
            with self.subTest(runtime=runtime), self.assertRaisesRegex(ValueError, "material_processor_runtime_invalid"):
                material_processor.container_options(runtime, "cpu")

    def test_processor_uses_runsc_when_runtime_is_not_configured(self):
        self.assert_created_runtime(None, "runsc")

    def test_processor_preserves_explicit_runtime_without_fallback(self):
        for runtime in ("runsc", "runc"):
            with self.subTest(runtime=runtime):
                self.assert_created_runtime(runtime, runtime)

    def assert_created_runtime(self, configured, expected):
        environment = {"MATERIAL_PROCESSOR_NAMESPACE": "owned", "MATERIAL_PROCESSOR_IMAGE": "processor-image"}
        if configured is not None:
            environment["OCI_RUNTIME"] = configured
        client = Mock()
        client.images.get.return_value.id = "sha256:processor"
        client.containers.create.side_effect = RuntimeError("spec_creation_stopped")
        with patch.dict(material_processor.os.environ, environment, clear=True), \
                patch.object(material_processor.MaterialWorker, "cleanup_specifications"), \
                self.assertRaisesRegex(RuntimeError, "spec_creation_stopped"):
            material_processor.MaterialWorker(client)
        self.assertEqual(client.containers.create.call_args.kwargs["runtime"], expected)
        self.assertEqual(client.containers.create.call_count, 1)

    def test_processor_cpu_quota_fits_four_core_hosts(self):
        for device in ("cpu", "gpu:0"):
            with self.subTest(device=device):
                options = material_processor.container_options("runc", device)
                self.assertEqual(options["nano_cpus"], 4_000_000_000)

    def test_orphan_spec_cleanup_is_owned_and_does_not_interrupt_recent_creation(self):
        worker = object.__new__(material_processor.MaterialWorker)
        worker.namespace, worker.client = "owned", Mock()
        def container(name, age):
            value = Mock()
            value.name = name
            value.labels = {"workspace.material.namespace": "owned", "workspace.material.spec": "true"}
            value.attrs = {"Created": (timezone.now() - timedelta(seconds=age)).isoformat()}
            return value
        old = container("material-spec-" + "a" * 32, 180)
        recent = container("material-spec-" + "b" * 32, 10)
        foreign = container("foreign", 180)
        worker.client.containers.list.return_value = [old, recent, foreign]
        worker.cleanup_specifications()
        old.remove.assert_called_once_with(force=True, v=True)
        recent.remove.assert_not_called()
        foreign.remove.assert_not_called()

    def test_processor_mounts_are_anonymous_volumes_not_host_binds(self):
        mounts = material_processor.processor_mounts()
        self.assertEqual([mount["Target"] for mount in mounts], ["/data/input", "/data/output"])
        self.assertTrue(all(mount["Type"] == "volume" and not mount.get("Source") for mount in mounts))

    def archive(self, name, data=b"text", kind=tarfile.REGTYPE):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            entry = tarfile.TarInfo(name)
            entry.type = kind
            entry.size = len(data) if kind == tarfile.REGTYPE else 0
            archive.addfile(entry, io.BytesIO(data))
        return stream.getvalue()

    def test_output_archive_rejects_paths_links_and_unknown_files(self):
        with TemporaryDirectory() as directory:
            for name, kind in [("output/../escape", tarfile.REGTYPE), ("output/canonical.md", tarfile.SYMTYPE), ("output/extra", tarfile.REGTYPE)]:
                with self.subTest(name=name, kind=kind), self.assertRaises(ValueError):
                    material_processor.unpack_outputs([self.archive(name, kind=kind)], Path(directory), 1000)

    def test_output_archive_is_bounded_and_preserves_bytes(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            material_processor.unpack_outputs([self.archive("output/canonical.md")], root, 1000)
            self.assertEqual((root / "canonical.md").read_bytes(), b"text")
            with self.assertRaises(ValueError):
                material_processor.unpack_outputs([self.archive("output/canonical.md", b"x" * 1001)], root, 1000)

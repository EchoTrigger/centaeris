import hashlib
import io
import json
from pathlib import Path
import tarfile
from unittest.mock import patch

from backports import zstd
from django.test import SimpleTestCase

from app_core.workspace_snapshot import SnapshotValidationError, validate_snapshot_stream


def sha(content):
    return "sha256:" + hashlib.sha256(content).hexdigest()


def manifest_for(content=b"abc", path="file", executable=False):
    return {"schema": "workspace.snapshot.v1", "files": [{"path": path,
        "sizeBytes": len(content), "sha256": sha(content), "executable": executable}]}


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def descriptor(content, manifest, encoding="tarZstdV1"):
    return {"generation": 1, "encoding": encoding,
        "snapshotSha256": sha(content), "snapshotSizeBytes": len(content),
        "expandedSizeBytes": sum(file["sizeBytes"] for file in manifest["files"]),
        "fileCount": len(manifest["files"])}


def entry(name, content, *, kind=tarfile.REGTYPE, mode=0o644, size=None):
    header = tarfile.TarInfo(name)
    header.type = kind
    header.mode = mode
    header.size = len(content) if size is None else size
    if kind in {tarfile.SYMTYPE, tarfile.LNKTYPE}:
        header.linkname = "outside"
    return header.tobuf(format=tarfile.GNU_FORMAT) + content + b"\0" * (-len(content) % 512)


def tar_bytes(manifest, bodies, *, manifest_bytes=None):
    result = entry("workspace-manifest.json", canonical(manifest) if manifest_bytes is None else manifest_bytes)
    for file, body in zip(manifest["files"], bodies, strict=True):
        result += entry("workspace/" + file["path"], body, mode=0o755 if file["executable"] else 0o644)
    return result + b"\0" * 1024


def compress(content, *, window=20):
    encoder = zstd.ZstdCompressor(options={zstd.CompressionParameter.window_log: window,
        zstd.CompressionParameter.checksum_flag: 1})
    return encoder.compress(content) + encoder.flush()


class LegacyFrameCharacterization(SimpleTestCase):
    def test_existing_reader_accepts_reordered_manifest_and_rejects_bad_bodies(self):
        from app_core.http.internal import _WorkspaceSnapshotReader, SessionWorkspaceError
        manifest = manifest_for()
        reordered = {"files": manifest["files"], "schema": manifest["schema"]}
        for document in [manifest, reordered]:
            encoded = canonical(document)
            frame = len(encoded).to_bytes(4, "big") + encoded + b"abc"
            candidate = descriptor(frame, manifest, "frameV1")
            reader = _WorkspaceSnapshotReader(io.BytesIO(frame), candidate)
            copied = b"".join(iter(lambda: reader.read(5), b""))
            reader.require_complete()
            self.assertEqual(copied, frame)
            for invalid in [frame[:-1], frame + b"extra", frame[:-1] + b"d"]:
                with self.subTest(document=document, invalid=invalid[-4:]), self.assertRaises(SessionWorkspaceError):
                    reader = _WorkspaceSnapshotReader(io.BytesIO(invalid), candidate)
                    while reader.read(5):
                        pass
                    reader.require_complete()


class WorkspaceSnapshotArchiveTests(SimpleTestCase):
    def test_accepts_rust_vectors_and_copies_exact_stored_bytes(self):
        path = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/workspace_archive/v1/valid.json"
        fixture = json.loads(path.read_text(encoding="utf-8"))
        for encoding in ["frameV1", "tarZstdV1"]:
            source = bytes.fromhex(fixture[encoding + "Hex"])
            sink = io.BytesIO()
            received = []
            result = validate_snapshot_stream(io.BytesIO(source), descriptor(source, fixture["manifest"], encoding),
                output=sink, on_read=received.append)
            self.assertEqual(result, fixture["manifest"])
            self.assertEqual(sink.getvalue(), source)
            self.assertEqual(sum(received), len(source))

    def test_encoding_is_explicit_and_empty_has_no_compressed_object(self):
        empty = {"generation": 1, "encoding": "frameV1", "snapshotSha256": "",
            "snapshotSizeBytes": 0, "expandedSizeBytes": 0, "fileCount": 0}
        self.assertIsNone(validate_snapshot_stream(io.BytesIO(), empty))
        for changed in [{"encoding": "tarZstdV1"}, {"encoding": "unknown"}, {"encoding": []},
                {"expandedSizeBytes": 1}, {"fileCount": 1}, {"snapshotSha256": sha(b"")}]:
            with self.subTest(changed=changed), self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(), empty | changed)
        del empty["encoding"]
        with self.assertRaises(SnapshotValidationError):
            validate_snapshot_stream(io.BytesIO(), empty)

    def test_legacy_reader_and_new_validator_agree_on_damaged_frames(self):
        from app_core.http.internal import _WorkspaceSnapshotReader, SessionWorkspaceError
        manifest = manifest_for()
        json_bytes = canonical(manifest)
        frame = len(json_bytes).to_bytes(4, "big") + json_bytes + b"abc"
        for invalid in [frame[:2], frame[:-1], frame + b"extra", frame[:-1] + b"d"]:
            candidate = descriptor(invalid, manifest, "frameV1")
            with self.subTest(size=len(invalid)), self.assertRaises(SessionWorkspaceError):
                reader = _WorkspaceSnapshotReader(io.BytesIO(invalid), candidate)
                while reader.read(5):
                    pass
                reader.require_complete()
            with self.subTest(size=len(invalid)), self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(invalid), candidate)

    def test_legacy_frame_retains_existing_key_order_behavior(self):
        manifest = manifest_for()
        document = {"files": manifest["files"], "schema": manifest["schema"]}
        encoded = canonical(document)
        frame = len(encoded).to_bytes(4, "big") + encoded + b"abc"
        self.assertEqual(validate_snapshot_stream(io.BytesIO(frame), descriptor(frame, manifest, "frameV1")), document)
        archive = compress(tar_bytes(manifest, [b"abc"], manifest_bytes=encoded))
        with self.assertRaises(SnapshotValidationError):
            validate_snapshot_stream(io.BytesIO(archive), descriptor(archive, manifest))

    def test_rejects_truncation_corruption_extra_zstd_frames_and_trailing_tar(self):
        manifest = manifest_for()
        raw = tar_bytes(manifest, [b"abc"])
        valid = compress(raw)
        validate_snapshot_stream(io.BytesIO(valid), descriptor(valid, manifest))
        bad_checksum = valid[:-1] + bytes([valid[-1] ^ 1])
        variants = [valid[:1], valid[:-1], bad_checksum, valid + valid, valid + b"extra",
            compress(raw[:-512]), compress(raw + b"\0" * 512), compress(raw + raw),
            compress(raw[:-1024] + entry("workspace/extra", b"x") + b"\0" * 1024)]
        for source in variants:
            with self.subTest(size=len(source)), self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(source), descriptor(source, manifest))

    def test_rejects_mismatched_headers_links_extensions_and_body_hash(self):
        manifest = manifest_for()
        start = entry("workspace-manifest.json", canonical(manifest))
        variants = [entry("workspace/file", b"abc", kind=kind)
            for kind in [tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE, tarfile.FIFOTYPE, tarfile.XHDTYPE]]
        variants += [entry("workspace/other", b"abc"), entry("workspace/file", b"abc", mode=0o755),
            entry("workspace/file", b"abd"), entry("workspace/file", b"ab"),
            entry("././@LongLink", b"a" * 5000, kind=tarfile.GNUTYPE_LONGNAME)]
        for replacement in variants:
            source = compress(start + replacement + b"\0" * 1024)
            with self.subTest(size=len(source)), self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(source), descriptor(source, manifest))

    def test_manifest_size_order_and_empty_body_integrity_are_checked(self):
        manifest = manifest_for()
        oversized = compress(entry("workspace-manifest.json", b"", size=1024 * 1024 + 1))
        with self.assertRaises(SnapshotValidationError):
            validate_snapshot_stream(io.BytesIO(oversized), descriptor(oversized, manifest))
        for paths in [("b", "a"), ("a", "a"), ("a", "a/child")]:
            document = {"schema": "workspace.snapshot.v1", "files": [manifest_for(path=path)["files"][0] for path in paths]}
            archive = compress(tar_bytes(document, [b"abc", b"abc"]))
            with self.subTest(paths=paths), self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(archive), descriptor(archive, document))
        document = manifest_for(b"")
        archive = compress(tar_bytes(document, [b""]))
        self.assertEqual(validate_snapshot_stream(io.BytesIO(archive), descriptor(archive, document)), document)
        document["files"][0]["sha256"] = sha(b"wrong")
        archive = compress(tar_bytes(document, [b""]))
        with self.assertRaises(SnapshotValidationError):
            validate_snapshot_stream(io.BytesIO(archive), descriptor(archive, document))

    def test_declared_manifest_limits_and_digest_bind_the_actual_stored_bytes(self):
        manifest = manifest_for()
        valid = compress(tar_bytes(manifest, [b"abc"]))
        candidate = descriptor(valid, manifest)
        validate_snapshot_stream(io.BytesIO(valid), candidate)
        for change in [{"expandedSizeBytes": 2}, {"fileCount": 2}, {"snapshotSizeBytes": len(valid) - 1},
                {"snapshotSizeBytes": len(valid) + 1}, {"snapshotSha256": sha(b"wrong")}, {"encoding": "frameV1"}]:
            with self.subTest(change=change), self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(valid), candidate | change)
        for path in ["../escape", "/absolute", "a/../b", "a\\b", "x" * 4097]:
            invalid_manifest = manifest_for(path=path)
            archive = compress(tar_bytes(invalid_manifest, [b"abc"]))
            with self.subTest(path=path), self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(archive), descriptor(archive, invalid_manifest))
        for changed in [{"schema": "unknown"}, {"unknown": True}]:
            document = manifest | changed
            archive = compress(tar_bytes(document, [b"abc"]))
            with self.assertRaises(SnapshotValidationError):
                validate_snapshot_stream(io.BytesIO(archive), descriptor(archive, manifest))

    def test_decompression_and_source_reads_are_bounded_and_window_limit_is_real(self):
        body = b"x" * (8 * 1024 * 1024)
        manifest = manifest_for(body)
        raw = tar_bytes(manifest, [body])
        archive = compress(raw)
        counts = []
        real_decompressor = zstd.ZstdDecompressor
        class ObservedDecompressor:
            def __init__(self, *args, **kwargs):
                self.inner = real_decompressor(*args, **kwargs)
            def decompress(self, *args, **kwargs):
                value = self.inner.decompress(*args, **kwargs)
                counts.append(len(value))
                return value
            def __getattr__(self, name):
                return getattr(self.inner, name)
        class BoundedSource(io.BytesIO):
            def read(self, size=-1):
                self_size = 64 * 1024
                if not 0 < size <= self_size:
                    raise AssertionError("read must be bounded")
                return super().read(size)
        with patch.object(zstd, "ZstdDecompressor", ObservedDecompressor):
            validate_snapshot_stream(BoundedSource(archive), descriptor(archive, manifest))
        self.assertGreater(len(counts), 1)
        self.assertLessEqual(max(counts), 64 * 1024)
        wide = compress(raw, window=21)
        with self.assertRaises(SnapshotValidationError):
            validate_snapshot_stream(io.BytesIO(wide), descriptor(wide, manifest))

    def test_failure_never_returns_success_even_if_output_already_has_bytes(self):
        manifest = manifest_for()
        archive = compress(tar_bytes(manifest, [b"abd"]))
        sink = io.BytesIO()
        with self.assertRaises(SnapshotValidationError):
            validate_snapshot_stream(io.BytesIO(archive), descriptor(archive, manifest), output=sink)
        self.assertGreater(len(sink.getvalue()), 0)
        class FailedSink:
            def write(self, _):
                raise OSError("destination failed")
        valid = compress(tar_bytes(manifest, [b"abc"]))
        with self.assertRaisesRegex(OSError, "destination failed"):
            validate_snapshot_stream(io.BytesIO(valid), descriptor(valid, manifest), output=FailedSink())

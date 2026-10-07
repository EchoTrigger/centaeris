# /// script
# requires-python = "==3.12.10"
# dependencies = ["boto3==1.43.97"]
# ///
"""Isolated Artifact object-storage experiment, not an application storage backend.

Run: uv run --script scripts/workspace/artifact_storage_probe.py --evidence <outside-repo-dir>
Only this program's fresh Compose project can be started, stopped or removed.
"""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, asdict
import hashlib
import http.client
import json
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
from urllib.parse import urlsplit
import uuid

IMAGE = "rustfs/rustfs@sha256:ba0a1b53e36f321c0d46f3867104abef169f7bc59c467c664ddac87e7ddc9a8b"
PREFIX = "centaeris-rustfs-probe-"


class Missing(Exception): pass
class Conflict(Exception): pass
class Denied(Exception): pass
class Unavailable(Exception): pass


def classify_error(status, code):
    if status == 404 and code in {"NoSuchKey", "NoSuchVersion"}:
        return Missing
    if status in {401, 403}:
        return Denied
    if status in {409, 412}:
        return Conflict
    return Unavailable


def require_local_endpoint(endpoint):
    url = urlsplit(endpoint)
    if (url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port
            or url.username or url.password or url.path or url.query or url.fragment):
        raise ValueError("probe endpoint must be an owned loopback port")


def make_compose(project, key, secret):
    if not project.startswith(PREFIX) or not project[len(PREFIX):].isalnum():
        raise ValueError("invalid isolated project")
    return {"name": project, "services": {"rustfs": {
        "image": IMAGE, "platform": "linux/amd64",
        "ports": ["127.0.0.1::9000"], "volumes": ["data:/data"],
        "environment": {"RUSTFS_ACCESS_KEY": key, "RUSTFS_SECRET_KEY": secret,
            "RUSTFS_VOLUMES": "/data", "RUSTFS_CONSOLE_ENABLE": "false"},
        "mem_limit": "1g", "cpus": 1, "pids_limit": 128,
        "restart": "no", "security_opt": ["no-new-privileges:true"],
        "labels": {"centaeris.probe": project},
    }}, "volumes": {"data": {"labels": {"centaeris.probe": project}}}}


def sdk(endpoint, key, secret):
    require_local_endpoint(endpoint)
    import boto3
    from botocore.config import Config
    return boto3.client("s3", endpoint_url=endpoint, aws_access_key_id=key,
        aws_secret_access_key=secret, region_name="us-east-1",
        config=Config(signature_version="s3v4", connect_timeout=2, read_timeout=3,
            retries={"total_max_attempts": 1}, proxies={}, s3={"addressing_style": "path"}))


@dataclass(frozen=True)
class ObjectRef:
    key: str
    size: int
    sha256: str
    etag: str
    version: str


class CandidateStore:
    """Evaluation-only version-pinned contract; not wired into default_storage."""
    def __init__(self, client, bucket):
        self.client, self.bucket = client, bucket

    def call(self, method, **kwargs):
        try:
            return getattr(self.client, method)(Bucket=self.bucket, **kwargs)
        except Exception as error:
            response = getattr(error, "response", {})
            status = response.get("ResponseMetadata", {}).get("HTTPStatusCode")
            code = response.get("Error", {}).get("Code")
            raise classify_error(status, code)(f"{method}:{status}:{code}") from error

    def put(self, key, data):
        sha = hashlib.sha256(data).hexdigest()
        try:
            result = self.call("put_object", Key=key, Body=data, IfNoneMatch="*",
                ChecksumSHA256=base64.b64encode(hashlib.sha256(data).digest()).decode(),
                Metadata={"sha256": sha})
        except Conflict:
            result = self.call("head_object", Key=key)
            if result["ContentLength"] != len(data):
                raise Conflict("existing key has different length")
        version = result.get("VersionId")
        if not version or version == "null":
            raise AssertionError("fixed non-null object version required")
        reference = ObjectRef(key, len(data), sha, result["ETag"], version)
        actual = self.read(reference)
        if len(actual) != len(data) or hashlib.sha256(actual).hexdigest() != sha:
            raise Conflict("existing key has different bytes")
        return reference

    def read(self, reference, start=0, end=None):
        end = reference.size if end is None else end
        if not 0 <= start <= end <= reference.size:
            raise ValueError("invalid byte interval")
        kwargs = {"Key": reference.key, "VersionId": reference.version, "IfMatch": reference.etag}
        if start != 0 or end != reference.size:
            if start == end:
                raise ValueError("empty partial interval is not an HTTP Range")
            kwargs["Range"] = f"bytes={start}-{end - 1}"
        result = self.call("get_object", **kwargs)
        try:
            data = result["Body"].read(end - start + 1)
        except Exception as error:
            raise Unavailable("response interrupted") from error
        finally:
            result["Body"].close()
        if len(data) != end - start or result["ContentLength"] != end - start:
            raise Unavailable("response length mismatch")
        if "Range" in kwargs and result.get("ContentRange") != f"bytes {start}-{end - 1}/{reference.size}":
            raise Unavailable("range response mismatch")
        return data

    def delete(self, reference):
        self.call("delete_object", Key=reference.key, VersionId=reference.version)


def docker(*args, timeout=60):
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"docker {args[0]} failed: {result.stderr[:1500]}")
    return result.stdout.strip()


def check_raises(expected, operation):
    try:
        operation()
    except expected:
        return
    raise AssertionError(f"expected {expected.__name__}")


def wait_ready(client):
    deadline = time.monotonic() + 60
    while True:
        try:
            client.list_buckets()
            return
        except Exception:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.2)


def reconnect_after_restart(store, compose, key, secret):
    compose("start", "rustfs")
    # Docker may allocate a new host port when restarting a dynamic binding.
    endpoint = "http://" + compose("port", "rustfs", "9000")
    store.client = sdk(endpoint, key, secret)
    wait_ready(store.client)
    return endpoint


def probe(store, endpoint, key, secret, compose, record):
    client, bucket = store.client, store.bucket
    client.create_bucket(Bucket=bucket)
    client.put_bucket_versioning(Bucket=bucket, VersioningConfiguration={"Status": "Enabled"})
    payload = bytes(range(256)) * 257
    refs = {}

    def ordinary():
        for name, data in (("empty", b""), ("binary", payload), ("utf8", "成果界😀".encode())):
            ref = store.put(name, data)
            assert store.read(ref) == data
            refs[name] = ref
    record("binary_and_empty_roundtrip", ordinary)

    def concurrency():
        with ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(lambda _: store.put("concurrent", payload), range(4)))
        assert len({ref.version for ref in results}) == 1
        check_raises(Conflict, lambda: store.put("concurrent", b"x" * len(payload)))
        check_raises(Conflict, lambda: store.put("concurrent", b"short"))
        assert store.read(results[0]) == payload
    record("concurrent_create_and_conflict", concurrency)

    def lost_response():
        # Server committed; caller never accepted the first response. Retrying
        # the same key must reuse the exact version, not create another object.
        first = client.put_object(Bucket=bucket, Key="response-lost", Body=payload, IfNoneMatch="*")
        second = store.put("response-lost", payload)
        assert first["VersionId"] == second.version
    record("retry_after_unobserved_put_response", lost_response)

    def ranges():
        ref = refs["binary"]
        for start, end in ((0, 1), (1, 65537), (ref.size - 1, ref.size)):
            assert store.read(ref, start, end) == payload[start:end]
        check_raises(ValueError, lambda: store.read(ref, ref.size, ref.size + 1))
        check_raises(Conflict, lambda: store.call("get_object", Key=ref.key, VersionId=ref.version, IfMatch='"wrong"'))
    record("bounded_ranges_and_preconditions", ranges)

    def versions():
        old = store.put("versioned", b"AAA")
        new = client.put_object(Bucket=bucket, Key="versioned", Body=b"BBB")
        assert store.read(old) == b"AAA"
        store.delete(old)
        store.delete(old)
        check_raises(Missing, lambda: store.read(old))
        body = client.get_object(Bucket=bucket, Key="versioned", VersionId=new["VersionId"])["Body"]
        try: assert body.read() == b"BBB"
        finally: body.close()
    record("fixed_version_and_owned_delete", versions)

    def denied():
        wrong = CandidateStore(sdk(endpoint, key, "wrong-synthetic-secret"), bucket)
        check_raises(Denied, lambda: wrong.read(refs["binary"]))
        check_raises(Missing, lambda: store.call("get_object", Key="absent"))
    record("denial_distinct_from_missing", denied)

    def multipart():
        data = b"a" * (5 * 1024 * 1024) + b"tail"
        upload = client.create_multipart_upload(Bucket=bucket, Key="multipart")["UploadId"]
        try:
            parts = []
            for number, chunk in enumerate((data[:-4], data[-4:]), 1):
                result = client.upload_part(Bucket=bucket, Key="multipart", UploadId=upload, PartNumber=number, Body=chunk)
                parts.append({"PartNumber": number, "ETag": result["ETag"]})
            result = client.complete_multipart_upload(Bucket=bucket, Key="multipart", UploadId=upload, MultipartUpload={"Parts": parts}, IfNoneMatch="*")
            ref = ObjectRef("multipart", len(data), hashlib.sha256(data).hexdigest(), result["ETag"], result["VersionId"])
            assert store.read(ref) == data
            assert ref.etag.strip('"') != ref.sha256
        except Exception:
            client.abort_multipart_upload(Bucket=bucket, Key="multipart", UploadId=upload)
            raise
        abandoned = client.create_multipart_upload(Bucket=bucket, Key="aborted")["UploadId"]
        client.upload_part(Bucket=bucket, Key="aborted", UploadId=abandoned, PartNumber=1, Body=data[:-4])
        check_raises(Missing, lambda: store.call("get_object", Key="aborted"))
        client.abort_multipart_upload(Bucket=bucket, Key="aborted", UploadId=abandoned)
        check_raises(Missing, lambda: store.call("get_object", Key="aborted"))
        remaining = client.list_multipart_uploads(Bucket=bucket).get("Uploads", [])
        assert not remaining
    record("multipart_complete_abort_and_etag", multipart)

    def truncated():
        url = urlsplit(client.generate_presigned_url("put_object", Params={"Bucket": bucket, "Key": "truncated"}, ExpiresIn=60))
        conn = http.client.HTTPConnection(url.hostname, url.port, timeout=3)
        try:
            conn.putrequest("PUT", url.path + "?" + url.query)
            conn.putheader("Content-Length", "4096")
            conn.endheaders()
            conn.send(b"incomplete")
            conn.sock.shutdown(socket.SHUT_WR)
            try:
                response = conn.getresponse()
                assert response.status >= 400
            except (OSError, http.client.HTTPException):
                pass
        finally:
            conn.close()
        check_raises(Missing, lambda: store.call("get_object", Key="truncated"))
    record("interrupted_put_is_not_visible", truncated)

    def outage():
        compose("stop", "rustfs")
        check_raises(Unavailable, lambda: store.read(refs["binary"]))
        resumed = reconnect_after_restart(store, compose, key, secret)
        print(json.dumps({"initialEndpoint": endpoint, "resumedEndpoint": resumed}), flush=True)
        assert store.read(refs["binary"]) == payload
    record("outage_and_restart_preserve_fixed_bytes", outage)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", required=True, type=Path)
    args = parser.parse_args()
    args.evidence.mkdir(parents=True, exist_ok=False)
    project = PREFIX + uuid.uuid4().hex[:12]
    key, secret = "probe" + secrets.token_hex(8), secrets.token_hex(24)
    report = {"project": project, "image": IMAGE, "sdk": "boto3==1.43.97", "cases": [],
        "budget": {"memory": "1GiB", "cpus": 1, "pids": 128, "maxConcurrentRequests": 4,
            "connectTimeoutSeconds": 2, "readTimeoutSeconds": 3, "sdkAttempts": 1,
            "largestObjectBytes": 5 * 1024 * 1024 + 4, "syntheticPayloadBudgetBytes": 64 * 1024 * 1024}}
    from importlib.metadata import version
    report["sdkVersions"] = {name: version(name) for name in ("boto3", "botocore")}
    report["sourceSha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    report["lockSha256"] = hashlib.sha256(Path(__file__).with_suffix(".py.lock").read_bytes()).hexdigest()
    before = set(docker("ps", "-q", "--no-trunc").splitlines())
    def record(name, action):
        start = time.monotonic()
        try:
            action()
            item = {"name": name, "passed": True}
        except Exception as error:
            item = {"name": name, "passed": False, "error": f"{type(error).__name__}: {error}"}
        item["elapsedMs"] = round((time.monotonic() - start) * 1000, 2)
        report["cases"].append(item)
        print(json.dumps(item), flush=True)
        if not item["passed"]:
            raise RuntimeError(f"contract failed: {name}")
    with tempfile.TemporaryDirectory(prefix=project) as directory:
        path = Path(directory) / "compose.json"
        path.write_text(json.dumps(make_compose(project, key, secret)), encoding="utf-8")
        def compose(*arguments):
            return docker("compose", "-p", project, "-f", str(path), *arguments)
        if docker("ps", "-aq", "--filter", f"label=com.docker.compose.project={project}"):
            raise RuntimeError("refusing to reuse an existing project")
        for kind in ("volume", "network"):
            if docker(kind, "ls", "-q", "--filter", f"label=com.docker.compose.project={project}"):
                raise RuntimeError("refusing to reuse existing project resources")
        try:
            compose("up", "-d", "--pull", "never")
            endpoint = "http://" + compose("port", "rustfs", "9000")
            client = sdk(endpoint, key, secret)
            wait_ready(client)
            report["container"] = compose("ps", "-q", "rustfs")
            report["imageIdentity"] = json.loads(docker("image", "inspect", IMAGE))[0]["Id"]
            probe(CandidateStore(client, "artifact-contract"), endpoint, key, secret, compose, record)
            report["passed"] = len(report["cases"]) == 9 and all(item["passed"] for item in report["cases"])
            report["stats"] = docker("stats", "--no-stream", "--format", "{{json .}}", report["container"])
        except Exception as error:
            report["passed"] = False
            report["failure"] = f"{type(error).__name__}: {error}"
            raise
        finally:
            try:
                logs = compose("logs", "--no-color", "--tail", "150")
                (args.evidence / "rustfs.log").write_text(logs.replace(key, "[redacted]").replace(secret, "[redacted]"), encoding="utf-8")
            finally:
                compose("down", "--volumes")
                report["ownedResourcesRemoved"] = not any((
                    docker("ps", "-aq", "--filter", f"label=com.docker.compose.project={project}"),
                    docker("volume", "ls", "-q", "--filter", f"label=com.docker.compose.project={project}"),
                    docker("network", "ls", "-q", "--filter", f"label=com.docker.compose.project={project}"),
                ))
                report["unrelatedContainersUnchanged"] = before == set(docker("ps", "-q", "--no-trunc").splitlines())
                (args.evidence / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    if not report["passed"] or not report["unrelatedContainersUnchanged"] or not report["ownedResourcesRemoved"]:
        raise RuntimeError("isolated contract gate failed")


if __name__ == "__main__":
    main()

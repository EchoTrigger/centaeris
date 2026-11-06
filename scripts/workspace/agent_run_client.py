"""Call the Agent Run API and verify its SSE output, or inspect an operation receipt."""
import argparse
import base64
import hashlib
from http.client import HTTPException
import json
import os
from pathlib import Path
import shlex
import sys
import time
from urllib.parse import quote
import urllib.request
import uuid


def read_events(response):
    event = ""
    data = []
    for raw in response:
        line = raw.decode("utf-8").rstrip("\r\n")
        if not line:
            if data:
                yield event, json.loads("\n".join(data))
            event, data = "", []
        elif line.startswith("event:"):
            event = line[6:].strip()
        elif line.startswith("data:"):
            data.append(line[5:].lstrip(" "))
    if event or data:
        raise RuntimeError("response ended inside an event")


def receive(response, output):
    files = {}
    completed = []
    terminal = None
    try:
        for event, value in read_events(response):
            if event == "runAccepted":
                print(json.dumps(value, ensure_ascii=False), file=sys.stderr)
            elif event == "outputText":
                print(value["text"], end="", flush=True)
            elif event == "artifactStarted":
                name = value["filename"]
                if not name or name in (".", "..") or any(char in name for char in "/\\:\0"):
                    raise RuntimeError("unsafe artifact filename")
                artifact_id = value["artifactId"]
                if artifact_id in files or not 0 <= value["sizeBytes"] <= 8 * 1024 * 1024:
                    raise RuntimeError("invalid artifact metadata")
                if len(files) >= 4 or sum(item["expectedSize"] for item in files.values()) + value["sizeBytes"] > 16 * 1024 * 1024:
                    raise RuntimeError("artifact limit exceeded")
                path = output / f".{uuid.uuid4().hex}.part"
                files[artifact_id] = {"path": path, "target": output / name, "file": path.open("xb"),
                    "digest": hashlib.sha256(), "size": 0, "index": 0,
                    "expectedSize": value["sizeBytes"], "expectedHash": value["sha256"],
                    "metadata": value, "status": "receiving"}
            elif event in {"artifactChunk", "artifactCompleted", "artifactFailed"}:
                item = files.get(value["artifactId"])
                if item is None or item["status"] != "receiving":
                    raise RuntimeError("artifact event without an active artifact")
                if event == "artifactChunk":
                    if value["index"] != item["index"]:
                        raise RuntimeError("artifact chunk sequence mismatch")
                    chunk = base64.b64decode(value["data"], validate=True)
                    item["size"] += len(chunk)
                    if item["size"] > item["expectedSize"]:
                        raise RuntimeError("artifact length mismatch")
                    item["digest"].update(chunk)
                    item["file"].write(chunk)
                    item["index"] += 1
                elif event == "artifactCompleted":
                    digest = f"sha256:{item['digest'].hexdigest()}"
                    if (any(value[field] != item["metadata"][field]
                            for field in ("filename", "contentType", "sizeBytes", "sha256"))
                            or item["size"] != item["expectedSize"] or digest != item["expectedHash"]
                            or value["chunkCount"] != item["index"]):
                        raise RuntimeError("artifact verification failed")
                    item["file"].close()
                    # Publish the verified file atomically without replacing caller data.
                    os.link(item["path"], item["target"])
                    item["path"].unlink()
                    item["status"] = "completed"
                    completed.append(item["target"])
                    print(f"\nSaved {item['target']} ({item['size']} bytes, {digest})", file=sys.stderr)
                else:
                    item["file"].close()
                    item["path"].unlink(missing_ok=True)
                    item["status"] = "failed"
                    print(f"\nArtifact {value['artifactId']} failed: {value['error']}; incomplete file discarded.",
                          file=sys.stderr)
            elif event == "runFinished":
                terminal = value
                break
        if terminal is None:
            raise RuntimeError("response disconnected before runFinished; check the original operationId")
        if terminal["runStatus"] != "completed" or terminal["deliveryStatus"] != "completed":
            raise RuntimeError(json.dumps(terminal))
        if any(item["status"] == "failed" for item in files.values()):
            raise RuntimeError("artifact delivery failed; verified files were kept")
        if len(completed) != len(files):
            raise RuntimeError("response ended with incomplete artifacts")
        return completed
    finally:
        for item in files.values():
            item["file"].close()
            item["path"].unlink(missing_ok=True)


def show_status_hint(url, operation_id):
    print("Check the original operation with the same CENTAERIS_RUN_KEY (status only; results are not replayed):",
          file=sys.stderr)
    print(shlex.join(["python", "scripts/workspace/agent_run_client.py", "--url", url,
                      "--status", "--operation-id", operation_id]), file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        "Submission exits 0 only after complete verified delivery, or 2 for a receipt without replay. "
        "--status exits 0 for a successful lookup, regardless of runStatus."
    ))
    parser.add_argument("--url", required=True, help="Externally reachable Workspace base URL")
    parser.add_argument("--prompt", help="Required for submission")
    parser.add_argument("--output", type=Path, help="Directory for verified artifacts; required for submission")
    parser.add_argument("--operation-id", help="Reuse the original value when acceptance is uncertain")
    parser.add_argument("--status", action="store_true", help="Read the original receipt without submitting a Run")
    args = parser.parse_args(argv)
    if args.operation_id == "":
        parser.error("--operation-id must not be empty")
    if args.status:
        if not args.operation_id:
            parser.error("--status requires --operation-id")
        if args.prompt is not None or args.output is not None:
            parser.error("--status does not accept --prompt or --output")
    elif args.prompt is None or args.output is None:
        parser.error("submission requires --prompt and --output")
    token = os.environ.get("CENTAERIS_RUN_KEY")
    if not token:
        parser.error("CENTAERIS_RUN_KEY must contain a Workspace API credential")
    operation_id = args.operation_id or f"{int(time.time())}.{uuid.uuid4()}"
    print(f"operationId={operation_id}", file=sys.stderr, flush=True)
    base_url = args.url.rstrip("/") + "/api/v1/agent"
    try:
        if args.status:
            request = urllib.request.Request(base_url + "/operations/" + quote(operation_id, safe=""), method="GET",
                headers={"Authorization": f"Bearer {token}", "Accept": "application/json"})
        else:
            args.output.mkdir(parents=True, exist_ok=True)
            request = urllib.request.Request(base_url + "/execute",
                data=json.dumps({"prompt": args.prompt}).encode(), method="POST",
                headers={"Authorization": f"Bearer {token}", "Idempotency-Key": operation_id,
                         "Content-Type": "application/json", "Accept": "text/event-stream, application/json"})
        with urllib.request.urlopen(request, timeout=700) as response:
            content_type = response.headers.get_content_type()
            if content_type == "application/json":
                receipt = json.load(response)
                if args.status:
                    print(json.dumps(receipt, ensure_ascii=False))
                    return 0
                print(json.dumps(receipt, ensure_ascii=False), file=sys.stderr)
                print("Operation receipt only; this response does not replay text or artifacts.", file=sys.stderr)
                show_status_hint(args.url, operation_id)
                return 2
            if args.status or content_type != "text/event-stream":
                raise RuntimeError(f"unexpected Content-Type: {content_type}")
            receive(response, args.output)
            return 0
    except (RuntimeError, OSError, ValueError, KeyError, HTTPException) as error:
        label = "Status lookup failed" if args.status else "Call incomplete"
        print(f"{label}: {error}", file=sys.stderr)
        show_status_hint(args.url, operation_id)
        return 1


if __name__ == "__main__":
    sys.exit(main())

"""Call the Workspace one-shot API and verify files received in its SSE response."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import sys
import time
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
                    "expectedSize": value["sizeBytes"], "expectedHash": value["sha256"]}
            elif event == "artifactChunk":
                item = files[value["artifactId"]]
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
                item = files[value["artifactId"]]
                digest = f"sha256:{item['digest'].hexdigest()}"
                if (item["size"] != item["expectedSize"] or digest != item["expectedHash"]
                        or value["sha256"] != digest or value["chunkCount"] != item["index"]):
                    raise RuntimeError("artifact verification failed")
                item["file"].close()
                # Publish the verified file atomically without replacing caller data.
                os.link(item["path"], item["target"])
                item["path"].unlink()
                completed.append(item["target"])
                print(f"\nSaved {item['target']} ({item['size']} bytes, {digest})", file=sys.stderr)
            elif event == "artifactFailed":
                raise RuntimeError(value["error"])
            elif event == "runFinished":
                terminal = value
                break
        if terminal is None:
            raise RuntimeError("response disconnected before runFinished; check the original operationId")
        if terminal["runStatus"] != "completed" or terminal["deliveryStatus"] != "completed":
            raise RuntimeError(json.dumps(terminal))
        if len(completed) != len(files):
            raise RuntimeError("response ended with incomplete artifacts")
        return completed
    finally:
        for item in files.values():
            item["file"].close()
            item["path"].unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="Externally reachable Workspace base URL")
    parser.add_argument("--prompt", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--operation-id", help="Reuse the original value when acceptance is uncertain")
    args = parser.parse_args()
    token = os.environ.get("CENTAERIS_RUN_KEY")
    if not token:
        parser.error("CENTAERIS_RUN_KEY must contain a Workspace API credential")
    operation_id = args.operation_id or f"{int(time.time())}.{uuid.uuid4()}"
    print(f"operationId={operation_id}", file=sys.stderr, flush=True)
    args.output.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(args.url.rstrip("/") + "/api/v1/agent-runs",
        data=json.dumps({"prompt": args.prompt}).encode(), method="POST",
        headers={"Authorization": f"Bearer {token}", "Idempotency-Key": operation_id,
                 "Content-Type": "application/json", "Accept": "text/event-stream, application/json"})
    with urllib.request.urlopen(request, timeout=700) as response:
        if response.headers.get_content_type() == "application/json":
            print(json.dumps(json.load(response)), file=sys.stderr)
            return
        receive(response, args.output)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError) as error:
        print(f"Call incomplete: {error}", file=sys.stderr)
        sys.exit(1)

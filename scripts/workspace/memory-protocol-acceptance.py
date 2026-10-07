"""Offline Memory protocol acceptance in fresh, network-disabled containers.

Requires an explicitly selected local execution image. Creates only a random
volume; never mounts deployment memory, credentials, or /mnt/data snapshots.
This checks protocol persistence, not whether a real model chooses to remember.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import uuid

ROOT = "plastic-memories://self/"
HELPER = "/opt/centaeris/bin/execution_agent"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, help="Existing local execution image; no build or pull")
    args = parser.parse_args()
    image = subprocess.check_output(["docker", "image", "inspect", args.image, "--format", "{{.Id}}"], text=True).strip()
    if not image.startswith("sha256:"):
        raise RuntimeError("image must resolve to an immutable local ID")
    volume = "centaeris-memory-check-" + uuid.uuid4().hex
    containers = set()
    requests = 0

    def container(command, scope=None, body=None):
        nonlocal requests
        requests += 1
        name = f"{volume}-{requests}"
        containers.add(name)
        mount = f"type=volume,src={volume},dst=/var/lib/centaeris/memory"
        if scope:
            mount += f",volume-subpath={scope}"
        result = subprocess.run(["docker", "run", "--rm", "--pull=never", "-i", "--name", name,
            "--network=none", "--read-only", "--tmpfs=/mnt/data:rw,size=16m", "--user=0:0", "--cap-drop=ALL",
            "--security-opt=no-new-privileges", "--mount", mount,
            "--entrypoint", command[0], image, *command[1:]],
            input=body, capture_output=True, text=True, encoding="utf-8", timeout=60)
        if result.returncode:
            raise RuntimeError(f"isolated helper failed: {result.stderr}")
        containers.discard(name)
        return result.stdout

    def operation(path, action, scope="user-a-agent-a", error=None):
        result = json.loads(container([HELPER, "filesystem-once"], scope,
            json.dumps({"path": path, "operation": action})))
        if error:
            assert result["output"] is None and result["error"]["kind"] == error, result
            return result["error"]
        assert result["error"] is None, result
        output = next(iter(result["output"].values()))
        assert output["identity"] == {"key": path, "displayPath": path}, output
        return output

    def read(path, scope="user-a-agent-a", error=None):
        return operation(path, {"type": "readFile", "maxBytes": 65536}, scope, error)

    def write(path, text, previous=None):
        return operation(path, {"type": "writeFile", "content": list(text.encode()),
            "observedFileHash": previous, "createOnly": previous is None})

    subprocess.run(["docker", "volume", "create", volume], check=True, capture_output=True, timeout=30)
    try:
        container(["python", "-c", "from pathlib import Path; import os; "
            "root=Path('/var/lib/centaeris/memory'); "
            "[( (root/s/'topics').mkdir(parents=True), os.chmod(root/s,0o700), os.chmod(root/s/'topics',0o700)) "
            "for s in ['user-a-agent-a','user-a-agent-b','user-b-agent-a']]"])
        skill_path = "/opt/centaeris/system-skills/memory/SKILL.md"
        installed = read(skill_path)
        expected = (Path(__file__).resolve().parents[1] / "skills/system/memory/SKILL.md").read_bytes()
        assert bytes(installed["bytes"]).replace(b"\r\n", b"\n") == expected.replace(b"\r\n", b"\n"), "installed skill differs from source"
        listing = operation(ROOT, {"type": "listDirectory", "recursive": True, "maxEntries": 20})
        assert [entry["path"] for entry in listing["entries"]] == [ROOT + "topics/"], listing
        read(ROOT + "MEMORY.md", error="not_found")
        topic = ROOT + "topics/test-preference.md"
        initial = "Synthetic preference: use green headings.\n"
        changed = "Synthetic preference: use blue headings.\n"
        index = "[Test preference](topics/test-preference.md) - synthetic heading preference.\n"
        write(topic, initial)
        write(ROOT + "MEMORY.md", index)
        # Every helper call runs in a new container, with only the same scoped
        # Memory volume carried forward. There is no conversation or data snapshot.
        restored = read(topic)
        assert bytes(restored["bytes"]).decode() == initial
        assert bytes(read(ROOT + "MEMORY.md")["bytes"]).decode() == index
        for scope in ["user-a-agent-b", "user-b-agent-a"]:
            read(topic, scope, "not_found")
            read(ROOT + "MEMORY.md", scope, "not_found")
        write(topic, changed, restored["fileHash"])
        operation(topic, {"type": "writeFile", "content": list(initial.encode()),
            "observedFileHash": restored["fileHash"], "createOnly": False}, error="conflict")
        updated = read(topic)
        assert bytes(updated["bytes"]).decode() == changed
        operation(topic, {"type": "deleteFile", "expectedFileHash": updated["fileHash"]}, error="permission_denied")
        assert bytes(read(topic)["bytes"]).decode() == changed, "denied deletion must preserve data"
        # Forget content through the supported mutation protocol, then verify
        # both files from fresh containers before fixture-volume cleanup.
        write(topic, "", updated["fileHash"])
        write(ROOT + "MEMORY.md", "", read(ROOT + "MEMORY.md")["fileHash"])
        assert read(topic)["bytes"] == []
        assert read(ROOT + "MEMORY.md")["bytes"] == []
        listing = operation(ROOT, {"type": "listDirectory", "recursive": True, "maxEntries": 20})
        assert all(entry["path"].startswith(ROOT) and ".memory" not in entry["path"] for entry in listing["entries"])
        for invalid in ["plastic-memories://other/MEMORY.md", ROOT + "topics/../MEMORY.md", ROOT + "MEMORY.md?x=1"]:
            read(invalid, error="invalid_path")
        print(json.dumps({"result": "passed", "imageId": image, "helperContainers": requests,
            "skillSha256": hashlib.sha256(expected.replace(b"\r\n", b"\n")).hexdigest(),
            "verified": ["installed skill read", "empty discovery", "write", "fresh-container restore",
                "separate scope isolation", "hash-guarded update", "delete denied", "forget content", "URI validation"],
            "realModelCalls": 0}, indent=2))
    finally:
        for name in containers:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True, timeout=30)
        subprocess.run(["docker", "volume", "rm", volume], check=True, capture_output=True, timeout=30)
        print("Isolated Memory fixture volume removed.")


if __name__ == "__main__":
    main()

#!/usr/bin/env bash
set -Eeuo pipefail

if [[ "${CENTAERIS_CI_DESTRUCTIVE_DOCKER:-}" != "1" || "${GITHUB_ACTIONS:-}" != "true" || "${RUNNER_ENVIRONMENT:-}" != "github-hosted" || $EUID -ne 0 ]]; then
  echo "storage preparation requires an explicitly disposable GitHub-hosted runner and root" >&2
  exit 64
fi
if [[ -n "$(docker ps -aq)" ]]; then
  echo "refusing to replace Docker storage while containers exist" >&2
  exit 65
fi

builder_root=/var/lib/docker
builder_containerd_root=/var/lib/centaeris-ci-builder-containerd
builder_socket=unix:///run/centaeris-builder/docker.sock
builder_containerd_socket=/run/centaeris-builder/containerd.sock
runtime_containerd_socket=/run/centaeris-runtime-containerd/containerd.sock
mkdir -p "$builder_root" "$builder_containerd_root"
for path in "$builder_root" "$builder_containerd_root" "$RUNNER_TEMP"; do
  if [[ "$(findmnt -n -o FSTYPE -T "$path")" != "ext4" ]]; then
    echo "CI build output and containerd metadata require ext4: $path" >&2
    exit 69
  fi
done

fixture_root="$(mktemp -d "$RUNNER_TEMP/centaeris-docker-XXXXXX")"
runtime_root="$fixture_root/runtime"
mkdir "$runtime_root"
truncate -s 32G "$fixture_root/runtime.xfs"
mkfs.xfs -q -n ftype=1 "$fixture_root/runtime.xfs"
mount -o loop,pquota "$fixture_root/runtime.xfs" "$runtime_root"
mkdir -p /run/centaeris-builder /run/centaeris-runtime-containerd
python3 -c '
import json, pathlib
runsc = json.loads(pathlib.Path("/etc/docker/daemon.json").read_text())["runtimes"]["runsc"]
config = {"runtimes": {"runsc": runsc}, "features": {"containerd-snapshotter": False}}
pathlib.Path("/run/centaeris-runtime-containerd/docker.json").write_text(json.dumps(config))
pathlib.Path("/run/centaeris-builder/docker.json").write_text(json.dumps({"features": {"containerd-snapshotter": False}}))
'
# No running containers belong to the stock daemon. Its ext4 data root becomes
# the builder; the default client socket belongs only to the XFS runtime.
systemctl stop docker.service docker.socket
ip link add name ct-build0 type bridge
ip address add 172.30.0.1/24 dev ct-build0
ip link set ct-build0 up
systemd-run --unit=centaeris-ci-builder-containerd --service-type=exec --property=Delegate=yes \
  containerd --root "$builder_containerd_root" --state /run/centaeris-builder/containerd \
  --address "$builder_containerd_socket"
systemd-run --unit=centaeris-ci-runtime-containerd --service-type=exec --property=Delegate=yes \
  containerd --root "$runtime_root/containerd" --state /run/centaeris-runtime-containerd/state \
  --address "$runtime_containerd_socket"
systemd-run --unit=centaeris-ci-runtime-docker --service-type=exec --property=Delegate=yes \
  dockerd --host unix:///var/run/docker.sock --config-file /run/centaeris-runtime-containerd/docker.json \
  --data-root "$runtime_root/docker" --pidfile /run/docker.pid --storage-driver overlay2 \
  --containerd "$runtime_containerd_socket"

# Docker 28 checks for each shared firewall chain before creating it. Finish
# runtime initialization before the builder can create the same chains.
for endpoint in unix:///var/run/docker.sock "$builder_socket"; do
  if [[ "$endpoint" == "$builder_socket" ]]; then
    systemd-run --unit=centaeris-ci-builder-docker --service-type=exec --property=Delegate=yes \
      dockerd --host "$builder_socket" --config-file /run/centaeris-builder/docker.json \
      --data-root "$builder_root" --exec-root /run/centaeris-builder/exec \
      --pidfile /run/centaeris-builder/docker.pid --storage-driver overlay2 \
      --bridge ct-build0 --containerd "$builder_containerd_socket"
  fi
  ready=0
  for ((attempt = 0; attempt < 120; attempt++)); do
    if docker --host "$endpoint" info >/dev/null 2>&1; then ready=1; break; fi
    sleep .25
  done
  if [[ "$ready" != "1" ]]; then
    journalctl --no-pager -u centaeris-ci-builder-docker -u centaeris-ci-runtime-docker \
      -u centaeris-ci-builder-containerd -u centaeris-ci-runtime-containerd --lines=100
    exit 69
  fi
done

python3 - "$runtime_root" "$builder_containerd_root" <<'PY'
import json
from pathlib import Path
import subprocess
import sys

runtime = Path(sys.argv[1])
def filesystem(path):
    return subprocess.check_output(["findmnt", "-n", "-o", "FSTYPE", "-T", str(path)], text=True).strip()

for endpoint, root, address, containerd_root, expected in (
    ("unix:///run/centaeris-builder/docker.sock", Path("/var/lib/docker"),
     "/run/centaeris-builder/containerd.sock", Path(sys.argv[2]), "ext4"),
    ("unix:///var/run/docker.sock", runtime / "docker",
     "/run/centaeris-runtime-containerd/containerd.sock", runtime / "containerd", "xfs"),
):
    info = json.loads(subprocess.check_output(["docker", "--host", endpoint, "info", "--format", "{{json .}}"], text=True))
    if (Path(info["DockerRootDir"]).resolve() != root.resolve()
            or info["Containerd"]["Address"] != address
            or filesystem(root) != expected
            or filesystem(containerd_root / "io.containerd.metadata.v1.bolt") != expected):
        raise RuntimeError(f"Docker or containerd storage boundary mismatch: {endpoint}")
    status = dict(info["DriverStatus"])
    if info["Driver"] != "overlay2" or (expected == "xfs" and (
            status.get("Backing Filesystem") != "xfs" or status.get("Supports d_type") != "true")):
        raise RuntimeError(f"Docker classic overlay2 backing mismatch: {endpoint}")
    print(f"Verified {endpoint}: Docker and containerd metadata on {expected}")
options = subprocess.check_output(["findmnt", "-n", "-o", "OPTIONS", "-T", str(runtime)], text=True).strip().split(",")
if not {"pquota", "prjquota"}.intersection(options):
    raise RuntimeError("runtime XFS project quotas are not enabled")
PY

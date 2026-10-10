#!/usr/bin/env bash
set -Eeuo pipefail

if [[ "${CENTAERIS_CI_DESTRUCTIVE_DOCKER:-}" != "1" ]]; then
  echo "docker release gate requires an explicitly disposable CI Docker host" >&2
  exit 64
fi

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$workspace_root"
export CENTAERIS_SOURCE_REVISION="$(node scripts/workspace/source-revision.mjs)"
if ! docker info --format '{{json .Runtimes}}' | grep -q '"runsc"'; then
  echo "fresh-start gate requires runsc on an explicitly disposable Docker host" >&2
  exit 69
fi

if docker ps -aq --filter label=com.docker.compose.project=centaeris-workspace | grep -q .; then
  echo "refusing to reuse an existing centaeris-workspace Compose project" >&2
  exit 65
fi
if docker volume ls -q | grep -q '^centaeris-workspace_'; then
  echo "refusing to reuse existing centaeris-workspace volumes" >&2
  exit 65
fi

env_file="$(mktemp)"
compose=(docker compose --parallel 1 --env-file "$env_file")

cleanup() {
  status=$?
  set +e
  if [[ $status -ne 0 ]]; then
    "${compose[@]}" ps
    "${compose[@]}" logs --no-color --tail=200
  fi
  "${compose[@]}" down --volumes --remove-orphans
  rm -f "$env_file"
  exit "$status"
}
trap cleanup EXIT

python3 - "$workspace_root/.env.example" "$env_file" <<'PY'
from pathlib import Path
import sys

source = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
target = Path(sys.argv[2])
values = {
    "DJANGO_SECRET_KEY": "ci-only-django-secret-key",
    "INTERNAL_API_TOKEN": "ci-only-internal-api-token",
    "AGENT_RUN_AUTHORIZATION_SIGNING_KEY": "ci-only-agent-run-signing-key",
    "CREDENTIAL_ENCRYPTION_KEY": "z5wA0vTzQGNG2LkVbNqnd3CPnGds4M8Xqy9lXgkqfZI=",
    "POSTGRES_PASSWORD": "ci-only-postgres-password",
    "BOOTSTRAP_SUPERADMIN_PASSWORD": "ci-only-bootstrap-password",
    # Synthetic empty-volume acceptance inputs, never deployment defaults.
    "UPLOAD_BODY_MAX_BYTES": "1048576",
    "UPLOAD_TEMP_MAX_BYTES": "4194304",
    "UPLOAD_MAX_CONCURRENT": "2",
    "WORK_RETURN_AUDIT_INTERVAL_SECONDS": "3600",
}
rendered = []
for line in source:
    key, separator, value = line.partition("=")
    if separator and key in values:
        line = f"{key}={values[key]}"
    rendered.append(line)
target.write_text("\n".join(rendered) + "\n", encoding="utf-8")
PY

"${compose[@]}" config --quiet
"${compose[@]}" config --format json | WORKSPACE_ROOT="$workspace_root" python3 -c '
import json, os, pathlib, sys
config = json.load(sys.stdin)
assert config["name"] == "centaeris-workspace"
required_builds = {"api", "document-processor", "runtime", "web", "worker", "workspace-general"}
assert required_builds <= {name for name, service in config["services"].items() if "build" in service}
for service_name, consumer in (("document-processor", "material-worker"), ("workspace-general", "runtime")):
    service = config["services"][service_name]
    assert not service.get("profiles")
    assert service_name in config["services"][consumer]["depends_on"]
workspace = pathlib.Path(os.environ["WORKSPACE_ROOT"]).resolve()
for name in required_builds:
    assert pathlib.Path(config["services"][name]["build"]["context"]).resolve() == workspace
for name in ("runtime", "workspace-general"):
    assert not config["services"][name]["build"].get("additional_contexts")
expected_volumes = {"agent-memory", "plugin-data", "postgres-data", "runtime-data", "storage-data", "upload-temp"}
assert set(config["volumes"]) == expected_volumes
for key, value in config["volumes"].items():
    assert value["name"] == f"centaeris-workspace_{key}"
'

for service in document-processor workspace-general runtime api worker web; do
  "${compose[@]}" build --pull "$service"
done
"${compose[@]}" config --format json | python3 scripts/workspace/verify-deployment-images.py
"${compose[@]}" up -d --wait --wait-timeout 420 postgres redis api runtime worker material-worker web

for service in postgres redis api runtime worker material-worker web; do
  "${compose[@]}" ps --status running --services | grep -Fx "$service" >/dev/null
done

"${compose[@]}" exec -T api python manage.py migrate --check --noinput
"${compose[@]}" exec -T api python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/health', timeout=5).read()"
"${compose[@]}" exec -T web wget -qO- http://127.0.0.1:3000/ >/dev/null

"${compose[@]}" exec -T api python /app/scripts/workspace/verify-api-deployment.py write
"${compose[@]}" up -d --no-build --no-deps --force-recreate --wait api
"${compose[@]}" exec -T api python /app/scripts/workspace/verify-api-deployment.py read

for service in document-processor workspace-general runtime api worker web; do
  image_id="$("${compose[@]}" images -q "$service" | head -n 1)"
  test -n "$image_id"
  test "$(docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.licenses" }}' "$image_id")" = "AGPL-3.0-only"
  test "$(docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$image_id")" = "$CENTAERIS_SOURCE_REVISION"
  test "$(docker image inspect --format '{{ index .Config.Labels "io.centaeris.source.revision" }}' "$image_id")" = "$CENTAERIS_SOURCE_REVISION"
  docker run --rm --entrypoint /bin/sh "$image_id" -ec 'test -f /usr/share/licenses/centaeris-workspace/LICENSE'
done

echo "Workspace Docker fresh-start gate passed."

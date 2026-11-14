"""Validate the rendered Compose configuration before the disposable build."""
import json
from pathlib import Path
import sys


def verify_compose(config, workspace):
    if config["name"] != "centaeris-workspace":
        raise ValueError("unexpected deployment project")
    services = config["services"]
    required = {"api", "document-processor", "runtime", "web", "workspace-general"}
    if not required <= {name for name, service in services.items() if "build" in service}:
        raise ValueError("deployment build is incomplete")
    for name, consumer in (("document-processor", "material-worker"), ("workspace-general", "runtime")):
        if services[name].get("profiles") or name not in services[consumer]["depends_on"]:
            raise ValueError(f"{consumer}: missing required {name} ownership")
    for name in required:
        if Path(services[name]["build"]["context"]).resolve() != workspace.resolve():
            raise ValueError(f"{name}: build context differs from source")
    for name in ("runtime", "workspace-general"):
        if services[name]["build"].get("additional_contexts"):
            raise ValueError(f"{name}: unexpected additional build context")
    expected = {"agent-memory", "material-broker-data", "plugin-data", "postgres-data", "runtime-data", "storage-data", "upload-temp"}
    if set(config["volumes"]) != expected:
        raise ValueError("deployment volume ownership differs")
    for name, volume in config["volumes"].items():
        if volume["name"] != f"centaeris-workspace_{name}":
            raise ValueError(f"{name}: volume belongs to another project")


if __name__ == "__main__":
    verify_compose(json.load(sys.stdin), Path(__file__).resolve().parents[2])

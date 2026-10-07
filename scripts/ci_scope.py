"""Route affected products and validate the always-running aggregate CI check."""
import argparse
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
PRODUCTS = ("coreRust", "localNode", "workspaceRust", "workspacePython", "web", "docker")
JOBS = {"coreRust": "core-rust", "localNode": "local-node", "workspaceRust": "workspace-rust",
        "workspacePython": "workspace-python", "web": "web", "docker": "docker-fresh-start"}


def select_products(paths):
    selected = set()
    shared = {"Cargo.toml", "Cargo.lock", "package.json", "package-lock.json", "pyproject.toml", "uv.lock",
              "rust-toolchain.toml", ".node-version", ".python-version", ".gitignore", ".dockerignore", "biome.json"}
    rules = [
        (("packages/core/", "packages/model-catalog/", "packages/mcp/", "packages/runtime_sqlite/"), PRODUCTS),
        (("packages/runtime/", "packages/tui/", "system-skills/", "scripts/harbor/"), ("coreRust", "localNode")),
        (("packages/desktop/",), ("localNode",)),
        (("packages/ui/",), ("localNode", "web")),
        (("packages/web/",), ("web", "docker")),
        (("packages/api/",), ("workspaceRust", "workspacePython", "docker")),
        (("packages/runtime_server/",), ("workspaceRust", "workspacePython", "web", "docker")),
        (("packages/hosted_execution/",), ("workspaceRust", "workspacePython", "docker")),
        (("packages/worker/", "packages/document_processor/"), ("workspacePython", "docker")),
        (("packages/code_preview_languages.json",), ("workspacePython", "web", "docker")),
        (("skills/system/",), ("workspaceRust", "workspacePython", "docker")),
        (("scripts/workspace/", "tests/workspace/", "perf/"), ("workspaceRust", "workspacePython", "web", "docker")),
        (("docker-compose", ".env.example"), ("workspacePython", "docker")),
    ]
    for path in paths:
        path = path.replace("\\", "/")
        # These documents are inputs to Rust protocol generation/parity checks.
        if path in {"docs/reference/RuntimeProtocol.md", "docs/reference/SessionEvents.md"}:
            selected.add("coreRust")
            continue
        if path in shared or path.startswith(".github/workflows/"):
            selected.update(PRODUCTS)
            continue
        for prefixes, products in rules:
            if path.startswith(prefixes):
                selected.update(products)
                break
        else:
            if path.startswith("scripts/"):
                selected.update(PRODUCTS)
            elif not (path.startswith(("docs/", ".github/ISSUE_TEMPLATE/")) or
                      path in {"README.md", "README.zh-CN.md", "LICENSE", "CONTRIBUTING.md", "THIRD_PARTY_NOTICES.md"}):
                selected.update(PRODUCTS)
    return {product: product in selected for product in PRODUCTS}


def aggregate_result(needs):
    scope = needs.get("scope", {})
    if scope.get("result") != "success":
        return False
    outputs = scope.get("outputs", {})
    for product, job in JOBS.items():
        selected = outputs.get(product)
        result = needs.get(job, {}).get("result")
        if selected not in ("true", "false") or result is None:
            return False
        if selected == "true" and result != "success":
            return False
        if selected == "false" and result not in ("success", "skipped"):
            return False
    return True


def changed_paths():
    event_path = os.environ.get("GITHUB_EVENT_PATH")
    if not event_path or os.environ.get("GITHUB_EVENT_NAME") == "workflow_dispatch":
        return None
    event = json.loads(Path(event_path).read_text(encoding="utf-8"))
    base = event.get("pull_request", {}).get("base", {}).get("sha") or event.get("before")
    if not base or set(base) == {"0"}:
        return None
    try:
        return subprocess.check_output(["git", "diff", "--name-only", base + "...HEAD"], cwd=ROOT, text=True).splitlines()
    except subprocess.CalledProcessError:
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*")
    parser.add_argument("--check-results", action="store_true")
    args = parser.parse_args()
    if args.check_results:
        if not aggregate_result(json.loads(os.environ["CI_JOB_RESULTS"])):
            raise SystemExit("Required product gates failed, were cancelled, or did not run")
        print("All required product gates passed")
        return
    paths = args.paths or changed_paths()
    selected = dict.fromkeys(PRODUCTS, True) if paths is None else select_products(paths)
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            for product, value in selected.items():
                output.write(f"{product}={str(value).lower()}\n")
    print(json.dumps(selected))


if __name__ == "__main__":
    main()

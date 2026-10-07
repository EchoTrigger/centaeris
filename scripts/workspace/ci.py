"""Portable Workspace CI; TEST_POSTGRES_* must select a disposable test database."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def run(label, args, capture=False):
    print(f"==> {label}", flush=True)
    env = {**os.environ, "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS", "1")}
    if os.name == "nt":
        bash = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/bin/bash.exe"
        if not bash.is_file():
            raise RuntimeError(f"Git Bash is required: {bash}")
        env["PATH"] = str(bash.parent) + os.pathsep + env["PATH"]
        env["CENTAERIS_TEST_BASH_PATH"] = str(bash)
        env["COMSPEC"] = str(Path(os.environ["SystemRoot"]) / "System32/cmd.exe")
    result = subprocess.run(args, cwd=ROOT, env=env, check=True, text=True, encoding="utf-8",
                            stdout=subprocess.PIPE if capture else None)
    return result.stdout or ""


def main(skip_frontend_tests=False):
    py = sys.executable
    npm = "npm.cmd" if os.name == "nt" else "npm"
    api = ["uv", "run", "--frozen", "--package", "api", "python"]
    gates = [
        ("Portable CI behavior", [py, "-B", "scripts/test_ci.py"]),
        ("Core pin parity", [py, "-B", "scripts/test_core_pin.py"]),
        ("Product version parity", [py, "-B", "scripts/test_product_version.py"]),
        ("Pinned public Core revision", ["node", "--test", "scripts/core-revision.test.mjs"]),
        ("Core source resolution", ["node", "--test", "scripts/core-source.test.mjs"]),
        ("Pinned Core source", ["node", "scripts/verify-core-checkout.mjs"]),
        ("Rust toolchain consistency", ["node", "--test", "scripts/rust-toolchain.test.mjs"]),
        ("Rust check", ["cargo", "check", "--workspace", "--locked"]),
        ("Rust tests", ["cargo", "test", "--workspace", "--locked"]),
        ("Transcript exporter cache isolation", [py, "-B", "scripts/test_transcript_schema.py"]),
        ("Transcript generated contract", [py, "scripts/transcript-schema.py", "--check"]),
        ("Outbox gate isolation and discovery guards", [py, "scripts/test_runtime_outbox_gate.py"]),
        ("Outbox PostgreSQL regressions", [*api, "scripts/runtime_outbox_gate.py"]),
        ("Authorization gate guards", [py, "scripts/test_authorization_gate.py"]),
        ("AgentRun authorization parity", [py, "scripts/agent-run-authorization-gate.py"]),
        ("Deployment identity contracts", [*api, "scripts/deployment-contract.test.py"]),
        ("Python discovery gate regressions", [py, "scripts/python_test_gate.py", "gate"]),
        ("Worker tests", [py, "scripts/python_test_gate.py", "worker"]),
        ("Performance harness isolation", [py, "-m", "unittest", "discover", "-s", "perf/tests", "-v"]),
        ("Performance workload metrics", ["node", "--test", "perf/tests/k6-metrics.test.mjs"]),
        ("Document processor tests", ["uv", "run", "--frozen", "--package", "centaeris-document-processor", "python", "scripts/python_test_gate.py", "document_processor"]),
        ("Django fresh migration", [*api, "packages/api/manage.py", "migrate", "--noinput", "--settings=api.migration_test_settings"]),
        ("Django migration drift", [*api, "packages/api/manage.py", "makemigrations", "--check", "--dry-run", "--settings=api.migration_test_settings", "--skip-checks"]),
        ("Full Django PostgreSQL suite", [*api, "scripts/python_test_gate.py", "api"]),
        ("First-party MCP Rust/Python client", [*api, "scripts/platform-mcp-client-gate.py"]),
        ("Node install", [npm, "ci"]),
        ("Performance artifact validation", ["node", "--test", "scripts/performance-eval-artifact.test.mjs"]),
        ("Web production validation", [npm, "run", "build", "--workspace", "packages/web"]),
    ]
    if not skip_frontend_tests:
        gates.append(("Web unit tests", [npm, "run", "test:unit", "--workspace", "packages/web"]))
    for label, args in gates:
        run(label, args)
    config = json.loads(run("Compose structure", ["docker", "compose", "--env-file", ".env.example", "config", "--format", "json"], capture=True))
    for name, consumer in (("document-processor", "material-worker"), ("workspace-general", "runtime")):
        if name not in config["services"] or config["services"][name].get("profiles") or not config["services"][consumer]["depends_on"].get(name):
            raise RuntimeError(f"Required execution image is not enabled: {name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-frontend-tests", action="store_true")
    main(parser.parse_args().skip_frontend_tests)

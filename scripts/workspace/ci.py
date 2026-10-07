"""Portable Workspace CI; TEST_POSTGRES_* must select a disposable test database."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from products import rust_packages


def run(label, args, capture=False):
    print(f"==> {label}", flush=True)
    env = {**os.environ, "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS", "2")}
    if args[:2] == ["docker", "compose"]:
        env["CENTAERIS_SOURCE_REVISION"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
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


def main(skip_frontend_tests=False, stage="All"):
    if stage not in ("All", "Rust", "Python", "Web"):
        raise ValueError(f"Unknown hosted gate stage: {stage}")
    py = sys.executable
    npm = "npm.cmd" if os.name == "nt" else "npm"
    api = ["uv", "run", "--frozen", "--package", "api", "python"]
    gates = [
        ("Portable CI behavior", [py, "-B", "scripts/workspace/test_ci.py"]),
        ("Product version parity", [py, "-B", "scripts/workspace/test_product_version.py"]),
        ("Core source resolution", ["node", "--test", "scripts/workspace/core-source.test.mjs"]),
        ("Monorepo Core source", ["node", "--test", "scripts/workspace/source-revision.test.mjs"]),
        ("Rust toolchain consistency", ["node", "--test", "scripts/workspace/rust-toolchain.test.mjs"]),


        ("Transcript exporter cache isolation", [py, "-B", "scripts/workspace/test_transcript_schema.py"]),
        ("Transcript generated contract", [py, "scripts/workspace/transcript-schema.py", "--check"]),
        ("Outbox gate isolation and discovery guards", [py, "scripts/workspace/test_runtime_outbox_gate.py"]),
        ("Outbox PostgreSQL regressions", [*api, "scripts/workspace/runtime_outbox_gate.py"]),
        ("Authorization gate guards", [py, "scripts/workspace/test_authorization_gate.py"]),
        ("AgentRun authorization parity", [py, "scripts/workspace/agent-run-authorization-gate.py"]),
        ("Deployment identity contracts", [*api, "scripts/workspace/deployment-contract.test.py"]),
        ("Python discovery gate regressions", [py, "scripts/workspace/python_test_gate.py", "gate"]),
        ("Worker tests", [py, "scripts/workspace/python_test_gate.py", "worker"]),
        ("Performance harness isolation", [py, "-m", "unittest", "discover", "-s", "perf/tests", "-v"]),
        ("Performance workload metrics", ["node", "--test", "perf/tests/k6-metrics.test.mjs"]),
        ("Document processor tests", ["uv", "run", "--frozen", "--package", "centaeris-document-processor", "python", "scripts/workspace/python_test_gate.py", "document_processor"]),
        ("Django fresh migration", [*api, "packages/api/manage.py", "migrate", "--noinput", "--settings=api.migration_test_settings"]),
        ("Django migration drift", [*api, "packages/api/manage.py", "makemigrations", "--check", "--dry-run", "--settings=api.migration_test_settings", "--skip-checks"]),
        ("Full Django PostgreSQL suite", [*api, "scripts/workspace/python_test_gate.py", "api"]),
        ("First-party MCP Rust/Python client", [*api, "scripts/workspace/platform-mcp-client-gate.py"]),
        ("Node install", [npm, "ci"]),
        ("Performance artifact validation", ["node", "--test", "scripts/workspace/performance-eval-artifact.test.mjs"]),
        ("Web production validation", [npm, "run", "build", "--workspace", "packages/web"]),
    ]
    if not skip_frontend_tests:
        gates.append(("Web unit tests", [npm, "run", "test:unit", "--workspace", "packages/web"]))
    rust_gates = []
    for name in rust_packages(hosted=True):
        rust_gates.extend([(f"Rust check {name}", ["cargo", "check", "--locked", "-p", name]),
                           (f"Rust tests {name}", ["cargo", "test", "--locked", "-p", name])])
    rust_labels = {"Core source resolution", "Monorepo Core source", "Rust toolchain consistency",
                   "Transcript exporter cache isolation", "Transcript generated contract"}
    web_labels = {"Node install", "Web production validation", "Web unit tests"}
    if stage == "Rust":
        gates = [gate for gate in gates if gate[0] in rust_labels] + rust_gates
    elif stage == "Web":
        gates = [gate for gate in gates if gate[0] in web_labels]
    elif stage == "Python":
        gates = [gate for gate in gates if gate[0] not in rust_labels | web_labels]
    else:
        gates = gates[:4] + rust_gates + gates[4:]
    for label, args in gates:
        run(label, args)
    config = json.loads(run("Compose structure", ["docker", "compose", "--env-file", ".env.example", "config", "--format", "json"], capture=True))
    for name, consumer in (("document-processor", "material-worker"), ("workspace-general", "runtime")):
        if name not in config["services"] or config["services"][name].get("profiles") or not config["services"][consumer]["depends_on"].get(name):
            raise RuntimeError(f"Required execution image is not enabled: {name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-frontend-tests", action="store_true")
    parser.add_argument("--stage", choices=("All", "Rust", "Python", "Web"), default="All")
    args = parser.parse_args()
    main(args.skip_frontend_tests, args.stage)

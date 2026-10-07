"""Verify synthetic Python signatures with the real Rust consumer."""
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def run_command(args, env):
    subprocess.run(args, cwd=ROOT, env=env, check=True)


def run_gate(run=run_command):
    with tempfile.TemporaryDirectory(prefix="centaeris-authorization-") as directory:
        artifact = Path(directory) / "vectors.json"
        receipt = Path(directory) / "receipt"
        env = {**os.environ, "AGENT_RUN_AUTHORIZATION_VECTORS": str(artifact),
               "AGENT_RUN_AUTHORIZATION_RECEIPT": str(receipt)}
        run(["uv", "run", "--frozen", "--package", "api", "python", "packages/api/manage.py", "test",
             "app_core.test_agent_run_authorization", "--noinput", "--settings=api.migration_test_settings"], env)
        if not artifact.is_file():
            raise RuntimeError("Python emitted no authorization vectors")
        run(["cargo", "test", "--locked", "-p", "runtime_server", "agent_run_authorization::", "--", "--nocapture"], env)
        if not receipt.is_file():
            raise RuntimeError("Rust did not consume Python vectors")
        try:
            consumed = int(receipt.read_text(encoding="utf-8").strip())
        except ValueError as error:
            raise RuntimeError("Invalid Rust consumption receipt") from error
        if consumed <= 0:
            raise RuntimeError("Rust consumed no Python vectors")
        print(f"Authorization parity passed: {consumed} Python-signed vectors verified by Rust.")
        return consumed


if __name__ == "__main__":
    run_gate()

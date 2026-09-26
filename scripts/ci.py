"""Portable source gates. Windows distribution acceptance remains in ci.ps1 Release."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def run_command(args, capture=False):
    print("==> " + " ".join(args), flush=True)
    env = {**os.environ, "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS", "1")}
    if os.name == "nt":
        bash = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/bin/bash.exe"
        if not bash.is_file():
            raise RuntimeError(f"Git Bash is required: {bash}")
        env["PATH"] = str(bash.parent) + os.pathsep + env["PATH"]
        env["CENTAERIS_TEST_BASH_PATH"] = str(bash)
        env["COMSPEC"] = str(Path(os.environ["SystemRoot"]) / "System32/cmd.exe")
    result = subprocess.run(args, cwd=ROOT, env=env, check=True, text=True,
                            encoding="utf-8", stdout=subprocess.PIPE if capture else None)
    return result.stdout or ""


def check_core_boundary(root=ROOT):
    forbidden = re.compile(r"centaeris_runtime_sqlite|rusqlite::|use\s+rusqlite|#\[path\s*=.*sqlite")
    for path in (root / "packages/core/src").rglob("*"):
        if path.is_file() and forbidden.search(path.read_text(encoding="utf-8")):
            raise RuntimeError(f"Core contains a SQLite adapter or source include: {path}")


def run_gate(stage, run=run_command, platform=sys.platform, frontend_tests=False):
    if stage not in ("Rust", "Node", "Source"):
        raise ValueError(f"Unknown source gate: {stage}")
    npm = "npm.cmd" if platform == "win32" else "npm"
    run([sys.executable, "-B", "scripts/test_product_version.py"])
    if stage in ("Rust", "Source"):
        for args in (
            ["fmt", "--all", "--", "--check"],
            ["check", "--workspace", "--locked"],
            ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"],
            ["run", "--locked", "-p", "centaeris-runtime", "--bin", "centaeris-runtime-protocol-docs", "--", "--check"],
        ):
            run(["cargo", *args])
        tree = run(["cargo", "tree", "--locked", "-p", "centaeris-core", "--edges", "normal,build,dev"], capture=True)
        if re.search(r"centaeris-runtime-sqlite|rusqlite", tree):
            raise RuntimeError("Core depends on the SQLite adapter")
        check_core_boundary()
        run(["cargo", "test", "--locked", "-p", "centaeris-core", "query_loop"])
        run(["cargo", "test", "--locked", "-p", "centaeris-runtime-sqlite", "--test", "core_runtime"])
        run(["cargo", "test", "--workspace", "--locked"])
    if stage in ("Node", "Source"):
        run([sys.executable, "-B", "scripts/test_system_skills.py"])
        run(["node", "--test", "packages/desktop/src/systemSkills.test.mjs"])
        run([npm, "ci"])
        if frontend_tests:
            run([npm, "run", "gate", "--workspace", "centaeris-ui"])
            run([npm, "run", "check", "--workspace", "@centaeris/electron-host"])
        else:
            for script in ("build", "lint:source"):
                run([npm, "run", script, "--workspace", "centaeris-ui"])
            for script in ("check:syntax", "check:host-parity"):
                run([npm, "run", script, "--workspace", "@centaeris/electron-host"])
        run([npm, "run", "test:third-party-licenses", "--workspace", "@centaeris/electron-host"])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", nargs="?", choices=("Rust", "Node", "Source"), default="Source")
    parser.add_argument("--frontend-tests", action="store_true")
    args = parser.parse_args()
    run_command([sys.executable, "-B", "scripts/test_ci.py"])
    run_gate(args.stage, frontend_tests=args.frontend_tests)

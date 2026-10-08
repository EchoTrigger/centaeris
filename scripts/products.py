"""Select product builds from the monorepo; no deployment or publication."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def rust_packages(hosted=False):
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    local = set(workspace["default-members"])
    paths = [path for path in workspace["members"] if (path not in local) == hosted]
    return [tomllib.loads((ROOT / path / "Cargo.toml").read_text(encoding="utf-8"))["package"]["name"] for path in paths]


def package_flags(hosted=False):
    return [flag for name in rust_packages(hosted) for flag in ("-p", name)]


def source_revision(clean=True):
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise RuntimeError("Source revision must be one complete checkout SHA")
    if clean and subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT, text=True).strip():
        raise RuntimeError("Artifact builds require clean tracked source")
    return revision


def build_commands(product, release=False):
    pnpm = "pnpm.cmd" if os.name == "nt" else "pnpm"
    rust = {"core": "centaeris-core", "runtime": "centaeris-runtime", "tui": "centaeris-tui",
            "runtime-server": "runtime_server", "hosted-execution": "hosted_execution"}
    if product in rust:
        names = ["centaeris-runtime", rust[product]] if product == "tui" else [rust[product]]
        return [["cargo", "build", "--locked", "-p", name, *(["--release"] if release else [])] for name in names]
    if product == "desktop":
        return [["pwsh", "-NoProfile", "-File", "scripts/build-desktop.ps1"]]
    if product == "web":
        return [[pnpm, "--filter", "web", "--filter", "centaeris", "install", "--frozen-lockfile"],
                [pnpm, "--filter", "web", "run", "build"]]
    python = {"api": "api", "worker": "workspace-agent-worker", "document-processor": "centaeris-document-processor"}
    if product in python:
        return [["uv", "sync", "--frozen", "--package", python[product]]]
    if product == "workspace-images":
        return [[sys.executable, "scripts/workspace/build.py"]]
    raise ValueError(f"Unknown product: {product}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("build",))
    parser.add_argument("product", choices=("core", "runtime", "tui", "desktop", "runtime-server", "hosted-execution",
                                           "web", "api", "worker", "document-processor", "workspace-images"))
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    commands = build_commands(args.product, args.release)
    if args.dry_run:
        print(json.dumps(commands))
        return
    for command in commands:
        subprocess.run(command, cwd=ROOT, check=True,
                       env={**os.environ, "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS", "2")})


if __name__ == "__main__":
    main()

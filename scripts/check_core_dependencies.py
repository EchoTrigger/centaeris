"""Check the package identities declared as Core's direct normal dependencies."""

import json
from pathlib import Path
import subprocess


CORE_MANIFEST = Path(__file__).resolve().parents[1] / "packages/core/Cargo.toml"
FORBIDDEN_DIRECT_DEPENDENCIES = {"rusqlite", "postgres", "windows-sys", "libc", "fs2", "zstd"}


def check_core_direct_dependencies(manifest=CORE_MANIFEST):
    manifest = manifest.resolve()
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--format-version", "1", "--no-deps", "--offline", "--locked",
        "--manifest-path", str(manifest),
    ], encoding="utf-8"))
    core = next((package for package in metadata["packages"]
                 if Path(package["manifest_path"]).resolve() == manifest), None)
    if core is None:
        raise RuntimeError(f"Cargo metadata does not contain {manifest}")

    violations = []
    for dependency in core["dependencies"]:
        name = dependency["name"]
        if dependency["kind"] is not None or name not in FORBIDDEN_DIRECT_DEPENDENCIES:
            continue
        alias = f" as {dependency['rename']}" if dependency["rename"] else ""
        target = f" for {dependency['target']}" if dependency["target"] else ""
        violations.append(f"{name}{alias}{target}")

    if violations:
        raise RuntimeError("Core direct normal dependencies must stay adapter-free: " + ", ".join(violations))


if __name__ == "__main__":
    check_core_direct_dependencies()

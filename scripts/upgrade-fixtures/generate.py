"""Generate synthetic fixtures using an isolated checkout of the released source.

Usage: python scripts/upgrade-fixtures/generate.py OLD_CHECKOUT OUTPUT_DIRECTORY
Requires Rust; never point OLD_CHECKOUT at a working development checkout.
"""
import os
from pathlib import Path
import sqlite3
import subprocess
import sys

old, output = map(lambda value: Path(value).resolve(), sys.argv[1:])
revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=old, text=True).strip()
assert revision == "5ba7cb73540ddb208cde40637a5044dd914b2a97", revision
output.mkdir(parents=True, exist_ok=False)
env = dict(os.environ, CENTAERIS_UPGRADE_FIXTURE_OUT=str(output))
source = old / "packages/core/src/session.rs"
original = source.read_bytes()
storage = old / "packages/runtime/src/message_log.rs"
storage_original = storage.read_bytes()
example = old / "packages/runtime_sqlite/examples/upgrade_fixture.rs"
assert not example.exists()
try:
    text = original.decode()
    offset = text.rfind("}")
    source.write_text(text[:offset] + Path(__file__).with_name("session.rs").read_text() + text[offset:], encoding="utf-8")
    example.parent.mkdir(exist_ok=True)
    example.write_bytes(Path(__file__).with_name("store.rs").read_bytes())
    subprocess.run(["cargo", "test", "--locked", "-p", "centaeris-core", "--lib", "export_released_upgrade_fixture"], cwd=old, env=env, check=True)
    subprocess.run(["cargo", "run", "--locked", "-p", "centaeris-runtime-sqlite", "--example", "upgrade_fixture"], cwd=old, env=env, check=True)
    storage.write_bytes(storage_original + b"\n" + Path(__file__).with_name("storage.rs").read_bytes())
    subprocess.run(["cargo", "test", "--locked", "-p", "centaeris-runtime", "--bin", "centaeris-runtime", "export_released_storage_fixture"], cwd=old, env=env, check=True)
    with sqlite3.connect(output / "runtime.db") as db:
        (output / "runtime.sql").write_text("\n".join(db.iterdump()) + "\n", encoding="utf-8")
finally:
    source.write_bytes(original)
    storage.write_bytes(storage_original)
    example.unlink(missing_ok=True)

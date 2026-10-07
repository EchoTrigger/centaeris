"""Read shared test corpora from the verified, pinned public Core source."""

from functools import cache
from pathlib import Path
import subprocess


@cache
def core_source():
    result = subprocess.run(
        ["node", "scripts/core-source.mjs", "--strict"],
        cwd=Path(__file__).resolve().parents[3], check=True,
        capture_output=True, encoding="utf-8",
    )
    return Path(result.stdout.strip())


def core_fixture(name):
    return core_source() / "packages/core/tests/fixtures" / name

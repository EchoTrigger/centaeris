"""Check or update derived Core pins without resolving a floating Git ref."""
import argparse
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]
URL = "https://github.com/EchoTrigger/centaeris.git"
PACKAGES = ("centaeris-core", "centaeris-model-catalog", "centaeris-mcp", "centaeris-runtime-sqlite")


def validate(root=ROOT):
    pin = (root / "core-revision.txt").read_text(encoding="utf-8").strip()
    if not re.fullmatch(r"[0-9a-f]{40}", pin):
        raise ValueError("Core pin must be a full lowercase SHA")
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    for name in PACKAGES:
        dependency = manifest["workspace"]["dependencies"][name]
        if dependency != {"git": URL, "rev": pin}:
            raise ValueError(f"{name} must use the exact Core Git pin")
    source = f"git+{URL}?rev={pin}#{pin}"
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    for name in PACKAGES:
        packages = [p for p in lock["package"] if p["name"] == name]
        if len(packages) != 1 or packages[0].get("source") != source:
            raise ValueError(f"Cargo.lock has an inconsistent {name} source")
    env = (root / ".env.example").read_text(encoding="utf-8")
    if re.findall(r"^CENTAERIS_CORE_REVISION=(.*)$", env, re.M) != [pin]:
        raise ValueError("Compose example must use the exact Core pin")
    return pin


def sync(root=ROOT):
    pin = (root / "core-revision.txt").read_text(encoding="utf-8").strip()
    if not re.fullmatch(r"[0-9a-f]{40}", pin):
        raise ValueError("Core pin must be a full lowercase SHA")
    path = root / "Cargo.toml"
    source = path.read_text(encoding="utf-8")
    for name in PACKAGES:
        pattern = rf'(\[workspace\.dependencies\.{re.escape(name)}\]\n)[^\[]*'
        source, count = re.subn(pattern, lambda m: m[1] + f'git = "{URL}"\nrev = "{pin}"\n\n', source)
        if count != 1:
            raise ValueError(f"Expected one manifest section for {name}")
    path.write_text(source.rstrip() + "\n", encoding="utf-8", newline="\n")
    path = root / ".env.example"
    source, count = re.subn(r"^CENTAERIS_CORE_REVISION=.*$", f"CENTAERIS_CORE_REVISION={pin}", path.read_text(encoding="utf-8"), flags=re.M)
    if count != 1:
        raise ValueError("Expected one Compose Core pin")
    path.write_text(source, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sync", action="store_true", help="Update Cargo.toml and .env.example; then run cargo check to update Cargo.lock")
    args = parser.parse_args()
    if args.sync:
        sync()
    else:
        print(validate())

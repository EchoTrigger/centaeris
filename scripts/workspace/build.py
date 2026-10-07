"""Build selected hosted images serially with the actual source revision."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from products import source_revision

SERVICES = ("runtime", "workspace-general", "api", "worker", "web", "document-processor")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("services", nargs="*")
    parser.add_argument("--env-file", default=".env.example")
    args = parser.parse_args()
    if set(args.services) - set(SERVICES):
        parser.error("Unknown hosted service")
    env = {**os.environ, "CENTAERIS_SOURCE_REVISION": source_revision()}
    for service in args.services or SERVICES:
        subprocess.run(["docker", "compose", "--parallel", "1", "--env-file", args.env_file,
                        "-f", "docker-compose.yml", "build", service], cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()

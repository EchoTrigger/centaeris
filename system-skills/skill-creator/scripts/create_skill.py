#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Derived from openai/skills and substantially modified for Centaeris.

import argparse
import os
import re
import unicodedata
import shutil
import tempfile
from pathlib import Path


NAME_PATTERN = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
OPTIONAL_DIRECTORIES = {"references", "scripts", "assets"}


def parse_optional_directories(raw: str) -> list[str]:
    names = [item.strip() for item in raw.split(",") if item.strip()]
    invalid = sorted(set(names) - OPTIONAL_DIRECTORIES)
    if invalid:
        raise ValueError(f"unsupported package directory: {invalid[0]}")
    return sorted(set(names))


def create_skill(name: str, description: str, catalog: Path, directories: list[str]) -> Path:
    if len(name) > 64 or not NAME_PATTERN.fullmatch(name):
        raise ValueError("name must be 1-64 lowercase ASCII letters, digits, or single hyphens")
    description = description.strip()
    if not description or len(description) > 1024 or any(
        unicodedata.category(character) == "Cc" for character in description
    ):
        raise ValueError(
            "description must be one non-empty line of at most 1024 characters without controls"
        )
    catalog = catalog.resolve(strict=True)
    if not catalog.is_dir():
        raise ValueError(f"catalog is not a directory: {catalog}")
    destination = catalog / name
    if os.path.lexists(destination):
        raise FileExistsError(f"skill already exists: {destination}")

    staging_root = Path(tempfile.mkdtemp(prefix=f".{name}-", dir=catalog))
    staged_skill = staging_root / name
    try:
        staged_skill.mkdir()
        (staged_skill / "SKILL.md").write_text(
            f"---\nname: {name}\ndescription: >-\n  {description}\n---\n\n# {name}\n\nDescribe the workflow here.\n",
            encoding="utf-8",
            newline="\n",
        )
        for directory in directories:
            (staged_skill / directory).mkdir()
        staged_skill.replace(destination)
        return destination
    finally:
        shutil.rmtree(staging_root, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser(description="Create a minimal Centaeris Skill package.")
    parser.add_argument("name")
    parser.add_argument("--catalog", required=True, type=Path)
    parser.add_argument("--description", required=True)
    parser.add_argument("--with", dest="directories", default="")
    args = parser.parse_args()
    created = create_skill(
        args.name,
        args.description,
        args.catalog,
        parse_optional_directories(args.directories),
    )
    print(created)


if __name__ == "__main__":
    main()

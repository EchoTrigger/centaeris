#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Derived from openai/skills and substantially modified for Centaeris.

import argparse
import os
import re
import shutil
import stat
import tempfile
from pathlib import Path


NAME_PATTERN = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
FRONTMATTER_NAME = re.compile(r"(?m)^name:\s*['\"]?([^'\"\r\n]+)['\"]?\s*$")
FRONTMATTER_DESCRIPTION = re.compile(r"(?m)^description:\s*\S.*$")


def is_link_like(path: Path) -> bool:
    metadata = path.lstat()
    return stat.S_ISLNK(metadata.st_mode) or bool(
        getattr(metadata, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    )


def validate_source(source: Path) -> tuple[Path, str]:
    source = source.absolute()
    if is_link_like(source):
        raise ValueError(f"source must not be a link or reparse point: {source}")
    source = source.resolve(strict=True)
    if not source.is_dir():
        raise ValueError(f"source must be a real directory: {source}")
    for current_root, directory_names, file_names in os.walk(source, followlinks=False):
        current = Path(current_root)
        for child_name in [*directory_names, *file_names]:
            child = current / child_name
            if is_link_like(child):
                raise ValueError(f"skill package contains a link or reparse point: {child}")

    skill_md = source / "SKILL.md"
    content = skill_md.read_text(encoding="utf-8").replace("\r\n", "\n")
    if not content.startswith("---\n") or "\n---\n" not in content[4:]:
        raise ValueError(f"SKILL.md must start with YAML frontmatter: {skill_md}")
    frontmatter = content[4 : content.index("\n---\n", 4)]
    name_match = FRONTMATTER_NAME.search(frontmatter)
    name = name_match.group(1).strip() if name_match else ""
    if not NAME_PATTERN.fullmatch(name) or len(name) > 64:
        raise ValueError(f"invalid Skill name in {skill_md}")
    if name != source.name:
        raise ValueError(f"Skill name must match its parent directory: {name} != {source.name}")
    if not FRONTMATTER_DESCRIPTION.search(frontmatter):
        raise ValueError(f"Skill description is required: {skill_md}")
    return source, name


def install_skill(source: Path, catalog: Path) -> Path:
    source, name = validate_source(source)
    catalog = catalog.resolve(strict=True)
    if not catalog.is_dir():
        raise ValueError(f"catalog is not a directory: {catalog}")
    destination = catalog / name
    if os.path.lexists(destination):
        raise FileExistsError(f"skill already exists: {destination}")

    staging_root = Path(tempfile.mkdtemp(prefix=f".{name}-", dir=catalog))
    staged_skill = staging_root / name
    try:
        shutil.copytree(source, staged_skill)
        staged_skill.replace(destination)
        return destination
    finally:
        shutil.rmtree(staging_root, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser(description="Install one trusted local Centaeris Skill package.")
    parser.add_argument("source", type=Path)
    parser.add_argument("--catalog", required=True, type=Path)
    args = parser.parse_args()
    print(install_skill(args.source, args.catalog))


if __name__ == "__main__":
    main()

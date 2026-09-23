"""Behavior checks for the bundled System Skill helpers."""

import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True


def load_helper(relative_path: str):
    path = ROOT / relative_path
    spec = importlib.util.spec_from_file_location(path.stem, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


creator = load_helper("system-skills/skill-creator/scripts/create_skill.py")
installer = load_helper("system-skills/skill-installer/scripts/install_skill.py")


def simulated_occupied_path(destination: Path):
    """Use a filesystem stand-in when Windows cannot create a dangling link."""
    original_lexists = os.path.lexists
    return mock.patch.object(
        os.path,
        "lexists",
        side_effect=lambda path: Path(path).name == destination.name or original_lexists(path),
    )


class SystemSkillHelpersTest(unittest.TestCase):
    def test_creator_preserves_literal_quotes_and_backslashes_in_description(self):
        with tempfile.TemporaryDirectory() as temporary:
            catalog = Path(temporary)
            description = 'Use when the user says "draft" or names C:\\drafts.'
            created = creator.create_skill("sample-skill", description, catalog, [])
            content = (created / "SKILL.md").read_text(encoding="utf-8")
            self.assertIn(f"description: >-\n  {description}\n", content)

    def test_created_package_can_be_copied_without_losing_resources(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_catalog = root / "source"
            destination_catalog = root / "destination"
            source_catalog.mkdir()
            destination_catalog.mkdir()
            created = creator.create_skill(
                "sample-skill", "Use when a sample is requested.", source_catalog, ["scripts"]
            )
            (created / "scripts" / "check.py").write_text(
                "print('sample')\n", encoding="utf-8"
            )
            installed = installer.install_skill(created, destination_catalog)
            self.assertEqual(installed.name, "sample-skill")
            self.assertEqual(
                (installed / "SKILL.md").read_bytes(),
                (created / "SKILL.md").read_bytes(),
            )
            self.assertEqual(
                (installed / "scripts" / "check.py").read_bytes(),
                (created / "scripts" / "check.py").read_bytes(),
            )

    def test_creator_rejects_embedded_control_characters(self):
        with tempfile.TemporaryDirectory() as temporary:
            for description in ("Use when a\rrequest arrives.", "Use when a\u0085request arrives."):
                with self.subTest(description=repr(description)):
                    with self.assertRaises(ValueError):
                        creator.create_skill(
                            "sample-skill", description, Path(temporary), []
                        )

    def test_creator_rejects_dangling_destination_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            catalog = Path(temporary)
            destination = catalog / "sample-skill"
            try:
                os.symlink(catalog / "missing-target", destination, target_is_directory=True)
            except (OSError, NotImplementedError):
                with simulated_occupied_path(destination):
                    with self.assertRaises(FileExistsError):
                        creator.create_skill("sample-skill", "Sample.", catalog, [])
                self.assertFalse(destination.exists())
            else:
                with self.assertRaises(FileExistsError):
                    creator.create_skill("sample-skill", "Sample.", catalog, [])
                self.assertTrue(destination.is_symlink())

    def test_installer_rejects_dangling_destination_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source" / "sample-skill"
            source.mkdir(parents=True)
            (source / "SKILL.md").write_text(
                "---\nname: sample-skill\ndescription: Sample.\n---\n",
                encoding="utf-8",
            )
            catalog = root / "catalog"
            catalog.mkdir()
            destination = catalog / "sample-skill"
            try:
                os.symlink(root / "missing-target", destination, target_is_directory=True)
            except (OSError, NotImplementedError):
                with simulated_occupied_path(destination):
                    with self.assertRaises(FileExistsError):
                        installer.install_skill(source, catalog)
                self.assertFalse(destination.exists())
            else:
                with self.assertRaises(FileExistsError):
                    installer.install_skill(source, catalog)
                self.assertTrue(destination.is_symlink())


if __name__ == "__main__":
    unittest.main()

"""Exercise the Core dependency check against real Cargo declarations."""

from pathlib import Path
import tempfile
import unittest

from check_core_dependencies import check_core_direct_dependencies


class CoreDirectDependencyTests(unittest.TestCase):
    def setUp(self):
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.manifest = root / "Cargo.toml"
        (root / "lib.rs").touch()
        for name in ("libc", "shared"):
            dependency = root / name
            dependency.mkdir()
            (dependency / "lib.rs").touch()
            (dependency / "Cargo.toml").write_text(
                f'[package]\nname = "{name}"\nversion = "0.1.0"\n[lib]\npath = "lib.rs"\n',
                encoding="utf-8",
            )

    def write_core_manifest(self, dependencies):
        self.manifest.write_text(
            '[package]\nname = "centaeris-core"\nversion = "0.1.0"\n[lib]\npath = "lib.rs"\n' + dependencies,
            encoding="utf-8",
        )

    def test_rejects_renamed_direct_dependencies_including_other_targets(self):
        other_target = 'cfg(target_os = "none")'
        for section, diagnostic in (
            ("[dependencies]", "libc as platform_api"),
            (f"[target.'{other_target}'.dependencies]", f"libc as platform_api for {other_target}"),
        ):
            with self.subTest(section=section):
                self.write_core_manifest(f'{section}\nplatform_api = {{ package = "libc", path = "libc" }}\n')
                with self.assertRaises(RuntimeError) as raised:
                    check_core_direct_dependencies(self.manifest)
                self.assertIn(diagnostic, str(raised.exception))

    def test_rejects_forbidden_dependency_in_dotted_table(self):
        self.write_core_manifest('[dependencies.libc]\npath = "libc"\n')
        with self.assertRaisesRegex(RuntimeError, "adapter-free: libc"):
            check_core_direct_dependencies(self.manifest)

    def test_allows_build_dev_and_allowed_package_regardless_of_alias(self):
        for section in ("[build-dependencies]", "[target.'cfg(unix)'.dev-dependencies]"):
            with self.subTest(section=section):
                self.write_core_manifest(
                    f'{section}\nplatform_api={{package="libc",path="libc"}}\n'
                    '[dependencies."libc"]\npackage="shared"\npath="shared"\n'
                )
                check_core_direct_dependencies(self.manifest)


if __name__ == "__main__":
    unittest.main()

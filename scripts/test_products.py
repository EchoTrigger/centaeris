"""Exercise product selection without executing external builds."""
import json
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ProductEntryTests(unittest.TestCase):
    def plan(self, product):
        result = subprocess.run([sys.executable, "scripts/products.py", "build", product, "--dry-run"],
                                cwd=ROOT, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_local_products_do_not_install_python_or_build_hosted_images(self):
        for product in ("core", "runtime", "tui", "desktop"):
            commands = self.plan(product)
            self.assertFalse(any(command[0] in ("uv", "docker") for command in commands))

    def test_hosted_rust_binaries_are_selected_independently(self):
        for product, package in (("runtime-server", "runtime_server"), ("hosted-execution", "hosted_execution")):
            commands = self.plan(product)
            self.assertEqual(len(commands), 1)
            self.assertIn(package, commands[0])
            self.assertNotIn("--workspace", commands[0])

    def test_web_build_selects_web_only(self):
        commands = self.plan("web")
        self.assertTrue(any("web" in command for command in commands))
        self.assertFalse(any("packages/desktop" in command for command in commands))

    def test_unknown_product_fails_without_running_a_build(self):
        result = subprocess.run([sys.executable, "scripts/products.py", "build", "unknown", "--dry-run"],
                                cwd=ROOT, capture_output=True)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()

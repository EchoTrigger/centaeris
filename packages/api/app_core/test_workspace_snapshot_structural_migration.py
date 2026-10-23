"""Fresh migration must work in the isolated structural-gate database."""
from pathlib import Path
import subprocess
import sys
from unittest import TestCase


class WorkspaceSnapshotMigrationTests(TestCase):
    def test_fresh_structural_migration_creates_publication_schema(self):
        manage = Path(__file__).resolve().parents[1] / "manage.py"
        result = subprocess.run([sys.executable, "-B", str(manage), "migrate",
            "--noinput", "--settings=api.migration_test_settings"],
            capture_output=True, text=True, encoding="utf-8", timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
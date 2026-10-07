"""Core corpora follow the locked source, independent of checkout layout."""

import subprocess
import tempfile
from pathlib import Path
from unittest.mock import patch

from django.test import SimpleTestCase

from . import core_test_fixtures


class CoreFixtureTests(SimpleTestCase):
    def tearDown(self):
        core_test_fixtures.core_source.cache_clear()

    def test_reads_corpus_from_resolved_source_outside_workspace(self):
        core_test_fixtures.core_source.cache_clear()
        with tempfile.TemporaryDirectory(prefix="Core source ") as directory:
            source = Path(directory)
            fixture = source / "packages/core/tests/fixtures/model_images.json"
            fixture.parent.mkdir(parents=True)
            fixture.write_text('{"source": "locked"}', encoding="utf-8")
            with patch.object(core_test_fixtures.subprocess, "run", return_value=
                              subprocess.CompletedProcess([], 0, stdout=str(source) + "\n")) as resolve:
                self.assertEqual(core_test_fixtures.core_fixture("model_images.json").read_text(encoding="utf-8"),
                                 '{"source": "locked"}')
                self.assertEqual(core_test_fixtures.core_fixture("live_reasoning.json"),
                                 fixture.with_name("live_reasoning.json"))
            resolve.assert_called_once_with(
                ["node", "scripts/core-source.mjs", "--strict"],
                cwd=Path(__file__).resolve().parents[3], check=True,
                capture_output=True, encoding="utf-8",
            )

    def test_resolution_failure_does_not_fall_back_to_adjacent_checkout(self):
        core_test_fixtures.core_source.cache_clear()
        error = subprocess.CalledProcessError(1, ["node", "scripts/core-source.mjs"])
        with patch.object(core_test_fixtures.subprocess, "run", side_effect=error):
            with self.assertRaises(subprocess.CalledProcessError):
                core_test_fixtures.core_fixture("model_images.json")

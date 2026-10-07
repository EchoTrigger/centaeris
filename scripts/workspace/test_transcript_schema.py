"""Exercise schema generation with real, dependency-free Cargo exporters."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "transcript_schema", Path(__file__).with_name("transcript-schema.py")
)
exporter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(exporter)


class TranscriptSchemaTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="transcript schema ")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        # TEMP may live inside a Cargo workspace. Reproduce that nesting even
        # when the host's temporary directory is outside the repository.
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = []\n', encoding="utf-8"
        )
        self.workspace = self.root / "workspace"
        self.core = self.root / "core"
        self.output = self.workspace / "generated" / "transcript_block.schema.json"
        self.make_package(self.core / "packages/core", "centaeris-core", r'''
fn main() {
    if std::env::args().any(|arg| arg == "--samples") {
        println!("{}", r#"[{"owner":"core"}]"#);
    } else {
        println!("{}", r#"{"owner":"core"}"#);
    }
}
''')
        self.make_package(self.workspace, "runtime_server", r'''
fn main() {
    println!("{}", r#"{"schemas":{"owner":"workspace"},"samples":[{"owner":"workspace"}]}"#);
}
''')
        # Only source discovery is replaced. Cargo really builds and runs two
        # identically named examples, including its warm-cache executable reuse.
        self.run_process = subprocess.run

    def make_package(self, directory, name, source):
        (directory / "examples").mkdir(parents=True)
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n'
            '[workspace]\n'
            '[features]\ncontract-schema = []\n', encoding="utf-8"
        )
        (directory / "Cargo.lock").write_text(
            f'version = 4\n[[package]]\nname = "{name}"\nversion = "0.1.0"\n',
            encoding="utf-8",
        )
        (directory / "examples/transcript_schema.rs").write_text(source, encoding="utf-8")

    def invoke(self, *, check=False):
        def run(command, **kwargs):
            if command == ["node", "scripts/core-source.mjs"]:
                return subprocess.CompletedProcess(command, 0, stdout=str(self.core))
            return self.run_process(command, **kwargs)

        argv = ["transcript-schema.py"] + (["--check"] if check else [])
        with patch.object(exporter, "ROOT", self.workspace), \
                patch.object(exporter, "OUTPUT", self.output), \
                patch.object(exporter.subprocess, "run", run), \
                patch.object(sys, "argv", argv):
            exporter.main()

    def test_repeated_export_and_check_keep_each_owners_contract(self):
        with patch.dict(os.environ):
            os.environ.pop("CARGO_TARGET_DIR", None)
            for target in (None, str(self.root / "custom target")):
                with self.subTest(target=target):
                    if target is not None:
                        os.environ["CARGO_TARGET_DIR"] = target
                    self.invoke()
                    self.invoke(check=True)
                    self.invoke()
                    self.invoke(check=True)
                    for filename, expected in (
                        ("transcript_block.schema.json", {"owner": "core"}),
                        ("transcript_block.samples.json", [{"owner": "core"}]),
                        ("transcript_envelopes.schema.json", {"owner": "workspace"}),
                        ("transcript_envelopes.samples.json", [{"owner": "workspace"}]),
                    ):
                        self.assertEqual(
                            json.loads(self.output.with_name(filename).read_text(encoding="utf-8")),
                            expected,
                        )

    def test_check_rejects_stale_contract_without_rewriting_it(self):
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.root / "target")}):
            self.invoke()
            self.output.write_text('{"changed":true}\n', encoding="utf-8")
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                self.invoke(check=True)
            self.assertEqual(self.output.read_text(encoding="utf-8"), '{"changed":true}\n')


if __name__ == "__main__":
    unittest.main()

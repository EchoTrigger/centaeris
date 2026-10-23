"""No-network parity checks for fixed Core Bash workspace traces."""
import hashlib
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

from .e2b_workspace_trace_test_support import (
    changed_bytes, expected_manifest, fixed_trace_case, trace_steps, trace_workload_report,
)


class E2BFixedWorkspaceTraceTests(unittest.TestCase):
    def test_case_selection_is_exact_and_rejects_unknown_values(self):
        for name in ('unchanged', 'reverted', 'smallChange', 'manyFiles'):
            self.assertEqual(fixed_trace_case(name), name)
        for value in (None, '', 'smallchange', 'all', 'unchanged ', [], {'case': 'unchanged'}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                fixed_trace_case(value)

    def test_manifest_has_fixed_size_counts_hashes_and_changes(self):
        expected = {
            'unchanged': (1, 8 * 1024 * 1024, 0),
            'reverted': (1, 8 * 1024 * 1024, 0),
            'smallChange': (1, 8 * 1024 * 1024, 4096),
            'manyFiles': (512, 512 * 4096, 4 * 4096),
        }
        for name, (files, size, changes) in expected.items():
            with self.subTest(case=name):
                before = expected_manifest(name, final=False)
                after = expected_manifest(name, final=True)
                self.assertEqual(len(before['files']), files)
                self.assertEqual(sum(item['sizeBytes'] for item in before['files']), size)
                self.assertEqual(changed_bytes(name), changes)
                self.assertEqual(after == before, changes == 0)
                self.assertTrue(all(not item['executable'] for item in after['files']))
                self.assertEqual(before, expected_manifest(name, final=False))
        self.assertEqual(expected_manifest('unchanged', final=False)['files'][0]['sha256'],
                         'sha256:8074442eeb8c97d8b297d0460512b235a2589f8040436701e7f7f2cf8ea4e918')

    def test_generated_commands_make_exact_corpus_then_case_change(self):
        for name in ('unchanged', 'reverted', 'smallChange', 'manyFiles'):
            with self.subTest(case=name), tempfile.TemporaryDirectory() as directory:
                for run_index in (0, 1):
                    steps = trace_steps(name, run_index)
                    self.assertEqual(len(steps), 2)
                    for tool, arguments in steps:
                        self.assertEqual(tool, 'bash')
                        self.assertEqual(set(arguments), {'command'})
                        argv = shlex.split(arguments['command'])
                        self.assertEqual(argv[:2], ['python3', '-c'])
                        completed = subprocess.run([sys.executable, *argv[1:]], cwd=directory,
                            capture_output=True, text=True, timeout=15, check=True)
                        report = json.loads(completed.stdout)
                        self.assertEqual(report['case'], name)
                        self.assertEqual(report['fileCount'], len(expected_manifest(name, final=run_index == 1)['files']))
                    actual = []
                    for path in sorted(Path(directory).rglob('*')):
                        if path.is_file():
                            content = path.read_bytes()
                            actual.append({'path': path.relative_to(directory).as_posix(), 'sizeBytes': len(content),
                                'sha256': 'sha256:' + hashlib.sha256(content).hexdigest(), 'executable': False})
                    self.assertEqual({'schema': 'workspace.snapshot.v1', 'files': actual},
                                     expected_manifest(name, final=run_index == 1))

    def test_b_rejects_incorrect_restore_and_unexpected_files(self):
        for name in ('unchanged', 'reverted', 'smallChange', 'manyFiles'):
            with self.subTest(case=name), tempfile.TemporaryDirectory() as directory:
                Path(directory, 'unexpected').write_bytes(b'wrong restored state')
                argv = shlex.split(trace_steps(name, 1)[0][1]['command'])
                result = subprocess.run([sys.executable, *argv[1:]], cwd=directory,
                    capture_output=True, timeout=15)
                self.assertNotEqual(result.returncode, 0)

    def test_report_identifies_fixed_seed_and_exact_manifest_without_io_claims(self):
        for name in ('unchanged', 'reverted', 'smallChange', 'manyFiles'):
            report = trace_workload_report(name)
            self.assertEqual(set(report), {'case', 'seed', 'initialManifestSha256',
                'finalManifestSha256', 'fileCount', 'logicalBytes', 'logicalChangedBytes'})
            self.assertEqual(report['seed'], 'centaeris-fixed-workspace-v1')
            self.assertEqual(report['case'], name)
            self.assertEqual(report['initialManifestSha256'] == report['finalManifestSha256'],
                             name in ('unchanged', 'reverted'))
            self.assertEqual(report['logicalChangedBytes'], changed_bytes(name))
        with self.assertRaises(ValueError):
            trace_workload_report('private-unknown-case')

    def test_trace_indices_are_bounded(self):
        for value in (-1, 2, True, None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                trace_steps('unchanged', value)

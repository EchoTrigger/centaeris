"""Discover complete package suites; fail closed on empty, omitted or skipped tests."""

import argparse
from collections import Counter
import os
from pathlib import Path
import sys
import tempfile
import unittest
import uuid


def test_ids(suite):
    for test in suite:
        if isinstance(test, unittest.TestSuite):
            yield from test_ids(test)
        else:
            yield test.id()


class AuditedResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.executed = Counter()
        self.audit_errors = []

    def startTest(self, test):
        self.executed[test.id()] += 1
        super().startTest(test)

    def wasSuccessful(self):
        return (super().wasSuccessful() and not self.skipped
                and not self.expectedFailures and not self.audit_errors)


class AuditedRunner(unittest.TextTestRunner):
    resultclass = AuditedResult

    def run(self, test):
        expected = Counter(test_ids(test))
        result = super().run(test)
        if not expected:
            result.audit_errors.append("No tests discovered")
        if any(count != 1 for count in expected.values()):
            result.audit_errors.append("Duplicate test identities discovered")
        if expected != result.executed:
            result.audit_errors.append(
                f"Discovery/execution mismatch: missing={expected - result.executed}; "
                f"extra={result.executed - expected}"
            )
        for error in result.audit_errors:
            self.stream.writeln(f"GATE FAILED: {error}")
        self.stream.writeln(
            f"Gate: discovered={sum(expected.values())}, executed={result.testsRun}, "
            f"skipped={len(result.skipped)}, expectedFailures={len(result.expectedFailures)}, "
            f"passed={result.wasSuccessful()}"
        )
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", choices=("api", "worker", "document_processor", "gate"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    directory = root / "packages" / args.package
    if args.package == "gate":
        directory = root / "scripts"
    sys.path.insert(0, str(directory))
    if args.package != "api":
        suite = unittest.TestLoader().discover(str(directory))
        return int(not AuditedRunner(verbosity=2).run(suite).wasSuccessful())

    # Use only the explicit TEST_POSTGRES_* endpoint, never the deployed database.
    # Each invocation owns a fresh database and a temporary storage directory.
    with tempfile.TemporaryDirectory(prefix="centaeris-api-ci-") as storage:
        os.environ.update(
            DJANGO_SETTINGS_MODULE="api.test_settings",
            STORAGE_ROOT=storage,
            RUNTIME_URL="http://127.0.0.1:1",
            REDIS_URL="redis://127.0.0.1:1/15",
        )
        import django
        django.setup()
        from django.conf import settings
        from django.test.runner import DiscoverRunner

        settings.DATABASES["default"]["TEST"]["NAME"] = "test_centaeris_ci_" + uuid.uuid4().hex[:12]

        class ApiRunner(DiscoverRunner):
            test_runner = AuditedRunner

            def suite_result(self, suite, result, **kwargs):
                return int(not result.wasSuccessful())

        return ApiRunner(verbosity=2, interactive=False).run_tests([str(directory)])


if __name__ == "__main__":
    sys.exit(main())

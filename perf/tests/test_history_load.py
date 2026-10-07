import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'harness'))
from history_load import database_delta


class HistoryLoadTests(unittest.TestCase):
    def test_rates_use_database_clock_and_preserve_raw_sessions(self):
        before = dict(epoch=100, sessions=20, statsReset='same')
        after = dict(epoch=120, sessions=60, statsReset='same')
        self.assertEqual(database_delta(before, after),
                         dict(seconds=20, sessions=40, connectionsPerSecond=2))

    def test_invalid_measurement_cannot_report_a_rate(self):
        before = dict(epoch=100, sessions=20, statsReset='same')
        for after in [dict(epoch=100, sessions=60, statsReset='same'),
                      dict(epoch=120, sessions=10, statsReset='same'),
                      dict(epoch=120, sessions=60, statsReset='reset')]:
            with self.assertRaises(ValueError):
                database_delta(before, after)

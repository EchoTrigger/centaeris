import unittest
from perf.harness.collect_observations import records


class ObservationCollectionTests(unittest.TestCase):
    def test_non_observation_logs_are_not_copied_to_evidence(self):
        text = 'raw private log\n{"other":"private"}\n[]\n{"schema":"workspace.perf.v1","phase":"rpc"}\n'
        self.assertEqual(list(records(text)), [{'schema': 'workspace.perf.v1', 'phase': 'rpc'}])

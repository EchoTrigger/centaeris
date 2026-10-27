import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('workload_budget', Path(__file__).with_name('workload_budget.py'))
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)


class FinalJobBudgetTests(unittest.TestCase):
    def test_durable_snapshot_budget_reserves_worker_settling_and_final_command(self):
        budget = module.workload_budget(4, 3, 300, 30000)
        self.assertEqual(module.FINAL_JOB_SETTLE_SECONDS, 5)
        self.assertEqual(budget['durableTimingSeconds'], 66)
        self.assertEqual(budget['coordinatorSeconds'], 6566)
        self.assertEqual(budget['k6']['scenarioSeconds'], 1230)


if __name__ == '__main__': unittest.main()

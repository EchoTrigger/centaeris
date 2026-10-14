import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('fixture_model', Path(__file__).with_name('model.py'))
model = importlib.util.module_from_spec(spec)
spec.loader.exec_module(model)


class HistoryFixtureTests(unittest.TestCase):
    def request(self, count, result='acceptance-history-ok'):
        return {'messages':[{'role':'user','content':'ACCEPTANCE_HISTORY'}]
            + [{'role':'tool','content':result} for _ in range(count)]}

    def test_history_requests70_actual_commands_then_completes(self):
        for count in range(70):
            name, args = model.next_call(self.request(count))
            self.assertEqual(name, 'bash')
            self.assertEqual(args, {'command':'printf acceptance-history-ok'})
        self.assertIsNone(model.next_call(self.request(70)))

    def test_failed_command_is_not_counted_as_completed_history(self):
        with self.assertRaisesRegex(ValueError, 'history command failed'):
            model.next_call(self.request(1, 'capacity rejection'))

    def test_explicit_history_capacity_after63_completions_finishes_run(self):
        request=self.request(63)
        request['messages'].append({'role':'tool','content':'task_history_capacity'})
        self.assertIsNone(model.next_call(request))

    def test_history_capacity_does_not_hide_prior_unrelated_failure(self):
        request=self.request(2,'unrelated failure')
        request['messages'].append({'role':'tool','content':'task_history_capacity'})
        with self.assertRaisesRegex(ValueError,'unrelated failure'):
            model.next_call(request)


if __name__ == '__main__':
    unittest.main()

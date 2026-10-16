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

    def test_history_capacity_is_a_failure_not_successful_completion(self):
        request=self.request(21)
        request['messages'].append({'role':'tool','content':'task_history_capacity'})
        with self.assertRaisesRegex(ValueError,'history command failed'):
            model.next_call(request)

    def test_long_run_requests1000_distinct_real_commands(self):
        messages=[{'role':'user','content':'ACCEPTANCE_LONG_RUN'}]
        for index in range(1000):
            command="printf '%%s\\n' %04d >> /mnt/data/retained-long-run-markers.txt; printf acceptance-long-%04d" % (index,index)
            self.assertEqual(model.next_call({'messages':messages}),('bash',{'command':command}))
            messages.append({'role':'tool','content':'acceptance-long-%04d' % index})
        self.assertIsNone(model.next_call({'messages':messages}))

    def test_long_run_progress_survives_compacted_tool_history(self):
        messages=[{'role':'user','content':'ACCEPTANCE_LONG_RUN nonce'},
            {'role':'tool','content':'acceptance-long-0499'}]
        self.assertEqual(model.next_call({'messages':messages}),
            ('bash',{'command':model.long_command(500)}))
        messages[-1]['content']='acceptance-long-0999'
        self.assertIsNone(model.next_call({'messages':messages}))

    def test_long_run_lost_progress_refuses_to_restart_sequence(self):
        marker='ACCEPTANCE_LONG_RUN fixture'
        state={}
        model.next_call({'messages':[{'role':'user','content':marker},
            {'role':'tool','content':'acceptance-long-0500'}]},state)
        with self.assertRaisesRegex(ValueError,'lost progress'):
            model.next_call({'messages':[{'role':'user','content':marker}]},state)
        with self.assertRaisesRegex(ValueError,'slots exhausted'):
            model.next_call({'messages':[{'role':'user','content':marker}]},
                {str(i):1 for i in range(16)})

    def test_long_run_rejects_missing_or_wrong_actual_output(self):
        for result in ('task_history_capacity','acceptance-long-9999',''):
            with self.subTest(result=result),self.assertRaises(ValueError):
                model.next_call({'messages':[{'role':'user','content':'ACCEPTANCE_LONG_RUN'},
                    {'role':'tool','content':result}]})

class AutomaticInstructionTests(unittest.TestCase):
    def test_actual_model_request_must_include_known_file_bytes(self):
        call=('bash',{'command':model.long_command(500)})
        request={'messages':[{'role':'system','content':model.LONG_INSTRUCTIONS+'\n'}]}
        self.assertEqual(model.instructions_observation(request,call)['commandIndex'],500)
        with self.assertRaisesRegex(ValueError,'lacks the actual AGENTS'):
            model.instructions_observation({'messages':[{'role':'user','content':'ACCEPTANCE_LONG_RUN'}]},call)

if __name__ == '__main__':
    unittest.main()

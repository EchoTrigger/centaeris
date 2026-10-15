import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
spec=importlib.util.spec_from_file_location('workload',Path(__file__).with_name('workload.py'))
workload=importlib.util.module_from_spec(spec);spec.loader.exec_module(workload)

class WorkloadFactsTests(unittest.TestCase):
    def records(self,count):
        rows=[]
        for i in range(count):
            rows.extend([SimpleNamespace(payload={'type':'tool_call','payload':{'toolName':'bash',
                'callId':'call-'+str(i),'normalizedInput':{'command':"printf '%%s\\n' %04d >> /mnt/data/retained-long-run-markers.txt; printf acceptance-long-%04d"%(i,i)}}}),
                SimpleNamespace(payload={'type':'tool_result','payload':{'callId':'call-'+str(i),
                    'resultState':'successWithOutput','modelContent':'acceptance-long-%04d'%i}})])
        return rows

    def test_counts_actual_unique_calls_and_matched_successful_outputs(self):
        self.assertEqual(workload.command_facts(self.records(1000),'acceptance-long-',1000),
            {'attemptedCommands':1000,'completedCommands':1000,'distinctCallIds':1000})

    def test_prompt_count_or_partial_or_duplicate_results_cannot_pass(self):
        rows=self.records(1000)
        for altered in (rows[:-1],rows+rows[-2:],rows[2:],rows[:-1]+[rows[1]]):
            with self.subTest(length=len(altered)),self.assertRaises(RuntimeError):
                workload.command_facts(altered,'acceptance-long-',1000)

    def test_wrong_output_and_non_success_reject(self):
        for replacement in ({'resultState':'successNoOutput','modelContent':''},
                            {'resultState':'successWithOutput','modelContent':'wrong'}):
            rows=self.records(1000);rows[-1].payload['payload'].update(replacement)
            with self.assertRaises(RuntimeError):workload.command_facts(rows,'acceptance-long-',1000)

class AutomaticInstructionsFactsTests(unittest.TestCase):
    def lines(self):
        import json
        return [json.dumps({'kind':'automaticInstructionsObserved','commandIndex':i,
            'instructionSha256':'actual-file-digest'}) for i in range(1000)]

    def test_all_actual_request_indices_and_file_digest_required(self):
        self.assertEqual(workload.instructions_facts(self.lines(),'actual-file-digest')['distinctCommandIndices'],1000)
        for lines,digest in ((self.lines()[:-1],'actual-file-digest'),
                             (self.lines(),'changed-file-digest')):
            with self.subTest(digest=digest),self.assertRaises(RuntimeError):
                workload.instructions_facts(lines,digest)

class AgentsManifestFactsTests(unittest.TestCase):
    def fixture(self):
        import hashlib,json
        def encode(value,domain):
            raw=json.dumps(value,separators=(',',':'))
            return 'sha256:'+hashlib.sha256(('centaeris.model_observation_'+domain+'.v1\0').encode()+raw.encode()).hexdigest(),raw
        content={'kind':'message','message':{'messageId':'msg:session:turn:agents_context',
            'role':'user','content':'# AGENTS.md instructions for /mnt/data\n\n<INSTRUCTIONS>\nknown file\n\n</INSTRUCTIONS>'}}
        digest,raw=encode(content,'content')
        root,manifest=encode({'parentDigest':None,'observationCount':1,
            'changes':[{'index':0,'kind':'message','contentDigest':digest}]},'manifest')
        records=[SimpleNamespace(payload={'type':'model_request_started','payload':{
            'purpose':'main','observations':{'manifestDigest':root}}}) for _ in range(1001)]
        return records,[(root,manifest)],[(digest,raw)]

    def test_authoritative_manifest_and_exact_agents_content_are_required(self):
        records,manifests,contents=self.fixture()
        facts=workload.agents_manifest_facts(records,manifests,contents,'known file\n')
        self.assertEqual(facts['mainRequestsWithAgentsInstructions'],1001)
        for rows in ([],[(contents[0][0],contents[0][1]+' ')],[(contents[0][0],contents[0][1].replace('known file','different'))]):
            with self.subTest(rows=rows),self.assertRaises(RuntimeError):
                workload.agents_manifest_facts(records,manifests,rows,'known file\n')

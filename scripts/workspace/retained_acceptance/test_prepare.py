import importlib.util
import json
import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('prepare', Path(__file__).with_name('prepare.py'))
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


class PreparationTests(unittest.TestCase):
    def fixture(self, root):
        source, bins = root / 'source', root / 'bins'
        source.mkdir()
        (source / '.git').write_text('fixture placeholder; git calls are isolated by mocks')
        bins.mkdir()
        (source / '.env.example').write_text('POSTGRES_DB=old\n')
        for name in ('runtime_server','execution_agent','environment_task_service'):
            (bins / name).write_bytes(name.encode())
        model = root / 'model.py'
        model.write_text('pass\n')
        manifest=root / 'build-manifest.json'
        manifest.write_text(json.dumps({'sourceRevision':'a'*40,'sourceTree':'c'*40,'binarySha256':{
            name:hashlib.sha256(name.encode()).hexdigest() for name in
            ('runtime_server','execution_agent','environment_task_service')}}))
        return ['prepare','--source',str(source),'--bins',str(bins),'--model-script',str(model),
                '--output',str(root / 'result'),'--revision','a' * 40,'--build-manifest',str(manifest)] + [
                    item for name in ('api','worker','runtime','general','postgres','redis')
                    for item in ('--'+name+'-image','fixture-image')]

    def test_foreign_inventory_refuses_before_writing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(sys, 'argv', self.fixture(root)), \
                    patch.object(prepare.subprocess,'run',return_value=subprocess.CompletedProcess([],0,'foreign\n','')):
                with self.assertRaisesRegex(SystemExit, 'managed sandboxes already exist'):
                    prepare.main()
            self.assertFalse((root / 'result').exists())

    def test_prepared_fixture_uses_independent_maintenance_authority_and_no_start(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(sys, 'argv', self.fixture(root)), \
                    patch.object(prepare.subprocess,'run',side_effect=[subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'sha256:'+'b'*64+'\n','')]*6
                        + [subprocess.CompletedProcess([],0,'a'*40,'')]
                        + [subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'c'*40,'')]) as run, \
                    patch('builtins.print'):
                prepare.main()
            self.assertEqual(run.call_count, 10)
            env = dict(line.split('=',1) for line in (root / 'result/synthetic.env').read_text().splitlines())
            self.assertGreaterEqual(len(env['ENVIRONMENT_MAINTENANCE_TOKEN'].encode()),32)
            self.assertNotEqual(env['ENVIRONMENT_MAINTENANCE_TOKEN'],env['INTERNAL_API_TOKEN'])
            compose = json.loads((root / 'result/compose.json').read_text())
            self.assertEqual(compose['services']['runtime']['ports'],['127.0.0.1:19001:9000'])
            self.assertTrue(compose['name'].startswith('retainedaccept'))


if __name__ == '__main__':
    unittest.main()

import importlib.util,struct,tempfile,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_launch',Path(__file__).with_name('launch.py'));launch=importlib.util.module_from_spec(spec);spec.loader.exec_module(launch)
class StaticHelperTests(unittest.TestCase):
 def elf(self,program_type):
  identity=b'\x7fELF\x02\x01'+bytes(10)
  header=struct.pack('<16sHHIQQQIHHHHHH',identity,2,62,1,0,64,0,0,64,56,1,0,0,0)
  return header+struct.pack('<IIQQQQQQ',program_type,0,0,0,0,0,0,0)
 def test_static_elf_program_headers_accepted_without_readelf_dependency(self):
  with tempfile.TemporaryDirectory() as tmp:
   path=Path(tmp)/'helper';path.write_bytes(self.elf(1));launch.static_helper(path)
 def test_dynamic_or_invalid_elf_rejected_before_any_daemon_start(self):
  for data in (self.elf(3),b'invalid'):
   with self.subTest(data=data[:6]),tempfile.TemporaryDirectory() as tmp:
    path=Path(tmp)/'helper';path.write_bytes(data)
    with self.assertRaises(RuntimeError):launch.static_helper(path)
class SharedArgumentTests(unittest.TestCase):
 def test_sparse_eight_parameters_are_forwarded_explicitly(self):
  self.assertEqual(launch.shared_arguments(8,400,125,2),['--model-wait-ms','400','--initial-stagger-ms','125','--expected-working-environments','2'])
  self.assertEqual(launch.shared_arguments(4),['--model-wait-ms','0','--initial-stagger-ms','0','--expected-working-environments','4'])
 def test_invalid_shared_budgets_reject_before_launch(self):
  for values in ((8,120001,0,None),(8,0,20000,None),(8,0,0,9),(8,0,0,0)):
   with self.subTest(values=values),self.assertRaises(ValueError):launch.shared_arguments(*values)
if __name__=='__main__':unittest.main()

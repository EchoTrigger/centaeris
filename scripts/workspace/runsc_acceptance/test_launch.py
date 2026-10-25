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
if __name__=='__main__':unittest.main()

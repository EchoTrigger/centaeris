import importlib.util
from pathlib import Path
import unittest

spec=importlib.util.spec_from_file_location('native_memory_profile',Path(__file__).with_name('memory_profile.py'))
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
memory_profile=module.memory_profile
workspace_memory_budget=module.workspace_memory_budget


class MemoryProfileTests(unittest.TestCase):
 def test_disabled_profile_and_both_positive_boundaries(self):
  self.assertEqual(memory_profile(4,3),{'memoryMiB':0,'minimumHoldMs':0})
  self.assertEqual(memory_profile(2,1,1,1),{'memoryMiB':1,'minimumHoldMs':1})
  self.assertEqual(memory_profile(64,1,1536,120000),{'memoryMiB':1536,'minimumHoldMs':120000})
 def test_invalid_pairs_bounds_and_shape_reject(self):
  for memory,hold in ((0,1),(1,0),(-1,1),(1,-1),(1537,1),(1,120001),(True,1),(1,True),(1.0,1),(1,'1')):
   with self.subTest(memory=memory,hold=hold),self.assertRaises(ValueError):memory_profile(8,1,memory,hold)
 def test_enabled_profile_requires_one_iteration_and_multiple_environments(self):
  for environments,iterations in ((1,1),(65,1),(8,0),(8,2),(8,True)):
   with self.subTest(environments=environments,iterations=iterations),self.assertRaises(ValueError):memory_profile(environments,iterations,1,1)
 def test_budget_defaults_preserve_sparse_semantics_and_explicit_bounds_are_inclusive(self):
  gib=1024**3
  self.assertEqual(workspace_memory_budget(4),8*gib)
  self.assertEqual(workspace_memory_budget(8),12*gib)
  for value in (4*gib,8*gib,12*gib):self.assertEqual(workspace_memory_budget(8,value),value)
  for value in (4*gib-1,12*gib+1,0,-1,True,8.0*gib,'8589934592'):
   with self.subTest(value=value),self.assertRaises(ValueError):workspace_memory_budget(8,value)


if __name__=='__main__':unittest.main()

import importlib.util,json,tempfile,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_isolation',Path(__file__).with_name('isolation.py'));isolation=importlib.util.module_from_spec(spec);spec.loader.exec_module(isolation)
class IsolationTests(unittest.TestCase):
 def fixture(self):
  root='/var/tmp/owned-native';namespace='runsc-'+'a'*12
  daemon=['dockerd','--host','unix://'+root+'/docker.sock','--data-root',root+'/data','--exec-root',root+'/exec','--pidfile',root+'/daemon.pid','--containerd',root+'/containerd.sock','--containerd-namespace',namespace,'--containerd-plugins-namespace',namespace+'-plugins']
  ctr=['containerd','--address',root+'/containerd.sock','--root',root+'/containerd-data','--state',root+'/containerd-state']
  return root,namespace,daemon,ctr
 def test_observed_processes_require_private_endpoint_roots_and_namespaces(self):
  root,ns,d,c=self.fixture();facts=isolation.validate_vectors(root,ns,ns+'-plugins',d,c)
  self.assertEqual(facts['runtimeRoot'],root+'/exec/runtime-runc/'+ns)
  self.assertEqual(facts['containerdSocket'],root+'/containerd.sock')
 def test_shared_containerd_or_namespace_is_rejected_before_product_use(self):
  for option,value in [('--containerd','/run/containerd/containerd.sock'),('--containerd-namespace','moby'),('--containerd-plugins-namespace','plugins.moby')]:
   root,ns,d,c=self.fixture();d[d.index(option)+1]=value
   with self.subTest(option=option),self.assertRaises(RuntimeError):isolation.validate_vectors(root,ns,ns+'-plugins',d,c)
 def test_owner_requires_raw_oci_observation_and_live_matching_process_arguments(self):
  root,ns,d,c=self.fixture()
  with tempfile.TemporaryDirectory() as tmp:
   path=Path(tmp)/'owner.json';owner={'root':root,'daemonPid':11,'containerdPid':12,'containerdNamespace':ns,'containerdPluginsNamespace':ns+'-plugins','privateContainerdVerified':True,'rawProbeRetired':True,'runtimeRoot':root+'/exec/runtime-runc/'+ns};path.write_text(json.dumps(owner))
   calls=[]
   def observe(pid):calls.append(pid);return {11:d,12:c}[pid]
   self.assertEqual(isolation.verify_owner(path,observe)['root'],root);self.assertEqual(calls,[11,12])
   owner['runtimeRoot']='/run/docker/runtime-runc/moby';path.write_text(json.dumps(owner))
   with self.assertRaises(RuntimeError):isolation.verify_owner(path,observe)


class StorageDriverIsolationTests(unittest.TestCase):
 fixture=IsolationTests.fixture
 def test_explicit_driver_matches_both_supported_cli_forms_and_rejects_mismatch(self):
  for driver in ('vfs','overlay2'):
   for flag in (['--storage-driver',driver],['--storage-driver='+driver]):
    root,ns,d,c=self.fixture();d+=flag
    isolation.validate_vectors(root,ns,ns+'-plugins',d,c,storage_driver=driver)
    with self.assertRaisesRegex(RuntimeError,'storage driver'):isolation.validate_vectors(root,ns,ns+'-plugins',d,c,storage_driver='overlay2' if driver=='vfs' else 'vfs')
 def test_duplicate_storage_flags_are_rejected_even_when_written_differently(self):
  root,ns,d,c=self.fixture();d+=['--storage-driver','vfs','--storage-driver=overlay2']
  with self.assertRaises(RuntimeError):isolation.validate_vectors(root,ns,ns+'-plugins',d,c,storage_driver='vfs')
 def test_declared_owner_driver_must_match_observation_and_live_process(self):
  root,ns,d,c=self.fixture();d+=['--storage-driver=vfs']
  with tempfile.TemporaryDirectory() as tmp:
   path=Path(tmp)/'owner.json';owner={'root':root,'daemonPid':11,'containerdPid':12,'containerdNamespace':ns,'containerdPluginsNamespace':ns+'-plugins','privateContainerdVerified':True,'rawProbeRetired':True,'runtimeRoot':root+'/exec/runtime-runc/'+ns,'requestedStorageDriver':'vfs','observedStorageDriver':'vfs'}
   observe=lambda pid:{11:d,12:c}[pid]
   path.write_text(json.dumps(owner));self.assertEqual(isolation.verify_owner(path,observe)['observedStorageDriver'],'vfs')
   owner['observedStorageDriver']='overlay2';path.write_text(json.dumps(owner))
   with self.assertRaisesRegex(RuntimeError,'storage driver'):isolation.verify_owner(path,observe)
   owner['observedStorageDriver']='vfs';owner['requestedStorageDriver']='overlay2';path.write_text(json.dumps(owner))
   with self.assertRaisesRegex(RuntimeError,'storage driver'):isolation.verify_owner(path,observe)

if __name__=='__main__':unittest.main()

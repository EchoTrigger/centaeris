"""Provider targets and physical resource facts without Harbor or provider IO."""
import copy
import hashlib
import json
from unittest import TestCase
from workspace_provider import ProviderTarget, OwnedProviderEnvironment, require_storage_probe

class ProviderOwnershipTests(TestCase):
    def profile(self):
        return {'schema':'runtime.e2b_environment_profile.v1','provider':'e2b','providerConfigDigest':'sha256:'+'f'*64,
            'templateRef':{'templateId':'task-template','buildId':'123e4567-e89b-12d3-a456-426614174000'},
            'workspaceRoot':'/app','user':'root','network':'publicInternet','lifetime':'retained',
            'timeoutSeconds':3600,'inputBudgetBytes':67108864,'workspaceInitialization':'taskImage'}
    def target(self):
        return ProviderTarget('e2b','task-create','instance_'+'a'*64,'sandbox-task','sha256:'+'b'*64,'sha256:'+'f'*64)
    def owned(self, **changes):
        p=self.profile()
        values=dict(session_id='session',environment_id='environment',agent_run_id=None,provider_target=self.target(),
            controller_epoch=2,template_id=p['templateRef']['templateId'],build_id=p['templateRef']['buildId'],
            profile_digest='sha256:'+hashlib.sha256(json.dumps(p,sort_keys=True,separators=(',',':')).encode()).hexdigest(),
            state='resident',resource_holding=1,cpu_count=1,memory_mib=2048)
        return OwnedProviderEnvironment(**(values|changes))
    def test_exact_provider_target_and_fresh_resource_facts_bind_original_profile(self):
        owned=self.owned()
        owned.require_profile(self.profile(),1,2048)
        self.assertEqual(owned.instance_id,self.target().instance_id)
        self.assertEqual(owned.sandbox_id,'sandbox-task')
        self.assertFalse(hasattr(owned,'container_id'))
        self.assertFalse(hasattr(owned,'host_id'))
        self.assertEqual(ProviderTarget.from_wire(self.target().to_wire()),self.target())
    def test_mismatched_target_build_and_actual_resources_are_rejected(self):
        for change in ({'cpu_count':2},{'memory_mib':1024},{'build_id':'123e4567-e89b-12d3-a456-426614174001'},
                {'template_id':'other'},{'profile_digest':'sha256:'+'c'*64}):
            with self.subTest(change=change),self.assertRaises(ValueError):
                self.owned(**change).require_profile(self.profile(),1,2048)
        for field,value in (('provider','docker'),('sandboxId','a/b'),('hostId','host')):
            with self.subTest(field=field),self.assertRaises(ValueError):
                ProviderTarget.from_wire(self.target().to_wire()|{field:value})
    def test_df_probe_enforces_minimum_available_space_without_claiming_upper_quota(self):
        self.assertEqual(require_storage_probe('Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/vda 20000000 1000 15000000 1% /\n',10240),15000000*1024)
        for output in ('', 'fake data', 'Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/vda 20000 1000 15000 1% /\n'):
            with self.subTest(output=output),self.assertRaises(ValueError):
                require_storage_probe(output,10240)

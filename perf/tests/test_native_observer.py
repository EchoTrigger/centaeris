import copy
import importlib.util
from pathlib import Path
import unittest

SOURCE=Path(__file__).resolve().parents[1]/'harness/native_observer.py'

class NativeObservationTests(unittest.TestCase):
    def subject(self):
        spec=importlib.util.spec_from_file_location('native_observer_subject',SOURCE)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
    def sample(self, tick):
        containers=[]
        for index in range(4):
            target={'containerId':str(index)*64,'environmentId':f'env-{index}', 'environmentInstanceId':f'instance-{index}', 'profileDigest':'sha256:'+'a'*64,'imageDigest':'sha256:'+'b'*64}
            containers.append({'state':'observed','resources':{'target':target,'initPid':100+index,'initStartTicks':500+index,
                'cgroupIdentity':{'path':f'/sys/fs/cgroup/test/{index}','device':1,'inode':1000+index},
                'cpuCounters':{'usageUsec':tick*150000}, 'memoryCurrentBytes':100000000}})
        return {'schema':'workspace.perf.native_sample.v1','host':{'schema':'native.host.resources.v1','hostId':'private-host','bootId':'a'*36,
            'observedAtBootMs':1000+tick*1000,'collectionDurationMs':5,'containers':containers},
            'states':{str(i)*64:{'running':True,'paused':False,'inspection':{'schema':'environment.task.store.inspection.v2','instanceId':f'instance-{i}','activeTasks':1},'caps':{'memoryBytes':4*1024**3,'cpuMilli':2000,'pids':512}} for i in range(4)}}
    def test_real_cpu_and_task_overlap_requires_two_consecutive_intervals(self):
        module=self.subject();samples=[self.sample(i) for i in range(3)]
        report=module.summarize(samples,4)
        self.assertEqual(report['physicalOverlap'],'verified');self.assertEqual(report['peakWorkingEnvironments'],4)
        self.assertEqual(report['consecutiveWorkingIntervals'],2)
        self.assertEqual(module.summarize(samples[:2],4)['physicalOverlap'],'notObserved')
    def test_unknown_identity_change_zero_cpu_or_no_active_task_cannot_count_as_four(self):
        module=self.subject()
        for defect in ('unknown','identity','cpu','active','paused','duplicate','caps'):
            samples=[self.sample(i) for i in range(3)]
            for sample in samples[1:]:
                item=sample['host']['containers'][3]
                if defect=='unknown': sample['host']['containers'][3]={'state':'unknown','target':item['resources']['target'],'reason':'probe_failed'}
                elif defect=='identity': item['resources']['cgroupIdentity']['inode']+=sample['host']['observedAtBootMs']
                elif defect=='cpu': item['resources']['cpuCounters']['usageUsec']=0
                elif defect=='active': sample['states']['3'*64]['inspection']['activeTasks']=0
                elif defect=='paused': sample['states']['3'*64]['paused']=True
                elif defect=='caps': sample['states']['3'*64]['caps']['memoryBytes']=1024**3
                else: item['resources']['target']['environmentId']='env-0'
            report=module.summarize(samples,4)
            self.assertEqual(report['physicalOverlap'],'notObserved',defect)
            self.assertLess(report['peakWorkingEnvironments'],4,defect)
    def test_host_boot_change_breaks_continuity_and_unknown_is_retained(self):
        module=self.subject();samples=[self.sample(i) for i in range(3)];samples[1]['host']['bootId']='changed'
        samples.append({'schema':'workspace.perf.native_sample.v1','state':'unknown','reason':'resource_probe_failed'})
        report=module.summarize(samples,4)
        self.assertEqual(report['physicalOverlap'],'notObserved');self.assertIn('resource_probe_failed',report['unknownReasons'])

    def test_sparse_gate_distinguishes_inventory_from_observed_working_target(self):
        module=self.subject();samples=[self.sample(i) for i in range(3)]
        for sample in samples:
            for index in (2,3):sample['states'][str(index)*64]['inspection']['activeTasks']=0
        report=module.summarize(samples,4,expected_working_environments=2)
        self.assertEqual(report['physicalOverlap'],'verified')
        self.assertEqual(report['expectedEnvironments'],4)
        self.assertEqual(report['expectedWorkingEnvironments'],2)
        self.assertEqual(report['peakWorkingEnvironments'],2)
        for target in (0,5):
            with self.assertRaises(ValueError):module.NativeObserver('/manifest','/output',4,expected_working_environments=target)

class NativeCollectorPausedTests(unittest.TestCase):
    def test_paused_container_is_sampled_without_requesting_running_task_inspection(self):
        import threading
        from unittest.mock import patch
        spec=importlib.util.spec_from_file_location('native_paused_subject',SOURCE)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        observer=module.NativeObserver('/manifest','/output',1);observer.root=Path('/private');observer.host_id='host';observer._stop=threading.Event()
        cid='a'*64
        labels={'centaeris.managed':'true','centaeris.environment_id':'env','centaeris.instance_id':'instance','centaeris.environment_profile_digest':'sha256:'+'b'*64}
        facts={'id':cid,'image':'sha256:'+'c'*64,'state':{'Running':True,'Paused':True},'labels':labels,'limits':{'Runtime':'runsc','Memory':4*1024**3,'CpuQuota':200000,'CpuPeriod':100000,'PidsLimit':512}}
        observer._docker=lambda args,deadline: cid if args[0]=='ps' else __import__('json').dumps(facts)
        requests=[]
        def rpc(request,deadline):
            requests.append(request)
            if request['operation']=='inspect':raise ValueError('running-only inspection rejects paused container')
            return {'kind':'resourceObservation','observation':{'schema':'native.host.resources.v1','hostId':'host','bootId':'test-boot','observedAtBootMs':1000,
                'containers':[{'state':'unknown','target':request['targets'][0],'reason':'test_unknown'}]}}
        observer._rpc=rpc
        class KernelFile:
            def __init__(self,path):self.path=path
            def read_text(self):return 'test-boot' if self.path.endswith('boot_id') else '1.0 1.0'
        with patch.object(module,'Path',KernelFile):sample=observer._sample()
        self.assertEqual([request['operation'] for request in requests],['observeResources'])
        self.assertTrue(sample['states'][cid]['paused']);self.assertIsNone(sample['states'][cid]['inspection'])

if __name__=='__main__':unittest.main()

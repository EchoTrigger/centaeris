import copy
import importlib.util
import json
import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

spec = importlib.util.spec_from_file_location('memory_contention', Path(__file__).with_name('memory_contention.py'))
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
GIB = 1024**3


class MemoryContentionTests(unittest.TestCase):
    def fixture(self):
        runs = [{'agentRunId': f'run-{i}', 'sessionId': f'session-{i}',
                 'environmentId': f'environment-{i}', 'instanceId': f'instance-{i}'} for i in range(8)]
        limits = {'global': {'count': 8, 'memoryBytes': 12*GIB, 'pids': 8192, 'workspaceBytes': 32*GIB},
                  'tenant': {'count': 8, 'memoryBytes': 8*GIB, 'pids': 2048, 'workspaceBytes': 32*GIB},
                  'hostHeadroomBytes': 3*GIB,
                  'requested': {'memoryBytes': 4*GIB, 'pids': 512, 'workspaceBytes': 4*GIB}}
        durable = {'finalJobs': [{'agentRunId': row['agentRunId'], 'sessionId': row['sessionId'],
                    'jobId': 'agent_run.lifecycle:'+row['agentRunId'], 'jobKind': 'agent_run.lifecycle',
                    'status': 'succeeded', 'retryCount': 0, 'maxRetries': 3} for row in runs],
                   'jobYields': [{'jobId': 'agent_run.lifecycle:run-7', 'agentRunId': 'run-7',
                    'sessionId': 'session-7', 'yieldedAtMs': 1000, 'runAtMs': 31000,
                    'transitionReason': 'execution_resident_capacity_wait'}],
                   'capacityClaims': [{'jobId': 'agent_run.lifecycle:run-7', 'agentRunId': 'run-7',
                    'generation': 2, 'wakeSource': 'physicalRelease', 'releaseRecordedAtMs': 5000, 'claimedAtMs': 5100}]}
        return [self.sample(1100), self.sample(2100)], durable, runs, limits

    def sample(self, start, *, memory=3*GIB//2):
        resources = []
        states = {}
        for i in range(3):
            container = str(i+1)*64
            resources.append({'state': 'observed', 'resources': {
                'target': {'containerId': container, 'environmentId': f'environment-{i}',
                    'environmentInstanceId': f'instance-{i}', 'profileDigest': 'sha256:'+'a'*64,
                    'imageDigest': 'sha256:'+'b'*64},
                'initPid': 100+i, 'initStartTicks': 500+i,
                'cgroupIdentity': {'path': f'/sys/fs/cgroup/private/{i}', 'device': 1, 'inode': 1000+i},
                'memoryCurrentBytes': memory, 'memorySwapCurrentBytes': 0, 'pidsCurrent': 20,
                'workspaceUsage': {'state': 'observed', 'bytes': 100*1024**2},
                'memoryEvents': {'oom': 0, 'oomKill': 0, 'oomGroupKill': None}}})
            states[container] = {'running': True, 'paused': False,
                                'caps': {'memoryBytes': 4*GIB, 'cpuMilli': 2000, 'pids': 512}}
        return {'schema': 'workspace.perf.native_sample.v1',
                'collectionWindow': {'startedAtUnixMs': start, 'completedAtUnixMs': start+100},
                'host': {'schema': 'native.host.resources.v1', 'hostId': 'host-a', 'bootId': 'boot-a',
                    'observedAtBootMs': start+90, 'collectionDurationMs': 50,
                    'memTotalBytes': 16*GIB, 'memAvailableBytes': 10*GIB, 'containers': resources},
                'states': states}

    def evaluate(self, fixture):
        return module.evaluate(*fixture)

    def test_real_tenant_pressure_and_same_job_release_wake_are_verified_without_low_water_claim(self):
        report = self.evaluate(self.fixture())
        self.assertEqual(report['status'], 'verified')
        proof = report['pressureProofs'][0]
        self.assertEqual(proof['jobId'], 'agent_run.lifecycle:run-7')
        self.assertEqual(proof['pressureSampleCount'], 2)
        self.assertEqual(proof['minimumMeasuredMemoryBytes'], 9*GIB//2)
        self.assertEqual(proof['postReleaseMemory'], 'notObserved')
        serialized = json.dumps(report)
        self.assertNotIn('/sys/fs/cgroup/private', serialized)
        self.assertNotIn('containerId', serialized)

    def test_low_water_is_observed_only_for_same_holder_inventory_and_identity(self):
        fixture = self.fixture()
        fixture[0].append(self.sample(5200, memory=GIB))
        low = self.evaluate(fixture)['pressureProofs'][0]['postReleaseMemory']
        self.assertEqual(low['measuredMemoryBytes'], 3*GIB)
        fixture[0][-1]['host']['containers'][0]['resources']['cgroupIdentity']['inode'] += 1
        self.assertEqual(self.evaluate(fixture)['pressureProofs'][0]['postReleaseMemory'], 'notObserved')

    def test_swap_and_workspace_byte_variants_are_charged(self):
        fixture = self.fixture()
        for sample in fixture[0]:
            for entry in sample['host']['containers']:
                entry['resources'].update(memoryCurrentBytes=GIB, memorySwapCurrentBytes=GIB//2,
                    workspaceUsage={'state': 'upperBound', 'bytes': 100*1024**2, 'source': 'containerWritableLayer'})
        self.assertEqual(self.evaluate(fixture)['status'], 'verified')
        for sample in fixture[0]:
            for entry in sample['host']['containers']:
                entry['resources']['workspaceUsage'] = {'state': 'unknown', 'reason': 'private error'}
        proof = self.evaluate(fixture)['pressureProofs'][0]
        self.assertEqual(proof['maximumWorkspaceChargeBytes'], 12*GIB)

    def test_missing_single_unknown_or_low_pressure_samples_cannot_prove_contention(self):
        for change in ('empty', 'single', 'unknown', 'partial', 'low', 'missingSwap', 'missingPids'):
            with self.subTest(change=change):
                fixture = self.fixture()
                if change == 'empty': fixture[0].clear()
                elif change == 'single': fixture[0].pop()
                elif change == 'unknown': fixture[0][1] = {'state': 'unknown', 'reason': 'private error'}
                elif change == 'partial': fixture[0][1]['host']['containers'][0]['state'] = 'unknown'
                elif change == 'low': fixture[0][1] = self.sample(2100, memory=GIB)
                else: fixture[0][1]['host']['containers'][0]['resources'].pop('memorySwapCurrentBytes' if change == 'missingSwap' else 'pidsCurrent')
                with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_unknown_gap_and_nonconsecutive_pressure_cannot_be_joined(self):
        fixture = self.fixture()
        fixture[0].insert(1, {'state': 'unknown', 'reason': 'private details'})
        with self.assertRaises(ValueError): self.evaluate(fixture)
        fixture = self.fixture()
        fixture[0].insert(1, self.sample(1600, memory=GIB))
        with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_full_collection_window_must_be_inside_wait_before_release(self):
        for start, end in ((999, 1200), (2100, 5001), (2201, 2200), (2100, True)):
            with self.subTest(start=start, end=end):
                fixture = self.fixture()
                fixture[0][1]['collectionWindow'] = {'startedAtUnixMs': start, 'completedAtUnixMs': end}
                with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_clock_boot_host_or_resource_identity_changes_break_continuity(self):
        for change in ('wall', 'bootClock', 'boot', 'host', 'instance', 'cgroup', 'init', 'inventory'):
            with self.subTest(change=change):
                fixture = self.fixture(); sample = fixture[0][1]
                if change == 'wall': sample['collectionWindow'] = {'startedAtUnixMs': 1050, 'completedAtUnixMs': 1090}
                elif change == 'bootClock': sample['host']['observedAtBootMs'] = 1000
                elif change == 'boot': sample['host']['bootId'] = 'other-boot'
                elif change == 'host': sample['host']['hostId'] = 'other-host'
                elif change == 'instance': sample['host']['containers'][0]['resources']['target']['environmentInstanceId'] = 'other-instance'
                elif change == 'cgroup': sample['host']['containers'][0]['resources']['cgroupIdentity']['inode'] += 1
                elif change == 'init': sample['host']['containers'][0]['resources']['initStartTicks'] += 1
                else: sample['host']['containers'].pop()
                with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_waiting_target_or_foreign_seven_way_inventory_cannot_supply_pressure(self):
        for index in (7, 99):
            fixture = self.fixture()
            for sample in fixture[0]:
                target = sample['host']['containers'][0]['resources']['target']
                target.update(environmentId=f'environment-{index}', environmentInstanceId=f'instance-{index}')
            with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_other_admission_bottlenecks_disqualify_memory_claim(self):
        changes = [lambda f: f[3]['global'].update(memoryBytes=8*GIB),
            lambda f: f[3]['global'].update(count=3), lambda f: f[3]['tenant'].update(count=3),
            lambda f: f[3]['global'].update(pids=550), lambda f: f[3]['tenant'].update(pids=550),
            lambda f: f[3]['global'].update(workspaceBytes=4*GIB), lambda f: f[3]['tenant'].update(workspaceBytes=4*GIB),
            lambda f: [s['host'].update(memAvailableBytes=6*GIB) for s in f[0]],
            lambda f: f[3].update(hostHeadroomBytes=7*GIB)]
        for index, change in enumerate(changes):
            with self.subTest(index=index):
                fixture = self.fixture(); change(fixture)
                with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_any_observed_oom_is_rejected(self):
        for field in ('oom', 'oomKill', 'oomGroupKill'):
            fixture = self.fixture()
            fixture[0][0]['host']['containers'][0]['resources']['memoryEvents'][field] = 1
            with self.subTest(field=field), self.assertRaises(ValueError): self.evaluate(fixture)
        fixture = self.fixture()
        partial = self.sample(3100)
        partial['host']['containers'][0]['state'] = 'unknown'
        partial['host']['containers'][1]['resources']['memoryEvents']['oomKill'] = 1
        fixture[0].append(partial)
        with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_durable_preparation_fallback_wrong_bindings_or_failed_retries_are_rejected(self):
        changes = [lambda f: f[1]['jobYields'][0].update(transitionReason='execution_host_preparation_wait'),
            lambda f: f[1]['capacityClaims'][0].update(wakeSource='hostPreparation'),
            lambda f: f[1]['capacityClaims'][0].update(claimedAtMs=31000),
            lambda f: f[1]['capacityClaims'][0].update(releaseRecordedAtMs=999),
            lambda f: f[1]['jobYields'][0].update(sessionId='other-session'),
            lambda f: f[1]['capacityClaims'][0].update(agentRunId='run-6'),
            lambda f: f[1]['finalJobs'][7].update(retryCount=1),
            lambda f: f[1]['finalJobs'][7].update(status='running'),
            lambda f: f[1]['finalJobs'][7].update(sessionId='other-session'),
            lambda f: f[1]['finalJobs'].pop(),
            lambda f: f[1]['finalJobs'].append(f[1]['finalJobs'][0].copy()),
            lambda f: f[2][7].update(instanceId='instance-0')]
        for index, change in enumerate(changes):
            with self.subTest(index=index):
                fixture = self.fixture(); change(fixture)
                with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_unrelated_valid_claim_does_not_complete_waiting_jobs_chain(self):
        fixture = self.fixture()
        fixture[1]['capacityClaims'][0].update(jobId='agent_run.lifecycle:run-6', agentRunId='run-6')
        with self.assertRaises(ValueError): self.evaluate(fixture)

    def test_errors_do_not_echo_raw_content(self):
        fixture = self.fixture()
        fixture[1]['jobYields'][0]['transitionReason'] = 'private password token'
        with self.assertRaises(ValueError) as caught: self.evaluate(fixture)
        self.assertNotIn('private password token', str(caught.exception))


class PerRunCleanupContentionTests(unittest.TestCase):
    def fixture(self):
        base = MemoryContentionTests().fixture()
        samples, durable, runs, limits = base
        durable['capacityClaims'][0] = {'jobId':'agent_run.lifecycle:run-7','agentRunId':'run-7',
            'generation':2,'wakeSource':'hostPreparation','hostId':'host-a','availabilityScope':'globalPreparation',
            'hostPreparationAvailableAtMs':5000,'hostPreparationToClaimMs':100,'claimedAtMs':5100}
        cleanup = [{**row,'containerId':str(i+1)*64,'cleanupComplete':True,'physicalRemoved':True,
            'containerAbsent':True,'residentHoldingCount':0,'historyPreserved':True,
            'cleanupReceiptAtMs':4000 if i==0 else 9000} for i,row in enumerate(runs)]
        return base, cleanup

    def evaluate(self, fixture):
        base, cleanup = fixture
        return module.evaluate(*base, per_run_cleanup=cleanup)

    def test_original_pressure_holder_cleanup_and_host_preparation_recheck_are_distinct_evidence(self):
        report = self.evaluate(self.fixture())
        proof = report['pressureProofs'][0]
        self.assertEqual(proof['wakeSource'],'hostPreparation')
        self.assertEqual(proof['hostPreparationAvailableAtMs'],5000)
        self.assertEqual(proof['hostPreparationToClaimMs'],100)
        self.assertEqual(proof['physicalCleanup']['agentRunId'],'run-0')
        self.assertEqual(proof['physicalCleanup']['instanceId'],'instance-0')
        self.assertEqual(proof['physicalCleanup']['containerId'],'1'*64)
        self.assertEqual(proof['physicalCleanup']['cleanupReceiptAtMs'],4000)
        self.assertEqual(proof['physicalCleanupToAvailabilityMs'],1000)
        self.assertEqual(proof['pressureSampleCount'],2)
        self.assertNotIn('releaseRecordedAtMs',proof)
        self.assertNotIn('postReleaseMemory',proof)

    def test_host_preparation_alone_or_unrelated_complete_cleanup_does_not_prove_release(self):
        base,cleanup = self.fixture()
        with self.assertRaises(ValueError):module.evaluate(*base)
        for change in ('unrelatedHolder','wrongRun','wrongSession','wrongEnvironment','wrongInstance','wrongContainer',
                'missingCleanup','duplicateCleanup','physicalUnknown','incomplete','present','holding','history',
                'booleanHolding','booleanReceipt'):
            fixture=self.fixture();base,proofs=fixture
            if change=='unrelatedHolder':proofs[0]['cleanupReceiptAtMs']=9000;proofs[6]['cleanupReceiptAtMs']=4000
            elif change=='missingCleanup':proofs.pop()
            elif change=='duplicateCleanup':proofs.append(proofs[0].copy())
            elif change=='wrongRun':proofs[0]['agentRunId']='run-6'
            elif change=='wrongSession':proofs[0]['sessionId']='session-6'
            elif change=='wrongEnvironment':proofs[0]['environmentId']='environment-6'
            elif change=='wrongInstance':proofs[0]['instanceId']='instance-6'
            elif change=='wrongContainer':proofs[0]['containerId']='f'*64
            elif change=='physicalUnknown':proofs[0]['physicalRemoved']=False
            elif change=='incomplete':proofs[0]['cleanupComplete']=False
            elif change=='present':proofs[0]['containerAbsent']=False
            elif change=='holding':proofs[0]['residentHoldingCount']=1
            elif change=='history':proofs[0]['historyPreserved']=False
            elif change=='booleanHolding':proofs[0]['residentHoldingCount']=False
            else:proofs[0]['cleanupReceiptAtMs']=True
            with self.subTest(change=change),self.assertRaises(ValueError):self.evaluate(fixture)

    def test_pressure_cleanup_availability_claim_and_fallback_order_must_all_hold(self):
        for change in ('cleanupBeforePressureEnd','cleanupBeforeYield','cleanupAfterAvailability','availabilityAfterClaim',
                'availabilityBeforeYield','claimAtFallback','wrongHost','wrongScope','wrongJob','wrongRun','badDelay','badGeneration'):
            fixture=self.fixture();base,cleanup=fixture;claim=base[1]['capacityClaims'][0]
            if change=='cleanupBeforePressureEnd':cleanup[0]['cleanupReceiptAtMs']=2150
            elif change=='cleanupBeforeYield':cleanup[0]['cleanupReceiptAtMs']=999
            elif change=='cleanupAfterAvailability':cleanup[0]['cleanupReceiptAtMs']=5001
            elif change=='availabilityAfterClaim':claim.update(hostPreparationAvailableAtMs=5101,hostPreparationToClaimMs=-1)
            elif change=='availabilityBeforeYield':claim.update(hostPreparationAvailableAtMs=999,hostPreparationToClaimMs=4101)
            elif change=='claimAtFallback':claim.update(claimedAtMs=31000,hostPreparationToClaimMs=26000)
            elif change=='wrongHost':claim['hostId']='other-host'
            elif change=='wrongScope':claim['availabilityScope']='other'
            elif change=='wrongJob':claim['jobId']='agent_run.lifecycle:run-6'
            elif change=='wrongRun':claim['agentRunId']='run-6'
            elif change=='badDelay':claim['hostPreparationToClaimMs']=99
            else:claim['generation']=True
            with self.subTest(change=change),self.assertRaises(ValueError):self.evaluate(fixture)

    def early_claim(self):
        return {'jobId':'agent_run.lifecycle:run-7','agentRunId':'run-7','generation':1,
            'wakeSource':'hostPreparation','hostId':'host-a','availabilityScope':'globalPreparation',
            'hostPreparationAvailableAtMs':2400,'hostPreparationToClaimMs':100,'claimedAtMs':2500}

    def test_a_completed_wait_episode_cannot_lend_pressure_to_a_later_claim(self):
        for path in ('hostPreparation','physicalRelease'):
            for later_yield in (False,True):
                fixture=self.fixture();base,cleanup=fixture
                if path=='physicalRelease':base[1]['capacityClaims'][0]=MemoryContentionTests().fixture()[1]['capacityClaims'][0]
                # Deliberately reverse chronological order: matching must use the
                # earliest observed claim, even when it cannot establish release.
                base[1]['capacityClaims'].append(self.early_claim())
                if later_yield:
                    later=copy.deepcopy(base[1]['jobYields'][0]);later.update(yieldedAtMs=3500,runAtMs=33500)
                    base[1]['jobYields'].append(later)
                with self.subTest(path=path,laterYield=later_yield),self.assertRaises(ValueError):
                    module.evaluate(*base,**({'per_run_cleanup':cleanup} if path=='hostPreparation' else {}))

    def test_any_next_yield_closes_the_pressure_window_even_without_a_recorded_earlier_claim(self):
        for path in ('hostPreparation','physicalRelease'):
            for reason in ('execution_host_preparation_wait','question_wait'):
                fixture=self.fixture();base,cleanup=fixture
                if path=='physicalRelease':base[1]['capacityClaims'][0]=MemoryContentionTests().fixture()[1]['capacityClaims'][0]
                later=copy.deepcopy(base[1]['jobYields'][0]);later.update(yieldedAtMs=3500,runAtMs=33500,transitionReason=reason)
                base[1]['jobYields'].append(later)
                with self.subTest(path=path,reason=reason),self.assertRaises(ValueError):
                    module.evaluate(*base,**({'per_run_cleanup':cleanup} if path=='hostPreparation' else {}))

    def test_reclaimed_same_job_needs_fresh_pressure_in_its_current_wait_episode(self):
        for path in ('hostPreparation','physicalRelease'):
            fixture=self.fixture();base,cleanup=fixture
            base[0].extend([MemoryContentionTests().sample(4100),MemoryContentionTests().sample(5100)])
            later=copy.deepcopy(base[1]['jobYields'][0]);later.update(yieldedAtMs=3500,runAtMs=33500)
            base[1]['jobYields'].append(later)
            if path=='hostPreparation':
                cleanup[0]['cleanupReceiptAtMs']=6000
                base[1]['capacityClaims'][0].update(hostPreparationAvailableAtMs=7000,claimedAtMs=7100)
            else:
                base[1]['capacityClaims'][0]={'jobId':'agent_run.lifecycle:run-7','agentRunId':'run-7',
                    'generation':2,'wakeSource':'physicalRelease','releaseRecordedAtMs':6000,'claimedAtMs':7100}
            base[1]['capacityClaims'].append(self.early_claim())
            report=module.evaluate(*base,**({'per_run_cleanup':cleanup} if path=='hostPreparation' else {}))
            with self.subTest(path=path):
                self.assertEqual(len(report['pressureProofs']),1)
                self.assertEqual(report['pressureProofs'][0]['pressureSampleCount'],2)
                self.assertEqual(report['pressureProofs'][0]['yieldedAtMs'],3500)
                self.assertEqual(report['matchingYieldProofs'],1)
                self.assertEqual(report['uniqueWaitingJobs'],1)
                self.assertEqual(report['uniqueWakeClaims'],1)

    def test_host_recheck_does_not_relax_pressure_continuity_other_budgets_or_oom(self):
        for change in ('single','unknown','low','cgroup','oom','globalMemory','count','pids','workspace','headroom'):
            fixture=self.fixture();base,cleanup=fixture;samples,durable,runs,limits=base
            if change=='single':samples.pop()
            elif change=='unknown':samples.insert(1,{'state':'unknown'})
            elif change=='low':samples[1]=MemoryContentionTests().sample(2100,memory=GIB)
            elif change=='cgroup':samples[1]['host']['containers'][0]['resources']['cgroupIdentity']['inode']+=1
            elif change=='oom':samples[0]['host']['containers'][0]['resources']['memoryEvents']['oomKill']=1
            elif change=='globalMemory':limits['global']['memoryBytes']=8*GIB
            elif change=='count':limits['tenant']['count']=3
            elif change=='pids':limits['tenant']['pids']=550
            elif change=='workspace':limits['tenant']['workspaceBytes']=4*GIB
            else:
                for sample in samples:sample['host']['memAvailableBytes']=6*GIB
            with self.subTest(change=change),self.assertRaises(ValueError):self.evaluate(fixture)

if __name__ == '__main__': unittest.main()

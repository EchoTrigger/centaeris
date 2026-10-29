"""Fixed two-process CPU/file work shared by mock requests and external expectations."""
import hashlib
import json
from pathlib import Path
import re
import shlex

FILENAME = 'perf-result.bin'
MARKER = 'PERF_FIXED_WORKLOAD '
FIELDS = {'sampleId', 'operationId', 'workUnits'}
MEMORY_FIELDS = {'memoryMiB', 'minimumHoldMs'}
BLOCK_BYTES = 1024 * 1024


def validate_sample(sample):
    if (not isinstance(sample, dict) or set(sample) not in (FIELDS, FIELDS | MEMORY_FIELDS)
            or any(not isinstance(sample[key], str) or not re.fullmatch('[a-f0-9]{32}', sample[key])
                   for key in ('sampleId', 'operationId'))
            or type(sample['workUnits']) is not int or not 1 <= sample['workUnits'] <= 1000000
            or ('memoryMiB' in sample and (type(sample['memoryMiB']) is not int
                or not 1 <= sample['memoryMiB'] <= 1536
                or type(sample['minimumHoldMs']) is not int
                or not 1 <= sample['minimumHoldMs'] <= 120000))):
        raise ValueError('fixed workload sample invalid')
    return sample


def block(sample_id, index):
    seed = (sample_id + ':' + str(index)).encode()
    return (seed * (BLOCK_BYTES // len(seed) + 1))[:BLOCK_BYTES]


def artifact_bytes(sample):
    validate_sample(sample)
    value = {'schema': 'workspace.perf.fixed_result.v1', 'sampleId': sample['sampleId'],
             'cpuWorkers': 2, 'completedWorkUnits': [sample['workUnits']] * 2,
             'workerDigests': [hashlib.sha256(block(sample['sampleId'], i)).hexdigest() for i in range(2)]}
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def expected_artifact(sample):
    data = artifact_bytes(sample)
    return {'filename': FILENAME, 'sizeBytes': len(data), 'sha256': 'sha256:' + hashlib.sha256(data).hexdigest()}


WORKER = '''import hashlib,sys
sample,units,index=sys.argv[1:];seed=(sample+':'+index).encode();data=(seed*(1048576//len(seed)+1))[:1048576]
for _ in range(int(units)):digest=hashlib.sha256(data).hexdigest()
print(digest)
'''
PROGRAM = '''import json,mmap,pathlib,subprocess,sys,time
sample,units,directory,*profile=sys.argv[1:];units=int(units)
memory_mib=int(profile[0]) if profile else 0
minimum_hold_ms=int(profile[1]) if profile else 0
worker=WORKER_SOURCE
children=[];memory=None
try:
 if memory_mib:
  memory_bytes=memory_mib*1048576;memory=bytearray(memory_bytes)
  for offset in range(0,memory_bytes,mmap.PAGESIZE):memory[offset]=1
  hold_until=time.monotonic()+minimum_hold_ms/1000
 for index in range(2):
  children.append(subprocess.Popen([sys.executable,'-c',worker,sample,str(units),str(index)],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE))
 if memory_mib:print('PERF_FIXED_WORK_READY '+sample,flush=True)
 digests=[]
 for child in children:
  stdout,stderr=child.communicate(timeout=300)
  if child.returncode:raise RuntimeError('fixed CPU work failed')
  digest=stdout.decode().strip()
  if len(digest)!=64:raise RuntimeError('fixed CPU digest invalid')
  digests.append(digest)
 if memory_mib:
  remaining=hold_until-time.monotonic()
  if remaining>0:time.sleep(remaining)
 value={'schema':'workspace.perf.fixed_result.v1','sampleId':sample,'cpuWorkers':2,'completedWorkUnits':[units]*2,'workerDigests':digests}
 data=(json.dumps(value,sort_keys=True,separators=(',',':'))+'\\n').encode();path=pathlib.Path(directory)/'perf-result.bin';path.write_bytes(data)
 print('PERF_FIXED_WORK_COMPLETE '+sample)
finally:
 try:
  for child in children:
   try:
    if child.poll() is None:child.kill();child.wait(timeout=10)
   finally:
    child.stdout.close();child.stderr.close()
 finally:
  memory=None
'''.replace('WORKER_SOURCE', repr(WORKER))


def command_for(sample, *, output_directory='/mnt/data'):
    validate_sample(sample)
    args = ['python3', '-c', PROGRAM, sample['sampleId'], str(sample['workUnits']), str(output_directory)]
    if 'memoryMiB' in sample:
        args.extend([str(sample['memoryMiB']), str(sample['minimumHoldMs'])])
    return shlex.join(args)


def sample_from_text(text):
    if not isinstance(text, str) or not text.startswith(MARKER):
        raise ValueError('fixed workload marker missing')
    try:
        sample = json.loads(text[len(MARKER):])
    except ValueError as error:
        raise ValueError('fixed workload JSON invalid') from error
    return validate_sample(sample)


RECOVERY_MARKER = 'PERF_WORKSPACE_RECOVERY '


def validate_recovery(sample):
    if (not isinstance(sample, dict) or set(sample) != {'schema', 'caseId', 'phase'}
            or sample['schema'] != 'workspace.recovery.fixture.v1'
            or not isinstance(sample['caseId'], str) or not re.fullmatch('[a-f0-9]{32}', sample['caseId'])
            or sample['phase'] not in ('write', 'readAndAdvance', 'read')):
        raise ValueError('strict workspace recovery fixture required')
    return sample


def recovery_sample(text):
    if not isinstance(text, str) or not text.startswith(RECOVERY_MARKER):
        raise ValueError('workspace recovery marker required')
    return validate_recovery(json.loads(text[len(RECOVERY_MARKER):]))


def recovery_bytes(case_id):
    validate_recovery({'schema': 'workspace.recovery.fixture.v1', 'caseId': case_id, 'phase': 'write'})
    return b'\x00Centaeris workspace recovery ' + case_id.encode() + b'\xff\n'


def recovery_expectation(case_id):
    data = recovery_bytes(case_id)
    return {'filename': 'recovery-' + case_id + '.bin', 'sizeBytes': len(data),
            'sha256': 'sha256:' + hashlib.sha256(data).hexdigest()}


def recovery_command(sample, *, output_directory='/mnt/data'):
    validate_recovery(sample)
    expected = recovery_expectation(sample['caseId'])
    if sample['phase'] == 'write':
        program = """import pathlib,sys
root=pathlib.Path(sys.argv[1]);name,data=sys.argv[2:]
with (root/name).open('xb') as stream:stream.write(bytes.fromhex(data))
(root/(name+'.churn')).write_bytes(b'first\\n')
"""
        arguments = [output_directory, expected['filename'], recovery_bytes(sample['caseId']).hex()]
    else:
        # Reader commands receive only a digest, never the bytes needed to reconstruct a missing file.
        program = """import hashlib,pathlib,sys
root=pathlib.Path(sys.argv[1]);name,digest,phase=sys.argv[2:]
data=(root/name).read_bytes()
if hashlib.sha256(data).hexdigest()!=digest:raise RuntimeError('restored sentinel digest mismatch')
if phase=='readAndAdvance':(root/(name+'.churn')).write_bytes(b'second\\n')
print('WORKSPACE_RECOVERY_READ '+hashlib.sha256(data).hexdigest())
"""
        arguments = [output_directory, expected['filename'], expected['sha256'][7:], sample['phase']]
    return shlex.join(['python3', '-c', program, *arguments])

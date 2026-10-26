"""Fixed two-process CPU/file work shared by mock requests and external expectations."""
import hashlib
import json
from pathlib import Path
import re
import shlex

FILENAME = 'perf-result.bin'
MARKER = 'PERF_FIXED_WORKLOAD '
FIELDS = {'sampleId', 'operationId', 'workUnits'}
BLOCK_BYTES = 1024 * 1024


def validate_sample(sample):
    if (not isinstance(sample, dict) or set(sample) != FIELDS
            or any(not isinstance(sample[key], str) or not re.fullmatch('[a-f0-9]{32}', sample[key])
                   for key in ('sampleId', 'operationId'))
            or type(sample['workUnits']) is not int or not 1 <= sample['workUnits'] <= 1000000):
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
PROGRAM = '''import json,pathlib,subprocess,sys
sample,units,directory=sys.argv[1:];units=int(units)
worker=WORKER_SOURCE
children=[subprocess.Popen([sys.executable,'-c',worker,sample,str(units),str(index)],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE) for index in range(2)]
try:
 digests=[]
 for child in children:
  stdout,stderr=child.communicate(timeout=300)
  if child.returncode:raise RuntimeError('fixed CPU work failed')
  digest=stdout.decode().strip()
  if len(digest)!=64:raise RuntimeError('fixed CPU digest invalid')
  digests.append(digest)
 value={'schema':'workspace.perf.fixed_result.v1','sampleId':sample,'cpuWorkers':2,'completedWorkUnits':[units]*2,'workerDigests':digests}
 data=(json.dumps(value,sort_keys=True,separators=(',',':'))+'\\n').encode();path=pathlib.Path(directory)/'perf-result.bin';path.write_bytes(data)
 print('PERF_FIXED_WORK_COMPLETE '+sample)
finally:
 for child in children:
  if child.poll() is None:child.kill();child.wait(timeout=10)
'''.replace('WORKER_SOURCE', repr(WORKER))


def command_for(sample, *, output_directory='/mnt/data'):
    validate_sample(sample)
    return shlex.join(['python3', '-c', PROGRAM, sample['sampleId'], str(sample['workUnits']), str(output_directory)])


def sample_from_text(text):
    if not isinstance(text, str) or not text.startswith(MARKER):
        raise ValueError('fixed workload marker missing')
    try:
        sample = json.loads(text[len(MARKER):])
    except ValueError as error:
        raise ValueError('fixed workload JSON invalid') from error
    return validate_sample(sample)

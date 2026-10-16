"""Bounded cross-Run observation and exact-fixture retry inputs."""
import json
from pathlib import Path
import re
import time


def wait_for_release(admitted, status, observe, *, clock=time.monotonic,
                     sleep=time.sleep, total_seconds=900, idle_seconds=120):
    start=last_progress=clock()
    previous=None
    while True:
        sample=status()
        if (sample.get('agentRunId')!=admitted['agentRunId']
                or sample.get('sessionId')!=admitted['sessionId']):
            raise RuntimeError('Run observation identity mismatch')
        if sample.get('coreTerminalType') in ('agent_run_failed','agent_run_interrupted'):
            raise RuntimeError('Run terminal failure: '+sample['coreTerminalType'])
        if sample.get('runStatus') not in ('queued','running','completed'):
            raise RuntimeError('Run terminal failure: '+str(sample.get('runStatus')))
        sequence=sample.get('highestRunSequence')
        release=sample.get('releaseReceiptCount')
        cleanup=sample.get('cleanupComplete')
        if (type(sequence) is not int or sequence<0 or type(release) is not int
                or release not in (0,1) or (cleanup is not None and type(cleanup) is not bool)):
            raise RuntimeError('invalid Run progress facts')
        progress=(sequence,release,cleanup is True)
        if previous is not None and any(a<b for a,b in zip(progress,previous)):
            raise RuntimeError('Run progress regressed')
        now=clock()
        if now-start>=total_seconds:
            raise RuntimeError('Run total deadline exceeded; fixture preserved')
        if progress!=previous:
            previous=progress;last_progress=now
        if now-last_progress>=idle_seconds:
            raise RuntimeError('Run made no progress; fixture preserved')
        if sample['runStatus']=='completed' and release==1 and cleanup is True:
            # The original full observer remains authoritative. Do not retry a
            # corrupt result, missing output or physical continuity violation.
            return observe()
        sleep(5)


def load_resume(root, evidence, expected_container):
    root=Path(root).resolve(strict=True);evidence=Path(evidence)
    if evidence.is_symlink():raise RuntimeError('resume evidence cannot be a symlink')
    evidence=evidence.resolve(strict=True)
    if evidence.parent!=root or not re.fullmatch(r'evidence-[0-9a-f]{32}',evidence.name):
        raise RuntimeError('resume evidence must belong to this fixture')
    if any((evidence/name).exists() for name in ('retirement-request.json','summary.json')):
        raise RuntimeError('cannot resume after retirement has started')
    def admission(prefix):
        paths=list(evidence.glob(prefix+'-*.json'))
        if len(paths)!=1:raise RuntimeError('resume admission identity is missing or ambiguous: '+prefix)
        value=json.loads(paths[0].read_text())
        if value.get('status')!='accepted' or not value.get('agentRunId') or not value.get('sessionId'):
            raise RuntimeError('invalid original admission')
        return value
    result={key:admission(prefix) for key,prefix in [('a','acceptance-a'),('b','acceptance-b'),('history','acceptance-use-0')]}
    result['stateA']=json.loads((evidence/'observed-a.json').read_text())
    result['stateB']=json.loads((evidence/'observed-b.json').read_text())
    a=result['stateA'];b=result['stateB']
    if not expected_container or any(s.get('containerId')!=expected_container or s.get('runStatus')!='completed' for s in (a,b)):
        raise RuntimeError('resume physical container or original Run status mismatch')
    for key in ('sessionId','environmentId','instanceId','containerId'):
        if not a.get(key) or a[key]!=b.get(key):raise RuntimeError('resume continuity mismatch: '+key)
    if (any(v['sessionId']!=a['sessionId'] for v in (result['a'],result['b'],result['history']))
            or result['a']['agentRunId']!=a['agentRunId'] or result['b']['agentRunId']!=b['agentRunId']
            or len({result[k]['agentRunId'] for k in ('a','b','history')})!=3):
        raise RuntimeError('resume admission binding mismatch')
    return result

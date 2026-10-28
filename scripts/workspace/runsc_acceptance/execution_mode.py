"""Verify one explicit lifecycle selection across the owned fixture services."""
from pathlib import Path


def fixture_execution_mode(manifest,compose):
    mode=manifest.get('executionMode')
    if mode not in ('perRun','retained'):
        raise ValueError('explicit supported fixture execution mode required')
    for name in ('api','worker','runtime'):
        service=compose['services'][name]
        if not isinstance(service.get('env_file'),list) or not service['env_file']:
            raise ValueError('explicit effective fixture service environment required')
        effective={}
        for path in service['env_file']:
            effective.update(line.split('=',1) for line in Path(path).read_text().splitlines() if line and not line.startswith('#'))
        effective.update(service.get('environment',{}))
        if effective.get('AGENT_RUN_EXECUTION_MODE')!=mode:
            raise ValueError('fixture service execution mode mismatch')
    return mode

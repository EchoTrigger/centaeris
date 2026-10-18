"""Immutable historical input construction, exclusively for forward migration tests."""
import hashlib
import hmac
import json
from pathlib import Path


HISTORY = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/historical_agent_run_authorization'


def historical_fixture():
    return json.loads((HISTORY / 'v1/valid.json').read_text(encoding='utf-8'))


def historical_profile():
    return json.loads((HISTORY / 'v2/valid.json').read_text(encoding='utf-8'))['benchmarkProfile']


def historical_digest(value):
    return 'sha256:' + hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
        separators=(',', ':')).encode()).hexdigest()


historical_profile_digest = historical_digest


def historical_signature(value, key):
    version = value['schema'].rsplit('.', 1)[1]
    domain = f'workspace:agent-run-authorization:{version}\0'
    return 'hmac-sha256:' + hmac.new(key.encode(), (domain + historical_digest(value)).encode(), hashlib.sha256).hexdigest()

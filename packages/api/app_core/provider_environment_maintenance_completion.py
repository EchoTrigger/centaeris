"""Exact direct SDK completion; never a resident task cleanup receipt."""
import base64
import binascii
import hashlib
import json
from pathlib import PurePosixPath
import re
import unicodedata
from .provider_environment_maintenance_authorization import validate_provider_target

SCHEMA = 'runtime.provider_environment.maintenance.completed.v1'
DISPATCH_SCHEMA = 'runtime.provider_environment.maintenance.dispatched.v1'
# Runtime MAX_HTTP_BODY_BYTES; this is deliberately below Harbor's JSON limit.
MAX_COMPLETION_BYTES = 1024 * 1024
BINDING_FIELDS = {'operationId','environmentId','providerTarget','profileDigest','requestDigest',
    'authorizationDigest','controllerEpoch','controllerLeaseDigest'}


def _invalid():
    raise ValueError('provider_direct_completion_invalid')


def _digest(value):
    return isinstance(value,str) and re.fullmatch(r'sha256:[0-9a-f]{64}',value) is not None


def _bytes(value, count):
    if not isinstance(value,str) or type(count) is not int or not 0 <= count <= MAX_COMPLETION_BYTES:
        _invalid()
    try:
        decoded=base64.b64decode(value,validate=True)
    except (ValueError,binascii.Error):
        _invalid()
    if len(decoded)!=count or base64.b64encode(decoded).decode()!=value:
        _invalid()
    return decoded


def validate_direct_completion(value, expected):
    if (not isinstance(value,dict) or set(value)!={'schema','output'}|BINDING_FIELDS
            or value['schema']!=SCHEMA or not isinstance(expected,dict) or set(expected)!=BINDING_FIELDS
            or any(value[key]!=expected[key] for key in BINDING_FIELDS)
            or type(value['controllerEpoch']) is not int or value['controllerEpoch']<=0):
        _invalid()
    validate_provider_target(value['providerTarget'])
    if any(not _digest(value[key]) for key in ('profileDigest','requestDigest','authorizationDigest','controllerLeaseDigest')):
        _invalid()
    if any(not isinstance(value[key],str) or re.fullmatch(r'[A-Za-z0-9_.:-]{1,160}',value[key]) is None
            for key in ('operationId','environmentId')):
        _invalid()
    try:
        encoded=json.dumps(value,ensure_ascii=False,sort_keys=True,separators=(',',':'),allow_nan=False).encode()
    except (TypeError,ValueError,RecursionError):
        _invalid()
    if len(encoded)>MAX_COMPLETION_BYTES:
        _invalid()
    output=value['output']
    if not isinstance(output,dict):
        _invalid()
    kind=output.get('kind')
    if kind=='command':
        if (set(output)!={'kind','exitCode','stdoutBytesBase64','stdoutByteLength','stderrBytesBase64','stderrByteLength'}
                or type(output['exitCode']) is not int or not -(2**31)<=output['exitCode']<2**31):
            _invalid()
        _bytes(output['stdoutBytesBase64'],output['stdoutByteLength'])
        _bytes(output['stderrBytesBase64'],output['stderrByteLength'])
    elif kind=='filesystemFailed':
        if set(output)!={'kind','code'} or output['code']!='fileNotFound':
            _invalid()
    elif kind in ('fileRead','fileWritten'):
        if (set(output)!=({'kind','byteLength','sha256','dataBase64'} if kind=='fileRead' else {'kind','byteLength','sha256'})
                or type(output['byteLength']) is not int or not 0<=output['byteLength']<=32*1024*1024
                or not _digest(output['sha256'])):
            _invalid()
        if kind=='fileRead':
            data=_bytes(output['dataBase64'],output['byteLength'])
            if output['sha256']!='sha256:'+hashlib.sha256(data).hexdigest():
                _invalid()
    elif kind=='directoryListed':
        if set(output)!={'kind','entries'} or not isinstance(output['entries'],list) or len(output['entries'])>10000:
            _invalid()
        seen=set()
        for entry in output['entries']:
            if (not isinstance(entry,dict) or set(entry)!={'path','kind','sizeBytes'}
                    or entry['kind'] not in ('file','directory') or type(entry['sizeBytes']) is not int or entry['sizeBytes']<0
                    or not isinstance(entry['path'],str)):
                _invalid()
            name=entry['path']
            path=PurePosixPath(name)
            if (not path.is_absolute() or name.startswith('//') or '\\' in name or str(path)!=name or '..' in path.parts
                    or unicodedata.normalize('NFC',name)!=name or any(unicodedata.category(c)=='Cc' for c in name)
                    or name in seen):
                _invalid()
            seen.add(name)
    else:
        _invalid()


def dispatch_record(completion):
    return {**{key: value for key, value in completion.items() if key not in {'schema', 'output'}},
        'schema': DISPATCH_SCHEMA}

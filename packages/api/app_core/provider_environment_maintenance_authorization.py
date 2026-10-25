"""Provider maintenance authority pins a verified target in a distinct HMAC domain."""
import hashlib
import hmac
import json

from .environment_maintenance_authorization import _matches

SCHEMA = 'workspace.provider_environment_maintenance_authorization.v1'
DOMAIN = b'workspace:provider-environment-maintenance-authorization:v1\0'
FIELDS = {'schema', 'operationKind', 'workspaceId', 'sessionId', 'environmentId', 'providerTarget',
    'profileDigest', 'controllerOwner', 'controllerEpoch', 'controllerLeaseDigest',
    'controllerPurpose', 'requestDigest', 'expiresAtMs'}
TARGET_FIELDS = {'provider', 'creationAttemptId', 'instanceId', 'sandboxId',
    'verificationDigest', 'providerConfigDigest'}


def validate_provider_target(target):
    if not isinstance(target, dict) or set(target) != TARGET_FIELDS or target['provider'] != 'e2b':
        raise ValueError('provider_maintenance_target_invalid')
    for field in ('creationAttemptId', 'sandboxId'):
        if not _matches(target[field], r'[A-Za-z0-9_.:-]{1,256}'):
            raise ValueError('provider_maintenance_target_invalid')
    if not _matches(target['instanceId'], r'instance_[0-9a-f]{64}'):
        raise ValueError('provider_maintenance_target_invalid')
    for field in ('verificationDigest', 'providerConfigDigest'):
        if not _matches(target[field], r'sha256:[0-9a-f]{64}'):
            raise ValueError('provider_maintenance_target_invalid')


def sign_provider_maintenance_authorization(payload, signing_key):
    if not isinstance(signing_key, str) or not signing_key:
        raise ValueError('provider_maintenance_authorization_key_invalid')
    if not isinstance(payload, dict) or set(payload) != FIELDS or payload['schema'] != SCHEMA:
        raise ValueError('provider_maintenance_authorization_fields_invalid')
    if payload['operationKind'] != 'task':
        raise ValueError('provider_maintenance_authorization_kind_invalid')
    for field in ('workspaceId', 'sessionId', 'environmentId'):
        if not _matches(payload[field], r'[A-Za-z0-9_.:-]{1,160}'):
            raise ValueError('provider_maintenance_authorization_identity_invalid')
    owner = payload['controllerOwner']
    if (not isinstance(owner, dict) or set(owner) != {'kind', 'operationId'}
            or owner['kind'] != 'maintenanceOperation'
            or not _matches(owner['operationId'], r'[A-Za-z0-9_.:-]{1,160}')):
        raise ValueError('provider_maintenance_authorization_owner_invalid')
    validate_provider_target(payload['providerTarget'])
    for field in ('profileDigest', 'controllerLeaseDigest', 'requestDigest'):
        if not _matches(payload[field], r'sha256:[0-9a-f]{64}'):
            raise ValueError('provider_maintenance_authorization_digest_invalid')
    for field in ('controllerEpoch', 'expiresAtMs'):
        if type(payload[field]) is not int or not 1 <= payload[field] <= 2**63 - 1:
            raise ValueError('provider_maintenance_authorization_fence_invalid')
    if payload['controllerPurpose'] not in ('execute', 'cleanup'):
        raise ValueError('provider_maintenance_authorization_purpose_invalid')
    canonical = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode('utf-8')
    digest = 'sha256:' + hashlib.sha256(canonical).hexdigest()
    return 'hmac-sha256:' + hmac.new(signing_key.encode('utf-8'),
        DOMAIN + digest.encode('ascii'), hashlib.sha256).hexdigest()

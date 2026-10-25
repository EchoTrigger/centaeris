"""Typed provider ownership; the node retains the original task filesystem."""
from dataclasses import dataclass
import hashlib
import json
import re
from uuid import UUID

_TARGET_FIELDS={'provider':'provider','creationAttemptId':'creation_attempt_id','instanceId':'instance_id',
    'sandboxId':'sandbox_id','verificationDigest':'verification_digest','providerConfigDigest':'provider_config_digest'}

def _opaque(value,limit=160):
    return isinstance(value,str) and re.fullmatch(r'[A-Za-z0-9_.:-]{1,'+str(limit)+'}',value) is not None

def _digest(value):
    return isinstance(value,str) and re.fullmatch(r'sha256:[0-9a-f]{64}',value) is not None

@dataclass(frozen=True)
class ProviderTarget:
    provider: str
    creation_attempt_id: str
    instance_id: str
    sandbox_id: str
    verification_digest: str
    provider_config_digest: str

    def __post_init__(self):
        if (self.provider!='e2b' or not _opaque(self.creation_attempt_id,256) or not _opaque(self.sandbox_id,256)
                or not isinstance(self.instance_id,str) or re.fullmatch(r'instance_[0-9a-f]{64}',self.instance_id) is None
                or not _digest(self.verification_digest) or not _digest(self.provider_config_digest)):
            raise ValueError('invalid provider target')

    @classmethod
    def from_wire(cls,value):
        if not isinstance(value,dict) or set(value)!=set(_TARGET_FIELDS):
            raise ValueError('invalid provider target fields')
        return cls(**{attribute:value[field] for field,attribute in _TARGET_FIELDS.items()})

    def to_wire(self):
        return {field:getattr(self,attribute) for field,attribute in _TARGET_FIELDS.items()}

@dataclass(frozen=True)
class OwnedProviderEnvironment:
    session_id: str
    environment_id: str
    agent_run_id: str | None
    provider_target: ProviderTarget
    controller_epoch: int
    template_id: str
    build_id: str
    profile_digest: str
    state: str
    resource_holding: int
    cpu_count: int
    memory_mib: int

    def __post_init__(self):
        if (not all(_opaque(value) for value in (self.session_id,self.environment_id))
                or self.agent_run_id is not None and not _opaque(self.agent_run_id)
                or not isinstance(self.provider_target,ProviderTarget)
                or type(self.controller_epoch) is not int or self.controller_epoch<0
                or not _digest(self.profile_digest) or self.state!='resident'
                or type(self.resource_holding) is not int or self.resource_holding!=1
                or type(self.cpu_count) is not int or self.cpu_count<=0
                or type(self.memory_mib) is not int or self.memory_mib<=0
                or not isinstance(self.template_id,str) or re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,127}',self.template_id) is None):
            raise ValueError('invalid owned provider environment')
        try:
            if not isinstance(self.build_id,str) or str(UUID(self.build_id))!=self.build_id:
                raise ValueError('invalid template build')
        except (ValueError,AttributeError):
            raise ValueError('invalid template build') from None

    @property
    def instance_id(self):
        return self.provider_target.instance_id

    @property
    def sandbox_id(self):
        return self.provider_target.sandbox_id

    def require_profile(self,profile,cpus,memory_mib):
        if not isinstance(profile,dict):
            raise ValueError('invalid provider profile')
        digest='sha256:'+hashlib.sha256(json.dumps(profile,sort_keys=True,ensure_ascii=False,separators=(',',':'),allow_nan=False).encode()).hexdigest()
        if (profile.get('schema')!='runtime.e2b_environment_profile.v1' or profile.get('provider')!='e2b'
                or profile.get('lifetime')!='retained' or profile.get('workspaceInitialization')!='taskImage'
                or profile.get('providerConfigDigest')!=self.provider_target.provider_config_digest
                or profile.get('templateRef')!={'templateId':self.template_id,'buildId':self.build_id}
                or digest!=self.profile_digest or type(cpus) is not int or type(memory_mib) is not int
                or self.cpu_count!=cpus or self.memory_mib!=memory_mib):
            raise ValueError('provider task image or actual resources mismatch')


def require_storage_probe(stdout,minimum_mib):
    """Return observed available bytes. No upper disk quota is asserted."""
    if not isinstance(stdout,str) or type(minimum_mib) is not int or minimum_mib<=0:
        raise ValueError('provider storage facts invalid')
    lines=[line.split() for line in stdout.splitlines() if line.strip()]
    if len(lines)!=2 or lines[0][:4]!=['Filesystem','1024-blocks','Used','Available'] or len(lines[1])!=6:
        raise ValueError('provider storage facts invalid')
    data=lines[1]
    if any(re.fullmatch(r'[0-9]+',value) is None for value in data[1:4]) or re.fullmatch(r'[0-9]+%',data[4]) is None or not data[5].startswith('/'):
        raise ValueError('provider storage facts invalid')
    total,used,available=map(int,data[1:4])
    if used>total or available>total or available<minimum_mib*1024:
        raise ValueError('provider minimum available storage mismatch')
    return available*1024

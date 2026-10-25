"""Attest explicit owned containerd and Docker endpoints before fixture mutation."""
import json,os,re
from pathlib import Path,PurePosixPath

def option(argv,name):
    indices=[i for i,v in enumerate(argv) if v==name]
    if len(indices)!=1 or indices[0]+1>=len(argv):raise RuntimeError('explicit unique private option required: '+name)
    return argv[indices[0]+1]

def validate_vectors(root,namespace,plugins,daemon,containerd):
    root=str(PurePosixPath(root))
    if not root.startswith('/') or root=='/' or '..' in root.split('/'):raise RuntimeError('private absolute root required')
    if not re.fullmatch('runsc-[a-f0-9]{12}',namespace) or plugins!=namespace+'-plugins':raise RuntimeError('unique containerd namespaces required')
    expected={'--host':'unix://'+root+'/docker.sock','--data-root':root+'/data','--exec-root':root+'/exec','--pidfile':root+'/daemon.pid','--containerd':root+'/containerd.sock','--containerd-namespace':namespace,'--containerd-plugins-namespace':plugins}
    if any(option(daemon,k)!=v for k,v in expected.items()):raise RuntimeError('private Docker transport/namespace mismatch')
    if any(option(containerd,k)!=v for k,v in {'--address':root+'/containerd.sock','--root':root+'/containerd-data','--state':root+'/containerd-state'}.items()):raise RuntimeError('private containerd endpoint/root/state mismatch')
    return {'dockerSocket':root+'/docker.sock','containerdSocket':root+'/containerd.sock','runtimeRoot':root+'/exec/runtime-runc/'+namespace}

def process_arguments(pid):
    return [v.decode() for v in Path('/proc/'+str(pid)+'/cmdline').read_bytes().split(b'\0') if v]

def verify_owner(path,read_process=process_arguments):
    path=Path(path)
    if path.is_symlink():raise RuntimeError('isolation owner symlink rejected')
    owner=json.loads(path.read_text())
    if not owner.get('privateContainerdVerified') or not owner.get('rawProbeRetired'):raise RuntimeError('private transport and raw OCI root must be observed first')
    if owner['daemonPid']==owner['containerdPid']:raise RuntimeError('independent owned process identities required')
    facts=validate_vectors(owner['root'],owner['containerdNamespace'],owner['containerdPluginsNamespace'],read_process(owner['daemonPid']),read_process(owner['containerdPid']))
    if owner.get('runtimeRoot')!=facts['runtimeRoot']:raise RuntimeError('observed OCI root differs from private namespace')
    return owner

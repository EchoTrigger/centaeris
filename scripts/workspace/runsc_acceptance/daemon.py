"""Owned Docker/containerd lifecycle for native validation only."""
import json,os,re,subprocess,time,uuid
from pathlib import Path,PurePosixPath
from runsc_acceptance.isolation import validate_vectors,process_arguments
SHARED=['docker','--host','unix:///var/run/docker.sock']

def command(argv,timeout=30):
    result=subprocess.run(argv,stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=timeout)
    if result.returncode:raise RuntimeError('validation command failed: '+argv[0]+'; private diagnostics suppressed')
    if len(result.stdout.encode())>2*1024*1024:raise RuntimeError('bounded command output exceeded')
    return result.stdout

def runtime_flags(path):
    result=subprocess.run([path,'flags'],stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=10)
    if result.returncode:raise RuntimeError('installed runtime flags unavailable')
    return result.stdout+result.stderr

def runtime_logging(arguments,root,help_text):
    for flag in ('panic-log','log','log-format'):
        if not re.search(r'^\s*-'+flag+r'(?:\s|$)',help_text,re.M):raise RuntimeError('installed runtime logging capability missing')
    logs=root/'runsc-logs';logs.mkdir(mode=0o700)
    return [*arguments,'--panic-log='+str(logs/'panic.log'),'--log='+str(logs/'runtime.log'),'--log-format=json']

def network_identity(shared_pid=None,read_namespace=None,owned_pids=()):
    if shared_pid is None:shared_pid=int(Path('/run/docker.pid').read_text())
    read_namespace=read_namespace or (lambda pid:Path('/proc/'+str(pid)+'/ns/net').stat().st_ino)
    result={'own':read_namespace('self'),'init':read_namespace(1),'sharedDocker':read_namespace(shared_pid)}
    if result['own'] in (result['init'],result['sharedDocker']):raise RuntimeError('private network namespace required before daemon startup; use unshare --net')
    if owned_pids:
        result['ownedProcesses']=[read_namespace(pid) for pid in owned_pids]
        if any(value!=result['own'] for value in result['ownedProcesses']):raise RuntimeError('private network namespace not inherited by owned process')
    return result


def cgroup_pids(container,pid,limit,proc_root=Path('/proc'),cgroup_root=Path('/sys/fs/cgroup')):
    if not re.fullmatch('[a-f0-9]{64}',container) or type(pid) is not int or pid<=0 or type(limit) is not int or limit<=0:raise RuntimeError('invalid cgroup identity/limit')
    entries=(proc_root/str(pid)/'cgroup').read_text().splitlines()
    paths=[line[3:] for line in entries if line.startswith('0::')]
    if len(paths)!=1:raise RuntimeError('unified cgroup identity unavailable')
    path=PurePosixPath(paths[0])
    if not path.is_absolute() or '..' in path.parts or not any(part in (container,'docker-'+container+'.scope') for part in path.parts):raise RuntimeError('cgroup container identity mismatch')
    group=cgroup_root.joinpath(*path.parts[1:]).resolve(strict=True)
    if not group.is_relative_to(cgroup_root.resolve(strict=True)):raise RuntimeError('cgroup identity escaped controller')
    current=int((group/'pids.current').read_text());maximum=(group/'pids.max').read_text().strip()
    if maximum!=str(limit):raise RuntimeError('physical PID limit differs from charged limit')
    events=dict(line.split() for line in (group/'pids.events').read_text().splitlines())
    return {'current':current,'max':limit,'eventsMax':int(events['max'])}


class PrivateDocker:
    def __init__(self,root,network=False):
        if Path(root).is_symlink():raise RuntimeError('private root symlink rejected')
        self.root=Path(root).resolve(strict=True)
        if self.root.parent!=Path('/var/tmp') or not self.root.name.startswith('centaeris-runsc-') or self.root.is_symlink():raise RuntimeError('fresh owned disk-backed root required')
        if any((self.root/n).exists() for n in ('owner.json','data','containerd-data','exec')):raise RuntimeError('existing validation responsibility must be preserved')
        self.root.chmod(0o700);self.network=network;self.daemon=None;self.containerd=None
        self.namespace='runsc-'+uuid.uuid4().hex[:12]
        self.argv=['docker','--host','unix://'+str(self.root/'docker.sock')]
        self.owner={'root':str(self.root),'fixtureOwner':uuid.uuid4().hex,'containerdNamespace':self.namespace,'containerdPluginsNamespace':self.namespace+'-plugins','daemonPid':None,'containerdPid':None,'status':'preparing'}
    def record(self,**fields):self.owner.update(fields);(self.root/'owner.json').write_text(json.dumps(self.owner,indent=2))
    def run(self,*args,timeout=30):return command([*self.argv,*args],timeout)
    def inspect(self,cid):return json.loads(self.run('inspect',cid))[0]
    def shared_identity(self):
        pid=int(Path('/run/docker.pid').read_text());parts=Path('/proc/'+str(pid)+'/stat').read_text().rsplit(')',1)[1].split()
        return {'id':command([*SHARED,'info','--format','{{.ID}}']).strip(),'pid':pid,'startTicks':parts[19]}
    def require_shared(self):
        if self.shared_identity()!=self.shared:raise RuntimeError('shared daemon changed during read-only cache transfer')
    def start(self):
        network=network_identity()
        if os.name!='posix' or os.geteuid()!=0:raise RuntimeError('explicit Linux root required')
        self.record(networkNamespaces=network)
        self.shared=self.shared_identity()
        runtimes=json.loads(command([*SHARED,'info','--format','{{json .Runtimes}}']))
        runtime=runtimes['runsc'];path=runtime.get('path',runtime.get('Path'));arguments=runtime.get('runtimeArgs',runtime.get('Args',[]))
        if path!='/usr/bin/runsc' or not isinstance(arguments,list):raise RuntimeError('installed explicit runsc runtime required')
        help_text=runtime_flags(path);version=command([path,'--version']).splitlines()
        arguments=runtime_logging(arguments,self.root,help_text)
        config=self.root/'daemon.json';config.write_text(json.dumps({'runtimes':{'runsc':{'path':path,'runtimeArgs':arguments}}}));config.chmod(0o600)
        ctr=['/usr/bin/containerd','--config','/dev/null','--address',str(self.root/'containerd.sock'),'--root',str(self.root/'containerd-data'),'--state',str(self.root/'containerd-state')]
        dock=['dockerd','--config-file',str(config),'--host','unix://'+str(self.root/'docker.sock'),'--data-root',str(self.root/'data'),'--exec-root',str(self.root/'exec'),'--pidfile',str(self.root/'daemon.pid'),'--containerd',str(self.root/'containerd.sock'),'--containerd-namespace',self.namespace,'--containerd-plugins-namespace',self.namespace+'-plugins','--bridge=none','--iptables='+str(self.network).lower(),'--ip6tables=false','--ip-forward='+str(self.network).lower(),'--ip-masq='+str(self.network).lower(),'--storage-driver=vfs']
        validate_vectors(str(self.root),self.namespace,self.namespace+'-plugins',dock,ctr)
        self.record(status='starting',sharedDaemonIdentity=self.shared,runscVersion=version,runscArguments=arguments)
        self.ctr_log=(self.root/'containerd.log').open('wb');self.containerd=subprocess.Popen(ctr,stdin=subprocess.DEVNULL,stdout=self.ctr_log,stderr=self.ctr_log,start_new_session=True)
        self.record(containerdPid=self.containerd.pid)
        end=time.monotonic()+20
        while not (self.root/'containerd.sock').exists():
            if self.containerd.poll() is not None or time.monotonic()>end:raise RuntimeError('owned containerd startup failed')
            time.sleep(.1)
        self.docker_log=(self.root/'daemon.log').open('wb');self.daemon=subprocess.Popen(dock,stdin=subprocess.DEVNULL,stdout=self.docker_log,stderr=self.docker_log,start_new_session=True)
        self.record(daemonPid=self.daemon.pid)
        end=time.monotonic()+45
        while True:
            if self.daemon.poll() is not None:raise RuntimeError('owned Docker startup failed')
            try:self.run('info','--format','{{.ID}}',timeout=3);break
            except (RuntimeError,subprocess.TimeoutExpired):
                if time.monotonic()>end:raise RuntimeError('owned Docker readiness timeout')
                time.sleep(.2)
        validate_vectors(str(self.root),self.namespace,self.namespace+'-plugins',process_arguments(self.daemon.pid),process_arguments(self.containerd.pid))
        log=(self.root/'daemon.log').read_text(errors='replace')
        if 'address='+str(self.root/'containerd.sock') not in log or 'address=/run/containerd/containerd.sock' in log:raise RuntimeError('owned containerd endpoint not observed')
        observed_network=network_identity(self.shared['pid'],owned_pids=[self.daemon.pid,self.containerd.pid])
        self.record(privateContainerdVerified=True,status='private-ready',networkNamespaces=observed_network)
    def load_image(self,image):
        self.require_shared();source=json.loads(command([*SHARED,'image','inspect',image]))[0]
        saved=subprocess.Popen([*SHARED,'image','save',image],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        try:
            loaded=subprocess.run([*self.argv,'image','load'],stdin=saved.stdout,capture_output=True,timeout=900);saved.stdout.close();saved.communicate(timeout=30)
            if saved.returncode or loaded.returncode:raise RuntimeError('owned cached image transfer failed')
        finally:
            if saved.poll() is None:saved.terminate();saved.wait(timeout=10)
        copied=json.loads(self.run('image','inspect',image))[0]
        if any(source.get(k)!=copied.get(k) for k in ('Architecture','Os','RootFS','Config')):raise RuntimeError('imported image content differs')
        self.require_shared();return copied['Id']
    def capture_runtime_errors(self):
        rows=[]
        for file in sorted((self.root/'runsc-logs').glob('*.log')):
            if file.is_symlink() or not file.is_file():raise RuntimeError('runtime log path changed')
            with file.open('rb') as stream:
                head=stream.read(32768).decode(errors='replace');stream.seek(max(0,file.stat().st_size-32768));tail=stream.read(32768).decode(errors='replace')
            lines=[]
            for line in head.splitlines()[:40]+['[bounded diagnostic middle omitted]']+tail.splitlines()[-60:]:
                if re.search('password|authorization|api.?key|token',line,re.I):line='[credential-related diagnostic suppressed]'
                lines.append(line[:1000])
            rows.append({'name':file.name,'bytes':file.stat().st_size,'tail':lines})
        return rows
    def metrics(self,cid,pid,limit):return cgroup_pids(cid,pid,limit)
    def probe(self,image_id,cycles=12,pids_limit=512):
        if type(pids_limit) is not int or pids_limit<=0:raise RuntimeError('positive bounded PID limit required')
        from runsc_rotation_acceptance import memory_oracle_server,memory_oracle_client,extract_oracle,challenge_proof
        owner=self.owner['fixtureOwner'];server=memory_oracle_server(owner,'A');server[3]="import os;os.makedirs('/mnt/data',exist_ok=True);"+server[3]
        name='runsc-raw-'+owner[:12];self.record(rawProbeName=name,status='raw-probe')
        cid=self.run('run','-d','--pull=never','--name',name,'--runtime=runsc','--network=none','--cap-drop=ALL','--security-opt=no-new-privileges','--memory=268435456','--pids-limit='+str(pids_limit),'--label','centaeris.acceptance.owner='+owner,'--entrypoint',server[0],image_id,*server[1:]).strip();self.record(rawProbeContainerId=cid)
        before=extract_oracle(self.run('exec',cid,'sh','-c',memory_oracle_client(owner,'A',0)));physical=self.inspect(cid);pid=physical['State']['Pid']
        if physical['Id']!=cid or physical['HostConfig']['Runtime']!='runsc' or not physical['State']['Running'] or pid<=0:raise RuntimeError('raw physical identity invalid')
        expected=self.root/'exec/runtime-runc'/self.namespace
        state=json.loads(command(['/usr/bin/runsc','--root='+str(expected),'state',cid]))
        if state.get('id')!=cid or state.get('status')!='running' or not expected.is_dir():raise RuntimeError('owned OCI root not physically verified')
        self.record(runtimeRoot=str(expected))
        baseline_pids=self.metrics(cid,pid,pids_limit)
        observations=[]
        for index in range(1,cycles+1):
            self.run('pause',cid);paused=self.inspect(cid)
            if paused['Id']!=cid or paused['State']['Paused'] is not True or paused['State']['Pid']!=pid:raise RuntimeError('raw pause lost original process')
            paused_pids=self.metrics(cid,pid,pids_limit)
            time.sleep(.05);self.run('unpause',cid);resumed=self.inspect(cid)
            if resumed['Id']!=cid or resumed['State']['Running'] is not True or resumed['State']['Paused'] or resumed['State']['Pid']!=pid:raise RuntimeError('raw resume stopped or replaced container')
            after=extract_oracle(self.run('exec',cid,'sh','-c',memory_oracle_client(owner,'A',index)))
            if any(before[k]!=after[k] for k in ('nonce','pid','startTicks','fileSha256')) or after['counter']<=before['counter'] or after['proof']!=challenge_proof(before['nonce'],after['challenge']):raise RuntimeError('raw resume lost memory/file identity')
            resumed_pids=self.metrics(cid,pid,pids_limit)
            if paused_pids['eventsMax'] or resumed_pids['eventsMax']:raise RuntimeError('owned PID controller exhausted')
            observations.append({'cycle':index,'hostPid':pid,'oracle':after,'pausedPids':paused_pids,'resumedPids':resumed_pids});before=after
        facts=self.inspect(cid)
        if facts['Config']['Labels'].get('centaeris.acceptance.owner')!=owner:raise RuntimeError('owned raw probe identity changed')
        self.run('rm','-f',cid)
        if self.run('ps','-aq','--filter','id='+cid).strip():raise RuntimeError('raw probe absence not confirmed')
        self.record(rawProbeRetired=True,status='raw-probe-passed')
        result={'schema':'runsc.raw.pause.v1','cycles':cycles,'pidsLimit':pids_limit,'baselinePids':baseline_pids,'runtimeRoot':str(expected),'namespace':self.namespace,'runscVersion':self.owner['runscVersion'],'runtimeArguments':self.owner['runscArguments'],'observations':observations}
        (self.root/'raw-probe.json').write_text(json.dumps(result,indent=2));return result
    def close(self):
        if self.run('ps','-aq','--filter','label=centaeris.managed=true').strip():raise RuntimeError('physical obligations remain; private services retained')
        ids=self.run('ps','-aq').split()
        for cid in ids:
            labels=self.inspect(cid)['Config'].get('Labels') or {}
            project=self.owner.get('project')
            if not (project is not None and labels.get('centaeris.acceptance.project')==project) and labels.get('centaeris.acceptance.owner')!=self.owner['fixtureOwner']:raise RuntimeError('unknown owned-daemon resource retained')
        if ids:self.run('rm','-f',*ids,timeout=90)
        self.daemon.terminate();self.daemon.wait(timeout=30);self.containerd.terminate();self.containerd.wait(timeout=30)
        self.record(status='private-processes-stopped',dataRetained=True)

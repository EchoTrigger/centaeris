"""Launch one owned native runsc product gate inside a private network namespace."""
import argparse,hashlib,json,os,struct,subprocess,sys,time,urllib.request
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from runsc_acceptance.daemon import PrivateDocker,command
from runsc_acceptance.isolation import verify_owner

def static_helper(path):
    data=path.read_bytes()
    if len(data)<64 or data[:6]!=b'\x7fELF\x02\x01':raise RuntimeError('static ELF64 helper required')
    header=struct.unpack_from('<16sHHIQQQIHHHHHH',data);offset,size,count=header[5],header[9],header[10]
    if header[2]!=62 or size!=56 or not 0<count<=128 or offset+size*count>len(data):raise RuntimeError('invalid helper ELF program headers')
    if any(struct.unpack_from('<I',data,offset+i*size)[0]==3 for i in range(count)):raise RuntimeError('dynamic helper interpreter rejected')


def shared_arguments(environments, model_wait_ms=0, initial_stagger_ms=0, expected_working_environments=None):
    expected=environments if expected_working_environments is None else expected_working_environments
    if (type(environments) is not int or not 2<=environments<=64
            or type(model_wait_ms) is not int or not 0<=model_wait_ms<=120000
            or type(initial_stagger_ms) is not int or not 0<=initial_stagger_ms<=120000
            or initial_stagger_ms*(environments-1)>120000
            or type(expected) is not int or not 1<=expected<=environments):
        raise ValueError('bounded shared model wait, initial stagger and working threshold required')
    return ['--model-wait-ms',str(model_wait_ms),'--initial-stagger-ms',str(initial_stagger_ms),'--expected-working-environments',str(expected)]

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',required=True,type=Path);parser.add_argument('--source',required=True,type=Path);parser.add_argument('--git-dir',type=Path)
    parser.add_argument('--api-port',type=int,default=18083);parser.add_argument('--runtime-port',type=int,default=19003);parser.add_argument('--execute',action='store_true')
    parser.add_argument('--shared-workload',action='store_true')
    parser.add_argument('--environment-count',type=int,default=4)
    parser.add_argument('--iterations',type=int,default=3)
    parser.add_argument('--work-units',type=int,default=2000)
    parser.add_argument('--model-wait-ms',type=int,default=0)
    parser.add_argument('--initial-stagger-ms',type=int,default=0)
    parser.add_argument('--expected-working-environments',type=int)
    parser.add_argument('--k6-binary',type=Path)
    for name in ('api','worker','runtime','general','postgres','redis'):parser.add_argument('--'+name+'-image',required=True)
    args=parser.parse_args()
    if not args.shared_workload and (args.model_wait_ms or args.initial_stagger_ms or args.expected_working_environments is not None):parser.error('workload timing and overlap parameters require shared mode')
    shared=[]
    if args.shared_workload:
        try:shared=shared_arguments(args.environment_count,args.model_wait_ms,args.initial_stagger_ms,args.expected_working_environments)
        except ValueError as error:parser.error(str(error))
    if args.shared_workload and (args.k6_binary is None or not 2 <= args.environment_count <= 64
            or not 1 <= args.iterations <= 20 or not 1 <= args.work_units <= 1000000):
        parser.error('shared workload requires Linux k6 and bounded environment/iteration/work counts')
    if args.shared_workload:
        args.k6_binary=args.k6_binary.resolve(strict=True)
        with args.k6_binary.open('rb') as stream:
            if stream.read(6)!=b'\x7fELF\x02\x01':parser.error('shared workload requires Linux ELF64 k6')
    if not args.execute:parser.error('explicit --execute required for an owned disposable fixture')
    if os.name!='posix' or os.geteuid()!=0 or Path('/proc/self/ns/net').stat().st_ino==Path('/proc/1/ns/net').stat().st_ino:raise RuntimeError('run under Linux root unshare --net')
    if args.root.is_symlink():raise RuntimeError('owned root symlink rejected')
    root=args.root.resolve(strict=True);source=args.source.resolve(strict=True);bins=root/'bins';manifest=root/'build-final.json';build=json.loads(manifest.read_text())
    for name in ('runtime_server','execution_agent','environment_host_supervisor'):
        file=bins/name;expected=build['hostSupervisorSha256'] if name=='environment_host_supervisor' else build['binarySha256'][name]
        if file.is_symlink() or hashlib.sha256(file.read_bytes()).hexdigest()!=expected:raise RuntimeError('matching candidate binary missing')
        file.chmod(0o755)
    static_helper(bins/'execution_agent');command(['ip','link','set','lo','up'])
    if {row['ifname'] for row in json.loads(command(['ip','-j','link']))}!={'lo'}:raise RuntimeError('fresh uplink-free network namespace required')
    docker=PrivateDocker(root,network=True);supervisor=None
    try:
        docker.start();print(json.dumps({'phase':'private-runtime-prerequisites'}),flush=True)
        images={name:getattr(args,name+'_image') for name in ('api','worker','runtime','general','postgres','redis')}
        private_images={name:docker.load_image(image) for name,image in images.items()}
        probe=docker.probe(private_images['general'],12)
        verify_owner(root/'owner.json')
        print(json.dumps({'phase':'raw-twelve-pause-resumes-passed','version':probe['runscVersion'],'namespace':probe['namespace']}),flush=True)
        for directory in ('host','host-control','host-output','tls'):(root/directory).mkdir(mode=0o700)
        tls=root/'tls'
        command(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','2','-keyout',str(tls/'model.key'),'-out',str(tls/'model.crt'),'-subj','/CN=model','-addext','subjectAltName=DNS:model'])
        (tls/'model.key').chmod(0o600);(tls/'combined-ca.pem').write_bytes(Path('/etc/ssl/certs/ca-certificates.crt').read_bytes()+(tls/'model.crt').read_bytes())
        env={**os.environ,'DOCKER_HOST':'unix://'+str(root/'docker.sock'),'DOCKER_BUILDKIT':'0','NO_PROXY':'127.0.0.1,localhost,::1','no_proxy':'127.0.0.1,localhost,::1'}
        if args.git_dir:env.update(GIT_DIR=str(args.git_dir.resolve()),GIT_WORK_TREE=str(source))
        def call(argv,timeout=120,input=None):
            p=subprocess.run(argv,env=env,input=input,text=True,capture_output=True,timeout=timeout)
            if p.returncode:raise RuntimeError('owned fixture step failed: '+argv[0]+'; private diagnostics suppressed')
            return p.stdout
        tools=source/'scripts/workspace/runsc_acceptance';fixture=root/'fixture'
        argv=['python3','-B',str(tools/'prepare.py'),'--source',str(source),'--bins',str(bins),'--revision',build['sourceRevision'],'--build-manifest',str(manifest),'--tls-dir',str(tls),'--output',str(fixture),'--runsc-rotation','--isolation-owner',str(root/'owner.json'),'--docker-socket',str(root/'docker.sock'),'--host-socket-directory',str(root/'host'),'--api-port',str(args.api_port),'--runtime-port',str(args.runtime_port)]
        if args.shared_workload:argv+=['--shared-workload','--environment-count',str(args.environment_count)]+shared
        for name,image in images.items():argv+=['--'+name+'-image',image]
        call(argv);config=json.loads((fixture/'compose.json').read_text());docker.record(project=config['name'],sourceRevision=build['sourceRevision'])
        # The containing network namespace has no uplink. A non-internal bridge
        # permits the two intentionally published loopback product endpoints.
        config['networks']={'default':{'internal':False,'ipam':{'config':[{'subnet':'10.253.0.0/24'}]}}};(fixture/'compose.json').write_text(json.dumps(config,indent=2))
        call([*docker.argv,'build','--network','none','-t',config['name']+'-general:fixed',str(fixture/'general-image')],900)
        dc=[*docker.argv,'compose','-f',str(fixture/'compose.json')]
        call([*dc,'up','-d','--wait','postgres','redis'])
        host={'schema':'environment.host.v1','socketPath':str(root/'host/control.sock'),'controlRoot':str(root/'host-control'),'outputRoot':str(root/'host-output'),'ociRuntime':'runsc','runtimePath':'/usr/bin/runsc','runtimeRoot':docker.owner['runtimeRoot'],'dockerPath':'/usr/bin/docker','dockerSocket':str(root/'docker.sock'),'controllerUid':0,'helperPath':str(bins/'execution_agent')}
        host_path=root/'host-supervisor.json';host_path.write_text(json.dumps(host));host_path.chmod(0o600);log=(root/'supervisor.log').open('wb')
        supervisor=subprocess.Popen([str(bins/'environment_host_supervisor'),str(host_path)],stdin=subprocess.DEVNULL,stdout=log,stderr=log,start_new_session=True);docker.record(supervisorPid=supervisor.pid)
        end=time.monotonic()+10
        while not (root/'host/control.sock').exists():
            if supervisor.poll() is not None or time.monotonic()>end:raise RuntimeError('owned supervisor readiness failed')
            time.sleep(.1)
        call([*dc,'run','--rm','-T','api','/app/.venv/bin/python','manage.py','migrate','--noinput']);call([*dc,'run','--rm','-T','catalog-init']);call([*dc,'run','--rm','-T','api','/app/.venv/bin/python','manage.py','shell'],input=(tools/'seed.py').read_text());call([*dc,'up','-d','model','api','runtime','worker'])
        api=call([*dc,'ps','-q','api']).strip();call([*docker.argv,'cp',api+':/var/lib/centaeris-storage/acceptance-seed.json',str(fixture/'seed.json')]);(fixture/'seed.json').chmod(0o600)
        opener=urllib.request.build_opener(urllib.request.ProxyHandler({}));end=time.monotonic()+90
        while True:
            try:
                with opener.open('http://127.0.0.1:'+str(args.api_port)+'/api/csrf',timeout=3) as response:
                    if response.status==200 and json.loads(response.read()).get('csrfToken'):break
            except Exception:
                if time.monotonic()>end:raise RuntimeError('owned API readiness failed')
                time.sleep(.3)
        docker.record(status='product-twelve-runs');print(json.dumps({'phase':'product-twelve-runs'}),flush=True)
        if args.shared_workload:
            output=call(['python3','-B',str(tools/'shared_workload.py'),'--fixture',str(fixture),'--result',str(root/'results'),
                '--k6-binary',str(args.k6_binary),'--environment-count',str(args.environment_count),'--iterations',str(args.iterations),'--work-units',str(args.work_units)]+shared,1800)
        else:
            output=call(['python3','-B',str(source/'scripts/workspace/runsc_rotation_acceptance.py'),'--fixture',str(fixture),'--result',str(root/'results'),'--acknowledge-fixture-retirement'],1800)
        # Successful product retirement has removed both managed workloads.
        # Stop only this recorded supervisor and isolated process pair.
        if docker.run('ps','-aq','--filter','label=centaeris.managed=true').strip():raise RuntimeError('completed gate still has physical responsibility')
        supervisor.terminate();supervisor.wait(timeout=20);docker.close();print(output.strip(),flush=True)
    except BaseException:
        docker.record(status='failed-preserved',runtimeDiagnostics=docker.capture_runtime_errors());raise
if __name__=='__main__':main()

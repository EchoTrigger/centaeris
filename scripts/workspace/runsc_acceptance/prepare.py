"""Prepare an exact native runsc fixture; never start services or reveal credentials."""
import argparse,base64,hashlib,json,os,re,secrets,shutil,subprocess
from pathlib import Path,PurePosixPath
from isolation import verify_owner
from memory_profile import memory_profile,workspace_memory_budget

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('source','bins','output','build-manifest','tls-dir','isolation-owner'):
        parser.add_argument('--'+name,required=True,type=Path)
    parser.add_argument('--shared-workload',action='store_true')
    parser.add_argument('--execution-mode',choices=('perRun','retained'),default='perRun')
    parser.add_argument('--environment-count',type=int,default=4)
    parser.add_argument('--iterations',type=int,default=3)
    parser.add_argument('--memory-mib',type=int,default=0)
    parser.add_argument('--minimum-hold-ms',type=int,default=0)
    parser.add_argument('--workspace-memory-budget-bytes',type=int)
    parser.add_argument('--model-wait-ms',type=int,default=0)
    parser.add_argument('--initial-stagger-ms',type=int,default=0)
    parser.add_argument('--expected-working-environments',type=int)
    parser.add_argument('--revision',required=True)
    parser.add_argument('--docker-socket',required=True)
    parser.add_argument('--host-socket-directory',required=True)
    parser.add_argument('--runsc-rotation',required=True,action='store_true')
    parser.add_argument('--model-script',type=Path,default=Path(__file__).with_name('model.py'))
    parser.add_argument('--api-port',type=int,default=18083)
    parser.add_argument('--runtime-port',type=int,default=19003)
    for name in ('api','worker','runtime','general','postgres','redis'):parser.add_argument('--'+name+'-image',required=True)
    args=parser.parse_args()
    if not 1 <= args.environment_count <= 64:parser.error('environment count must be between 1 and 64')
    if not args.shared_workload and (args.memory_mib or args.minimum_hold_ms or args.workspace_memory_budget_bytes is not None or args.model_wait_ms or args.initial_stagger_ms or args.expected_working_environments is not None):parser.error('workload timing and overlap parameters require shared mode')
    if not args.shared_workload and args.execution_mode!='retained':parser.error('rotation requires retained execution mode')
    try:
        profile=memory_profile(args.environment_count,args.iterations,args.memory_mib,args.minimum_hold_ms)
        tenant_memory=workspace_memory_budget(args.environment_count,args.workspace_memory_budget_bytes)
    except ValueError as error:parser.error(str(error))
    if (not 0<=args.model_wait_ms<=120000 or not 0<=args.initial_stagger_ms<=120000
            or args.initial_stagger_ms*(args.environment_count-1)>120000):parser.error('shared workload wait and total initial stagger must be bounded by 120000ms')
    expected_working=args.environment_count if args.expected_working_environments is None else args.expected_working_environments
    if not 1<=expected_working<=args.environment_count:parser.error('working threshold must be between 1 and the environment count')
    for value in (args.docker_socket,args.host_socket_directory):
        if not value.startswith('/') or '..' in value.split('/') or any(c.isspace() for c in value) or ':' in value or '\\' in value:
            parser.error('dedicated canonical Linux paths required')
    if args.docker_socket=='/var/run/docker.sock' or args.host_socket_directory=='/run/centaeris-host':parser.error('shared service endpoints rejected')
    isolation=verify_owner(args.isolation_owner)
    if args.docker_socket!=str(PurePosixPath(isolation['root'])/'docker.sock') or args.host_socket_directory!=str(PurePosixPath(isolation['root'])/'host'):parser.error('fixture socket directories differ from verified owner')
    source=args.source.resolve(strict=True);bins=args.bins.resolve(strict=True);model=args.model_script.resolve(strict=True)
    if args.output.exists():raise SystemExit('new fixture directory required')
    hashes={name:hashlib.sha256((bins/name).read_bytes()).hexdigest() for name in ('runtime_server','execution_agent')}
    build=json.loads(args.build_manifest.read_text())
    if build.get('sourceRevision')!=args.revision or build.get('binarySha256')!=hashes or build.get('hostSupervisorSha256')!=hashlib.sha256((bins/'environment_host_supervisor').read_bytes()).hexdigest():
        raise SystemExit('source/binary manifest mismatch')
    docker=['docker','--host','unix://'+args.docker_socket]
    def command(argv):return subprocess.run(argv,check=True,capture_output=True,text=True).stdout.strip()
    if command([*docker,'ps','-aq','--filter','label=centaeris.managed=true']):raise SystemExit('managed sandbox responsibility already exists')
    images={}
    for name in ('api','worker','runtime','general','postgres','redis'):
        reference=getattr(args,name+'_image');identity=command([*docker,'image','inspect',reference,'--format','{{.Id}}'])
        if not re.fullmatch('sha256:[a-f0-9]{64}',identity):raise SystemExit('cached image identity unavailable')
        images[name]={'reference':reference,'id':identity}
    sha=command(['git','-C',str(source),'rev-parse','HEAD'])
    if sha!=args.revision or command(['git','-C',str(source),'status','--porcelain']):raise SystemExit('exact clean candidate required')
    if command(['git','-C',str(source),'rev-parse','HEAD^{tree}'])!=build.get('sourceTree'):raise SystemExit('source tree differs from build')
    tls=args.tls_dir.resolve(strict=True)
    if any(not (tls/name).is_file() for name in ('model.key','model.crt','combined-ca.pem')):raise SystemExit('private model TLS files required')
    host_memory_headroom=None
    if args.shared_workload:
        host_memory=json.loads(command([*docker,'info','--format','{{json .MemTotal}}']))
        if type(host_memory) is not int or host_memory<=0:raise SystemExit('positive Docker host memory required')
        host_memory_headroom=max(host_memory//5,512*1024**2)
    root=args.output.resolve();root.mkdir(mode=0o700,parents=True);project='retainedaccept'+secrets.token_hex(5)
    env={}
    for line in (source/'.env.example').read_text().splitlines():
        if line and not line.startswith('#') and '=' in line:
            key,value=line.split('=',1)
            if value:env[key]=value
    env.update(DJANGO_SECRET_KEY=secrets.token_urlsafe(48),INTERNAL_API_TOKEN=secrets.token_urlsafe(48),ENVIRONMENT_MAINTENANCE_TOKEN=secrets.token_urlsafe(48),AGENT_RUN_AUTHORIZATION_SIGNING_KEY=secrets.token_urlsafe(48),CREDENTIAL_ENCRYPTION_KEY=base64.urlsafe_b64encode(secrets.token_bytes(32)).decode(),POSTGRES_PASSWORD=secrets.token_urlsafe(24),POSTGRES_DB='retained_acceptance',POSTGRES_USER='acceptance',POSTGRES_HOST='postgres',POSTGRES_PORT='5432',DJANGO_ALLOWED_HOSTS='localhost,127.0.0.1,api',WORK_RETURN_AUDIT_INTERVAL_SECONDS='30',UPLOAD_BODY_MAX_BYTES='1048576',UPLOAD_TEMP_MAX_BYTES='4194304',UPLOAD_MAX_CONCURRENT='1',UPLOAD_TEMP_ROOT='/var/lib/centaeris-upload-temp',STORAGE_ROOT='/var/lib/centaeris-storage',PLUGIN_CATALOG_ROOT='/opt/centaeris/plugins',SANDBOX_MEMORY_BYTES='268435456',SANDBOX_CPU_MILLI='1000',SANDBOX_PIDS_LIMIT='512',SANDBOX_DATA_TMPFS_BYTES='67108864',RUNTIME_URL='http://runtime:9000',RUNTIME_INTERNAL_URL='http://runtime:9000',API_INTERNAL_URL='http://api:8000',REDIS_URL='redis://redis:6379/0',WEB_ORIGIN=f'http://127.0.0.1:{args.api_port}',API_BASE_URL=f'http://127.0.0.1:{args.api_port}',RUNTIME_BIND_ADDRESS='0.0.0.0',RUNTIME_PORT='9000',RUNTIME_STATE_ROOT='/var/lib/centaeris-runtime/state',OCI_RUNTIME='runsc',WORKSPACE_GENERAL_IMAGE=project+'-general:fixed',PLUGIN_VOLUME_NAME=project+'_plugins',AGENT_MEMORY_VOLUME_NAME=project+'_memory',AGENT_RUN_EXECUTION_MODE=args.execution_mode,WORKER_SLOT_COUNT='2',EXECUTION_GLOBAL_LIMIT='2',RESIDENT_GLOBAL_COUNT='2',RESIDENT_TENANT_COUNT='2',API_POSTGRES_POOL_MAX_SIZE='0',CENTAERIS_SOURCE_REVISION=sha,RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS='1',RUNTIME_RUNSC_IDLE_PAUSE_MS='3000',ENVIRONMENT_HOST_SOCKET='/run/centaeris-host/control.sock',ENVIRONMENT_EXECUTION_HELPER=str(bins/'execution_agent'))
    resource_limits=None
    if args.shared_workload:
        count=str(args.environment_count);storage=str(max(32,args.environment_count*4)*1024**3)
        env.update(SANDBOX_MEMORY_BYTES=str(4*1024**3),SANDBOX_CPU_MILLI='2000',SANDBOX_DATA_TMPFS_BYTES=str(4*1024**3),
            WORKER_SLOT_COUNT=count,EXECUTION_GLOBAL_LIMIT=count,RESIDENT_GLOBAL_COUNT=count,RESIDENT_TENANT_COUNT=count,
            RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS=count,RUNTIME_NATIVE_SHARED_ADMISSION='1',
            RESIDENT_HOST_MEMORY_HEADROOM_BYTES=str(host_memory_headroom),RESIDENT_GLOBAL_MEMORY_BYTES=str(12*1024**3),RESIDENT_TENANT_MEMORY_BYTES=str(tenant_memory),
            RESIDENT_GLOBAL_CPU_MILLI='8000',RESIDENT_TENANT_CPU_MILLI='4000',
            RESIDENT_GLOBAL_WORKSPACE_BYTES=storage,RESIDENT_TENANT_WORKSPACE_BYTES=storage,
            CENTAERIS_ACCEPTANCE_SHARED_WORKLOAD='1',WORKSPACE_PERF_OBSERVATIONS='1')
        fields={'count':'COUNT','memoryBytes':'MEMORY_BYTES','pids':'PIDS','workspaceBytes':'WORKSPACE_BYTES'}
        resource_limits={scope:{field:int(env['RESIDENT_'+scope.upper()+'_'+suffix]) for field,suffix in fields.items()} for scope in ('global','tenant')}
        resource_limits['hostHeadroomBytes']=int(env['RESIDENT_HOST_MEMORY_HEADROOM_BYTES'])
        resource_limits['requested']={field:int(env[key]) for field,key in {'memoryBytes':'SANDBOX_MEMORY_BYTES','pids':'SANDBOX_PIDS_LIMIT','workspaceBytes':'SANDBOX_DATA_TMPFS_BYTES'}.items()}
        pem=tls/'model.pem';pem.write_bytes((tls/'model.crt').read_bytes()+(tls/'model.key').read_bytes());pem.chmod(0o600)
    env['DATABASE_URL']=f"postgresql://{env['POSTGRES_USER']}:{env['POSTGRES_PASSWORD']}@postgres:5432/{env['POSTGRES_DB']}"
    def private_file(name,values):
        path=root/name;path.write_text(''.join(f'{k}={v}\n' for k,v in sorted(values.items())));path.chmod(0o600);return str(path)
    full=private_file('synthetic.env',env);limited=private_file('service.env',{k:v for k,v in env.items() if k!='ENVIRONMENT_MAINTENANCE_TOKEN'})
    sources=[f'{source/"packages/api"}:/app/packages/api:ro',f'{source/"packages/worker"}:/app/packages/worker:ro']
    services={
        'postgres':{'image':images['postgres']['id'],'environment':{k:env[k] for k in ('POSTGRES_DB','POSTGRES_USER','POSTGRES_PASSWORD')},'volumes':['postgres:/var/lib/postgresql'],'healthcheck':{'test':['CMD-SHELL','pg_isready -U acceptance -d retained_acceptance'],'interval':'2s','retries':30}},
        'redis':{'image':images['redis']['id'],'command':['redis-server','--save','','--appendonly','no','--maxmemory','32mb'],'healthcheck':{'test':['CMD','redis-cli','ping'],'interval':'2s','retries':30}},
        'model':{'image':images['api']['id'],'entrypoint':[],'command':['/app/.venv/bin/python','/harness/model.py'],'environment':{'MOCK_TLS_CERT':'/harness/model.crt','MOCK_TLS_KEY':'/harness/model.key'},'volumes':[f'{model}:/harness/model.py:ro',f'{source/"scripts/workspace/runsc_rotation_acceptance.py"}:/harness/runsc_rotation_acceptance.py:ro',f'{tls/"model.crt"}:/harness/model.crt:ro',f'{tls/"model.key"}:/harness/model.key:ro']},
        'api':{'image':images['api']['id'],'env_file':[limited],'entrypoint':[],'working_dir':'/app/packages/api','command':['/app/.venv/bin/uvicorn','api.asgi:application','--host','0.0.0.0','--port','8000'],'volumes':sources+['storage:/var/lib/centaeris-storage','uploads:/var/lib/centaeris-upload-temp','plugins:/opt/centaeris/plugins:ro'],'ports':[f'127.0.0.1:{args.api_port}:8000']},
        'runtime':{'image':images['runtime']['id'],'env_file':[full],'entrypoint':['/usr/local/bin/runtime_server'],'user':'0:0','volumes':[f'{bins/"runtime_server"}:/usr/local/bin/runtime_server:ro',args.docker_socket+':/var/run/docker.sock',args.host_socket_directory+':/run/centaeris-host:ro',str(bins/'execution_agent')+':'+str(bins/'execution_agent')+':ro','runtime:/var/lib/centaeris-runtime','memory:/var/lib/centaeris-agent-memory','plugins:/opt/centaeris/plugins:ro'],'ports':[f'127.0.0.1:{args.runtime_port}:9000']},
        'worker':{'image':images['worker']['id'],'env_file':[limited],'working_dir':'/app/packages/worker','command':['/app/.venv/bin/python','worker.py','serve'],'volumes':sources,'init':True}}
    for name in ('api','runtime','worker'):
        services[name].setdefault('environment',{})['SSL_CERT_FILE']='/harness/combined-ca.pem';services[name]['volumes'].append(f'{tls/"combined-ca.pem"}:/harness/combined-ca.pem:ro')
    if args.shared_workload:
        scripts=source/'perf/mock-model'
        for filename in ('server.py','fixed_workload.py'):
            if not (scripts/filename).is_file():raise RuntimeError('fixed workload model source missing')
        services['model']['command']=['/app/.venv/bin/python','/harness/server.py']
        services['model']['environment']['MOCK_FIXED_MODEL_WAIT_MS']=str(args.model_wait_ms)
        services['model']['volumes']=[f'{scripts/"server.py"}:/harness/server.py:ro',f'{scripts/"fixed_workload.py"}:/harness/fixed_workload.py:ro',f'{tls/"model.pem"}:/opt/mock/tls.pem:ro']
    services['catalog-init']={**services['api'],'profiles':['setup'],'command':['/app/.venv/bin/python','manage.py','initialize_plugin_catalog'],'volumes':[v.replace('plugins:/opt/centaeris/plugins:ro','plugins:/opt/centaeris/plugins:rw') for v in services['api']['volumes']]};services['catalog-init'].pop('ports')
    for service in services.values():service.update(restart='no',labels={'centaeris.acceptance.project':project},mem_limit='512m',cpus='1',pids_limit=128)
    volumes={'postgres':{},'storage':{},'uploads':{},'plugins':{'name':project+'_plugins'},'memory':{'name':project+'_memory'},'runtime':{}}
    (root/'compose.json').write_text(json.dumps({'name':project,'services':services,'volumes':volumes},indent=2))
    image=root/'general-image';image.mkdir();shutil.copyfile(bins/'execution_agent',image/'execution_agent');(image/'execution_agent').chmod(0o755)
    (image/'Dockerfile').write_text('FROM '+images['general']['id']+'\nCOPY execution_agent /opt/centaeris/bin/\n')
    (root/'manifest.json').write_text(json.dumps({'project':project,'sourceRevision':sha,'source':str(source),'binaryDirectory':str(bins),'binarySha256':hashes,'buildManifest':build,'images':images,'apiPort':args.api_port,'runtimePort':args.runtime_port,'runscRotation':True,'executionMode':args.execution_mode,'resourceLimits':resource_limits,'sharedWorkload':({'environmentCount':args.environment_count,'idlePauseMs':3000,'capMemoryBytes':4*1024**3,'capCpuMilli':2000,'modelWaitMs':args.model_wait_ms,'initialStaggerMs':args.initial_stagger_ms,'expectedWorkingEnvironments':expected_working,'workspaceMemoryBudgetBytes':int(env['RESIDENT_TENANT_MEMORY_BYTES']),'workProfile':profile} if args.shared_workload else None),'dockerSocket':args.docker_socket,'hostSocketDirectory':args.host_socket_directory,'isolationOwner':str(args.isolation_owner.resolve()),'status':'prepared-not-started'},indent=2))
    print(json.dumps({'project':project,'sourceRevision':sha,'status':'prepared-not-started'}))
if __name__=='__main__':main()

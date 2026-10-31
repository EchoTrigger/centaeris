"""Generate an isolated retained-lifecycle Docker project; never start or stop it.

Generated deployment credentials are synthetic and stay in the private directory.
Benchmark provider credentials are supplied separately to the one-shot seed process.

The caller must build the exact integrated SHA and supply a reviewed model script.

"""

import argparse

import base64

import json

import os

import hashlib

import re

from pathlib import Path, PurePosixPath

import secrets

import subprocess

def main():

    parser = argparse.ArgumentParser()

    parser.add_argument("--source", required=True, type=Path)

    parser.add_argument("--bins", required=True, type=Path)

    parser.add_argument("--model-script", type=Path)
    parser.add_argument('--benchmark-mode', action='store_true', help='Prepare a real-provider Workspace benchmark stack without a model stub')
    parser.add_argument('--docker-socket', help='Absolute Linux Unix socket used for every Docker observation and the Runtime bind')
    parser.add_argument('--benchmark-helper', type=Path)
    parser.add_argument('--benchmark-helper-sha256')
    parser.add_argument('--benchmark-helper-manifest', type=Path)

    parser.add_argument("--output", required=True, type=Path)

    parser.add_argument("--revision", required=True, help="Exact 40-hex revision used for this source archive and build")

    parser.add_argument("--api-port", type=int, default=18080)
    parser.add_argument("--runtime-port", type=int, default=19001)
    parser.add_argument('--build-manifest', required=True, type=Path)
    for name in ('api','worker','runtime','general','postgres','redis'):
        parser.add_argument('--'+name+'-image', required=True)

    parser.add_argument("--python-deps", type=Path, help="Isolated dependency target for cached API image")

    parser.add_argument("--tls-dir", type=Path)
    args = parser.parse_args()

    if args.benchmark_mode:
        if not args.docker_socket or args.model_script is not None or args.tls_dir is not None:
            raise SystemExit('benchmark mode requires an explicit Docker socket and no mock model/TLS configuration')
        if not all((args.benchmark_helper, args.benchmark_helper_sha256, args.benchmark_helper_manifest)):
            raise SystemExit('benchmark mode requires the static helper, digest, and matching build manifest')
        if not (1024 <= args.api_port <= 65535 and 1024 <= args.runtime_port <= 65535
                and args.api_port != args.runtime_port):
            raise SystemExit('benchmark loopback ports must be distinct unprivileged TCP ports')
    elif args.model_script is None:
        raise SystemExit('deterministic acceptance requires --model-script')
    elif any((args.benchmark_helper, args.benchmark_helper_sha256, args.benchmark_helper_manifest)):
        raise SystemExit('benchmark helper arguments require benchmark mode')
    if args.docker_socket is not None and (not args.docker_socket.startswith('/')
            or args.docker_socket.startswith('//') or '..' in args.docker_socket.split('/')
            or any(ord(c) < 33 for c in args.docker_socket) or ':' in args.docker_socket
            or '\\' in args.docker_socket or str(PurePosixPath(args.docker_socket)) != args.docker_socket):
        raise SystemExit('Docker socket must be a canonical absolute Linux Unix socket path')
    docker = ['docker'] + (['--host', 'unix://' + args.docker_socket] if args.docker_socket else [])
    source, bins = (p.resolve(strict=True) for p in (args.source, args.bins))
    model = args.model_script.resolve(strict=True) if args.model_script else None

    for name in ("runtime_server", "execution_agent", "environment_task_service"):

        if not (bins / name).is_file():

            raise SystemExit(f"missing fixed build binary: {name}")

    if args.output.exists():

        raise SystemExit("refusing to replace an existing harness directory")

    # Observe every managed sandbox, including unknown/foreign allocations.

    inventory = subprocess.run(docker + ["ps", "-a", "--filter", "label=centaeris.managed=true", "--format", "{{.ID}}"], check=True, capture_output=True, text=True)

    if inventory.stdout.strip():

        raise SystemExit("managed sandboxes already exist; isolated DB cannot reconcile this host")
    images={}
    for name in ('api','worker','runtime','general','postgres','redis'):
        reference=getattr(args,name+'_image')
        inspected=subprocess.run(docker + ['image','inspect','--format','{{.Id}}',reference],
                                 check=True,capture_output=True,text=True).stdout.strip()
        if not re.fullmatch(r'sha256:[0-9a-f]{64}',inspected):
            raise SystemExit('image identity unavailable: '+name)
        images[name]={'requestedRef':reference,'id':inspected}
    hashes = {name: hashlib.sha256((bins / name).read_bytes()).hexdigest() for name in ("runtime_server","execution_agent","environment_task_service")}
    build=json.loads(args.build_manifest.read_text())
    if build.get('sourceRevision')!=args.revision or build.get('binarySha256')!=hashes:
        raise SystemExit('build manifest must tie this exact source revision to all binary hashes')

    sha = args.revision

    if not re.fullmatch("[0-9a-f]{40}", sha):

        raise SystemExit("revision must be an exact full lowercase SHA")

    if (source / ".git").exists():

        actual = subprocess.run(["git", "-C", str(source), "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()

        dirty = subprocess.run(["git", "-C", str(source), "status", "--porcelain"], check=True, capture_output=True, text=True).stdout

        if actual != sha or dirty.strip():

            raise SystemExit("source checkout must be clean at the declared revision")
        source_tree=subprocess.run(['git','-C',str(source),'rev-parse','HEAD^{tree}'],
            check=True,capture_output=True,text=True).stdout.strip()
        if build.get('sourceTree')!=source_tree:
            raise SystemExit('build manifest source tree does not match mounted checkout')
    else:
        raise SystemExit('mounted source must be a verifiable clean Git checkout; unverified archive rejected')

    benchmark_helper = None
    docker_host_id = None
    if args.benchmark_mode:
        helper = args.benchmark_helper.resolve(strict=True)
        if not helper.is_file() or args.benchmark_helper.is_symlink() or helper.stat().st_size > 128 * 1024 * 1024:
            raise SystemExit('benchmark helper must be a bounded regular file')
        helper_hash = hashlib.sha256(helper.read_bytes()).hexdigest()
        helper_build = json.loads(args.benchmark_helper_manifest.read_text(encoding='utf-8'))
        if (not re.fullmatch('[0-9a-f]{64}', args.benchmark_helper_sha256)
                or helper_hash != args.benchmark_helper_sha256
                or helper_build.get('sourceRevision') != sha or helper_build.get('sourceTree') != source_tree
                or helper_build.get('target') != 'x86_64-unknown-linux-musl'
                or helper_build.get('binarySha256', {}).get('execution_agent') != helper_hash):
            raise SystemExit('benchmark helper must match the same source and static musl build proof')
        benchmark_helper = {'path': str(helper), 'sha256': helper_hash, 'target': helper_build['target'],
            'sourceRevision': sha, 'sourceTree': source_tree, 'buildManifest': helper_build}
        docker_host_id = subprocess.run(docker + ['info', '--format', '{{.ID}}'],
            check=True, capture_output=True, text=True).stdout.strip()
        if not re.fullmatch('[A-Za-z0-9:_-]{1,128}', docker_host_id):
            raise SystemExit('selected Docker daemon identity unavailable')

    root = args.output.resolve()

    root.mkdir(mode=0o700, parents=True)

    project = ("tbworkspace" if args.benchmark_mode else "retainedaccept") + secrets.token_hex(5)

    env = {}

    for line in (source / ".env.example").read_text(encoding="utf-8").splitlines():

        if line and not line.startswith("#") and "=" in line:

            key, value = line.split("=", 1)

            if value:

                env[key] = value

    env.update({

        "DJANGO_SECRET_KEY": secrets.token_urlsafe(48), "INTERNAL_API_TOKEN": secrets.token_urlsafe(48),
        "ENVIRONMENT_MAINTENANCE_TOKEN": secrets.token_urlsafe(48),

        "AGENT_RUN_AUTHORIZATION_SIGNING_KEY": secrets.token_urlsafe(48),

        "CREDENTIAL_ENCRYPTION_KEY": base64.urlsafe_b64encode(secrets.token_bytes(32)).decode(),

        "POSTGRES_PASSWORD": secrets.token_urlsafe(24), "POSTGRES_DB": "retained_acceptance", "POSTGRES_USER": "acceptance",

        "POSTGRES_HOST": "postgres", "POSTGRES_PORT": "5432", "DJANGO_ALLOWED_HOSTS": "localhost,127.0.0.1,api",

        "WORK_RETURN_AUDIT_INTERVAL_SECONDS": "30", "UPLOAD_BODY_MAX_BYTES": "1048576", "UPLOAD_TEMP_MAX_BYTES": "4194304", "UPLOAD_MAX_CONCURRENT": "1",

        "UPLOAD_TEMP_ROOT": "/var/lib/centaeris-upload-temp", "STORAGE_ROOT": "/var/lib/centaeris-storage", "PLUGIN_CATALOG_ROOT": "/opt/centaeris/plugins",

        "SANDBOX_MEMORY_BYTES": "268435456", "SANDBOX_CPU_MILLI": "1000", "SANDBOX_PIDS_LIMIT": "128", "SANDBOX_WORKSPACE_BYTES": "67108864",

        "RUNTIME_URL": "http://runtime:9000", "RUNTIME_INTERNAL_URL": "http://runtime:9000", "API_INTERNAL_URL": "http://api:8000",

        "REDIS_URL": "redis://redis:6379/0", "WEB_ORIGIN": f"http://127.0.0.1:{args.api_port}", "API_BASE_URL": f"http://127.0.0.1:{args.api_port}",

        "RUNTIME_BIND_ADDRESS": "0.0.0.0", "RUNTIME_PORT": "9000", "RUNTIME_STATE_ROOT": "/var/lib/centaeris-runtime/state", "OCI_RUNTIME": "runc", "RUNTIME_NATIVE_SHARED_ADMISSION": "0",
        "RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS": "", "RUNTIME_RUNSC_IDLE_PAUSE_MS": "",

        "WORKSPACE_GENERAL_IMAGE": project + "-general:fixed", "PLUGIN_VOLUME_NAME": project + "_plugins", "AGENT_MEMORY_VOLUME_NAME": project + "_memory",

        "AGENT_RUN_EXECUTION_MODE": "retained", "WORKER_SLOT_COUNT": "1", "EXECUTION_GLOBAL_LIMIT": "1",

        "RESIDENT_GLOBAL_COUNT": "2", "RESIDENT_TENANT_COUNT": "2", "API_POSTGRES_POOL_MAX_SIZE": "0", "CENTAERIS_SOURCE_REVISION": sha,

    })

    if args.benchmark_mode:
        env.update({
            'POSTGRES_DB': project, 'POSTGRES_USER': project,
            'CENTAERIS_BENCHMARK_FIXTURE_ID': project,
            'SANDBOX_PIDS_LIMIT': '4096', 'RESIDENT_GLOBAL_PIDS': '8192', 'RESIDENT_TENANT_PIDS': '8192',
            'WORKER_SLOT_COUNT': '2', 'EXECUTION_GLOBAL_LIMIT': '2',
            'EXECUTION_GLOBAL_QUEUE_LIMIT': '2', 'EXECUTION_TENANT_QUEUE_LIMIT': '2',
            'RESIDENT_GLOBAL_MEMORY_BYTES': str(12 * 1024**3), 'RESIDENT_TENANT_MEMORY_BYTES': str(12 * 1024**3),
            'RESIDENT_GLOBAL_CPU_MILLI': '6000', 'RESIDENT_TENANT_CPU_MILLI': '6000',
            'BENCHMARK_EXECUTION_HELPER': benchmark_helper['path'],
            'BENCHMARK_EXECUTION_HELPER_SHA256': benchmark_helper['sha256'],
        })

    env["DATABASE_URL"] = f"postgresql://{env['POSTGRES_USER']}:{env['POSTGRES_PASSWORD']}@postgres:5432/{env['POSTGRES_DB']}"

    envpath = root / "synthetic.env"

    envpath.write_text("".join(f"{k}={v}\n" for k,v in sorted(env.items())), encoding="utf-8")

    os.chmod(envpath, 0o600)
    # Only Runtime and the private host maintainer receive maintenance authority.
    service_envpath = root / 'service.env'
    service_envpath.write_text(''.join(f'{key}={value}\n' for key,value in sorted(env.items())
        if key != 'ENVIRONMENT_MAINTENANCE_TOKEN'), encoding='utf-8')
    os.chmod(service_envpath, 0o600)

    source_mounts = [f"{source / 'packages/api'}:/app/packages/api:ro", f"{source / 'packages/worker'}:/app/packages/worker:ro"]

    volumes = {"postgres": {}, "storage": {}, "uploads": {}, "plugins": {"name": project + "_plugins"}, "memory": {"name": project + "_memory"}, "runtime": {}}

    services = {

        "postgres": {"image": images['postgres']['id'], "environment": {k:env[k] for k in ("POSTGRES_DB","POSTGRES_USER","POSTGRES_PASSWORD")}, "volumes": ["postgres:/var/lib/postgresql"], "healthcheck": {"test": ["CMD-SHELL", "pg_isready -U acceptance -d retained_acceptance"], "interval": "2s", "retries":30}},

        "redis": {"image":images['redis']['id'], "command":["redis-server","--save","","--appendonly","no","--maxmemory","32mb"], "healthcheck":{"test":["CMD","redis-cli","ping"],"interval":"2s","retries":30}},

        "model": {"image":images['api']['id'], "entrypoint":[], "command":["/app/.venv/bin/python","/harness/model.py"], "volumes":[f"{model}:/harness/model.py:ro",f"{Path(__file__).with_name('boundary.py').resolve(strict=True)}:/harness/boundary.py:ro"]},

        "api": {"image":images['api']['id'], "env_file":[str(service_envpath)], "entrypoint":[], "working_dir":"/app/packages/api", "command":["/app/.venv/bin/uvicorn","api.asgi:application","--host","0.0.0.0","--port","8000"], "volumes":source_mounts+["storage:/var/lib/centaeris-storage","uploads:/var/lib/centaeris-upload-temp","plugins:/opt/centaeris/plugins:ro"], "ports":[f"127.0.0.1:{args.api_port}:8000"]},

        "runtime": {"image":images['runtime']['id'], "env_file":[str(envpath)], "entrypoint":["/usr/local/bin/runtime_server"], "volumes":[f"{bins / 'runtime_server'}:/usr/local/bin/runtime_server:ro",(args.docker_socket or "/var/run/docker.sock") + ":/var/run/docker.sock","runtime:/var/lib/centaeris-runtime","memory:/var/lib/centaeris-agent-memory","plugins:/opt/centaeris/plugins:ro"]},

        "worker": {"image":images['worker']['id'], "env_file":[str(service_envpath)], "working_dir":"/app/packages/worker", "command":["/app/.venv/bin/python","worker.py","serve"], "volumes":source_mounts, "init":True},

    }
    if args.benchmark_mode:
        services.pop('model')
        services['api']['env_file'] = [str(envpath)]
        services['runtime']['volumes'].append(benchmark_helper['path'] + ':' + benchmark_helper['path'] + ':ro')
        services['postgres']['healthcheck']['test'] = ['CMD-SHELL', 'pg_isready -U ' + project + ' -d ' + project]
        identity = root / 'benchmark-fixture.json'
        identity.write_text(json.dumps({'schema': 'centaeris.workspaceBenchmarkFixture.v1',
            'fixtureId': project, 'databaseName': project, 'databaseUser': project,
            'sourceRevision': sha, 'sourceTree': source_tree, 'dockerHostId': docker_host_id,
            'dockerSocket': args.docker_socket}, indent=2), encoding='utf-8')
        os.chmod(identity, 0o600)
        services['api']['volumes'].append(str(identity) + ':/harness/benchmark-fixture.json:ro')
        seed_script = (source / 'scripts/workspace/retained_acceptance/benchmark_seed.py').resolve(strict=True)
        services['api']['volumes'].append(str(seed_script) + ':/harness/benchmark_seed.py:ro')
        model_validator = (source / 'scripts/harbor/workspace_plan.py').resolve(strict=True)
        services['api']['volumes'].append(str(model_validator) + ':/harness/workspace_plan.py:ro')

    services['runtime']['ports'] = [f'127.0.0.1:{args.runtime_port}:9000']

    if args.python_deps is not None:

        deps = args.python_deps.resolve(strict=True)

        services["api"]["environment"] = {"PYTHONPATH":"/harness/python-deps"}

        services["api"]["volumes"].append(f"{deps}:/harness/python-deps:ro")

    if args.tls_dir is not None:
        tls=args.tls_dir.resolve(strict=True)
        for name in ("model.crt","model.key","combined-ca.pem"):
            if not (tls/name).is_file():
                raise SystemExit("missing isolated TLS artifact: "+name)
        services["model"]["environment"]={"MOCK_TLS_CERT":"/harness/tls/model.crt","MOCK_TLS_KEY":"/harness/tls/model.key"}
        services["model"]["volumes"].append(f"{tls}:/harness/tls:ro")
        services["api"].setdefault("environment",{})["SSL_CERT_FILE"]="/harness/combined-ca.pem"
        services["api"]["volumes"].append(f"{tls/'combined-ca.pem'}:/harness/combined-ca.pem:ro")
    services['catalog-init'] = {
        **services['api'],
        'profiles':['setup'],
        'command':['/app/.venv/bin/python','manage.py','initialize_plugin_catalog'],
        'volumes':['plugins:/opt/centaeris/plugins:rw' if mount=='plugins:/opt/centaeris/plugins:ro'
            else mount for mount in services['api']['volumes']],
    }
    services['catalog-init'].pop('ports',None)
    if args.benchmark_mode:
        services['catalog-init']['env_file'] = [str(service_envpath)]
    for service in services.values():

        service.update({"restart":"no", "labels":{"centaeris.acceptance.project":project}, "mem_limit":"512m", "cpus":"1", "pids_limit":128})

    (root / "compose.json").write_text(json.dumps({"name":project,"services":services,"volumes":volumes},indent=2),encoding="utf-8")

    # Build context copies only current binaries, without changing shared images.

    image = root / "general-image"

    image.mkdir()

    import shutil

    for name in ("execution_agent","environment_task_service"):

        shutil.copyfile(bins / name,image / name)

        os.chmod(image / name,0o755)

    (image / "Dockerfile").write_text("FROM "+images['general']['id']+"\nCOPY execution_agent environment_task_service /opt/centaeris/bin/\n",encoding="utf-8")

    hashes = {name: hashlib.sha256((bins / name).read_bytes()).hexdigest() for name in ("runtime_server","execution_agent","environment_task_service")}

    (root / "manifest.json").write_text(json.dumps({"project":project,"sourceRevision":sha,"source":str(source),"binaryDirectory":str(bins),"binarySha256":hashes,'images':images,'buildManifest':build,'mountedPackages':['packages/api','packages/worker'],"apiPort":args.api_port,"runtimePort":args.runtime_port,"status":"prepared-not-started"},indent=2),encoding="utf-8")

    if args.benchmark_mode:
        manifest_path = root / 'manifest.json'
        manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
        manifest.update(benchmarkMode=True, dockerSocket=args.docker_socket, dockerHostId=docker_host_id,
                        benchmarkHelper=benchmark_helper, fixtureIdentity='benchmark-fixture.json')
        manifest_path.write_text(json.dumps(manifest, indent=2), encoding='utf-8')

    print(json.dumps({"project":project,"directory":str(root),"sourceRevision":sha,"status":"prepared-not-started"}))

if __name__ == "__main__":

    main()


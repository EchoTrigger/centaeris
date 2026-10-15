"""Generate an isolated retained-lifecycle Docker project; never start or stop it.

All credentials are synthetic and stay in the generated private directory.

The caller must build the exact integrated SHA and supply a reviewed model script.

"""

import argparse

import base64

import json

import os

import hashlib

import re

from pathlib import Path

import secrets

import subprocess

def main():

    parser = argparse.ArgumentParser()

    parser.add_argument("--source", required=True, type=Path)

    parser.add_argument("--bins", required=True, type=Path)

    parser.add_argument("--model-script", required=True, type=Path)

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

    source, bins, model = (p.resolve(strict=True) for p in (args.source, args.bins, args.model_script))

    for name in ("runtime_server", "execution_agent", "environment_task_service"):

        if not (bins / name).is_file():

            raise SystemExit(f"missing fixed build binary: {name}")

    if args.output.exists():

        raise SystemExit("refusing to replace an existing harness directory")

    # Observe every managed sandbox, including unknown/foreign allocations.

    inventory = subprocess.run(["docker", "ps", "-a", "--filter", "label=centaeris.managed=true", "--format", "{{.ID}}"], check=True, capture_output=True, text=True)

    if inventory.stdout.strip():

        raise SystemExit("managed sandboxes already exist; isolated DB cannot reconcile this host")
    images={}
    for name in ('api','worker','runtime','general','postgres','redis'):
        reference=getattr(args,name+'_image')
        inspected=subprocess.run(['docker','image','inspect','--format','{{.Id}}',reference],
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

    root = args.output.resolve()

    root.mkdir(mode=0o700, parents=True)

    project = "retainedaccept" + secrets.token_hex(5)

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

        "SANDBOX_MEMORY_BYTES": "268435456", "SANDBOX_CPU_MILLI": "1000", "SANDBOX_PIDS_LIMIT": "128", "SANDBOX_DATA_TMPFS_BYTES": "67108864",

        "RUNTIME_URL": "http://runtime:9000", "RUNTIME_INTERNAL_URL": "http://runtime:9000", "API_INTERNAL_URL": "http://api:8000",

        "REDIS_URL": "redis://redis:6379/0", "WEB_ORIGIN": f"http://127.0.0.1:{args.api_port}", "API_BASE_URL": f"http://127.0.0.1:{args.api_port}",

        "RUNTIME_BIND_ADDRESS": "0.0.0.0", "RUNTIME_PORT": "9000", "RUNTIME_STATE_ROOT": "/var/lib/centaeris-runtime/state", "OCI_RUNTIME": "runc",

        "WORKSPACE_GENERAL_IMAGE": project + "-general:fixed", "PLUGIN_VOLUME_NAME": project + "_plugins", "AGENT_MEMORY_VOLUME_NAME": project + "_memory",

        "AGENT_RUN_EXECUTION_MODE": "retained", "WORKER_SLOT_COUNT": "1", "EXECUTION_GLOBAL_LIMIT": "1", "EXECUTION_TENANT_LIMIT": "1",

        "RESIDENT_GLOBAL_COUNT": "2", "RESIDENT_TENANT_COUNT": "2", "API_POSTGRES_POOL_MAX_SIZE": "0", "CENTAERIS_SOURCE_REVISION": sha,

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

        "model": {"image":images['api']['id'], "entrypoint":[], "command":["/app/.venv/bin/python","/harness/model.py"], "volumes":[f"{model}:/harness/model.py:ro"]},

        "api": {"image":images['api']['id'], "env_file":[str(service_envpath)], "entrypoint":[], "working_dir":"/app/packages/api", "command":["/app/.venv/bin/uvicorn","api.asgi:application","--host","0.0.0.0","--port","8000"], "volumes":source_mounts+["storage:/var/lib/centaeris-storage","uploads:/var/lib/centaeris-upload-temp","plugins:/opt/centaeris/plugins:ro"], "ports":[f"127.0.0.1:{args.api_port}:8000"]},

        "runtime": {"image":images['runtime']['id'], "env_file":[str(envpath)], "entrypoint":["/usr/local/bin/runtime_server"], "volumes":[f"{bins / 'runtime_server'}:/usr/local/bin/runtime_server:ro","/var/run/docker.sock:/var/run/docker.sock","runtime:/var/lib/centaeris-runtime","memory:/var/lib/centaeris-agent-memory","plugins:/opt/centaeris/plugins:ro"]},

        "worker": {"image":images['worker']['id'], "env_file":[str(service_envpath)], "working_dir":"/app/packages/worker", "command":["/app/.venv/bin/python","worker.py","serve"], "volumes":source_mounts, "init":True},

    }
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

    print(json.dumps({"project":project,"directory":str(root),"sourceRevision":sha,"status":"prepared-not-started"}))

if __name__ == "__main__":

    main()


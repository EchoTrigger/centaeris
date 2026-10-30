"""Build exact clean Linux source and record source/compiler/binary identities."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def command(argv, cwd):
    return subprocess.run(argv,cwd=cwd,check=True,capture_output=True,text=True).stdout.strip()


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--source',required=True,type=Path)
    parser.add_argument('--target-dir',required=True,type=Path)
    parser.add_argument('--output',required=True,type=Path)
    parser.add_argument('--runsc-rotation',action='store_true',help='Build and stage the static musl native helper required by runsc')
    parser.add_argument('--artifact-directory',type=Path,help='New artifact directory, required with --runsc-rotation')
    args=parser.parse_args()
    if bool(args.artifact_directory)!=args.runsc_rotation:
        parser.error('--runsc-rotation and --artifact-directory must be supplied together')
    if args.artifact_directory is not None and args.artifact_directory.exists():
        parser.error('artifact directory must be new')
    if sys.platform!='linux':
        raise SystemExit('physical acceptance requires Linux binaries; this builder does not certify other hosts')
    source=args.source.resolve(strict=True)
    revision=command(['git','rev-parse','HEAD'],source)
    tree=command(['git','rev-parse','HEAD^{tree}'],source)
    if command(['git','status','--porcelain'],source):
        raise SystemExit('source must be clean before building')
    target=args.target_dir.resolve()
    build=['cargo','build','--locked','--target-dir',str(target),
           '-p','runtime_server','-p','hosted_execution','--bins']
    subprocess.run(build,cwd=source,check=True)
    helper_build=None
    helper=target/'debug/execution_agent'
    if args.runsc_rotation:
        helper_build=['cargo','rustc','--locked','--release','--target','x86_64-unknown-linux-musl',
                      '--target-dir',str(target),'-p','hosted_execution','--bin','execution_agent',
                      '--','-C','relocation-model=static']
        subprocess.run(helper_build,cwd=source,check=True,
                       env={**os.environ,'RUSTFLAGS':'-Ctarget-feature=+crt-static'})
        helper=target/'x86_64-unknown-linux-musl/release/execution_agent'
        if 'INTERP' in command(['readelf','-l',str(helper)],source):
            raise SystemExit('runsc requires a static helper without an ELF interpreter')
    if (command(['git','rev-parse','HEAD'],source)!=revision
            or command(['git','status','--porcelain'],source)):
        raise SystemExit('source changed during build; no acceptance manifest emitted')
    binaries=target/'debug'
    if args.artifact_directory is not None:
        binaries=args.artifact_directory.resolve()
        binaries.mkdir(mode=0o700)
        for name in ('runtime_server','execution_agent','environment_host_supervisor'):
            original=helper if name=='execution_agent' else target/'debug'/name
            shutil.copyfile(original,binaries/name)
            os.chmod(binaries/name,0o755)
    manifest={'sourceRevision':revision,'sourceTree':tree,
        'buildCommand':build,'compiler':command(['rustc','--version'],source),
        'cargo':command(['cargo','--version'],source),
        'hostSupervisorSha256':hashlib.sha256((binaries/'environment_host_supervisor').read_bytes()).hexdigest(),
        'binarySha256':{name:hashlib.sha256((binaries/name).read_bytes()).hexdigest()
            for name in ('runtime_server','execution_agent')}}
    if helper_build is not None:
        manifest.update(executionHelperTarget='x86_64-unknown-linux-musl',
                        executionHelperBuildCommand=helper_build,
                        executionHelperRustflags='-Ctarget-feature=+crt-static',
                        binaryDirectory=str(binaries))
    args.output.write_text(json.dumps(manifest,indent=2))
    print(json.dumps({'sourceRevision':revision,'manifest':str(args.output)}))


if __name__=='__main__':
    main()

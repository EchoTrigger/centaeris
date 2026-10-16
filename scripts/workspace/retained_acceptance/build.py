"""Build exact clean Linux source and record source/compiler/binary identities."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def command(argv, cwd):
    return subprocess.run(argv,cwd=cwd,check=True,capture_output=True,text=True).stdout.strip()


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--source',required=True,type=Path)
    parser.add_argument('--target-dir',required=True,type=Path)
    parser.add_argument('--output',required=True,type=Path)
    args=parser.parse_args()
    if sys.platform!='linux':
        raise SystemExit('physical acceptance requires Linux binaries; this builder does not certify other hosts')
    source=args.source.resolve(strict=True)
    revision=command(['git','rev-parse','HEAD'],source)
    tree=command(['git','rev-parse','HEAD^{tree}'],source)
    if command(['git','status','--porcelain'],source):
        raise SystemExit('source must be clean before building')
    build=['cargo','build','--locked','--target-dir',str(args.target_dir.resolve()),
           '-p','runtime_server','-p','hosted_execution','--bins']
    subprocess.run(build,cwd=source,check=True)
    if (command(['git','rev-parse','HEAD'],source)!=revision
            or command(['git','status','--porcelain'],source)):
        raise SystemExit('source changed during build; no acceptance manifest emitted')
    binaries=args.target_dir / 'debug'
    manifest={'sourceRevision':revision,'sourceTree':tree,
        'buildCommand':build,'compiler':command(['rustc','--version'],source),
        'cargo':command(['cargo','--version'],source),
        'binarySha256':{name:hashlib.sha256((binaries / name).read_bytes()).hexdigest()
            for name in ('runtime_server','execution_agent','environment_task_service')}}
    args.output.write_text(json.dumps(manifest,indent=2))
    print(json.dumps({'sourceRevision':revision,'manifest':str(args.output)}))


if __name__=='__main__':
    main()

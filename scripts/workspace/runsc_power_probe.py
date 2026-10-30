"""Twelve-cycle raw runsc retained-memory check on owned containerd/Docker."""
import argparse,json,sys,uuid
from pathlib import Path
from runsc_acceptance.daemon import PrivateDocker

def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--image',required=True);parser.add_argument('--root',type=Path);parser.add_argument('--pids-limit',type=int,default=512);args=parser.parse_args()
    root=args.root or Path('/var/tmp')/('centaeris-runsc-power-'+uuid.uuid4().hex);root.mkdir(mode=0o700,exist_ok=False)
    docker=PrivateDocker(root)
    try:
        docker.start();image=docker.load_image(args.image);result=docker.probe(image,12,pids_limit=args.pids_limit);docker.close()
        print(json.dumps({'schema':'runsc.power.probe.v1','outcome':'passed','cycles':result['cycles'],'runtimeRoot':result['runtimeRoot'],'namespace':result['namespace'],'runscVersion':result['runscVersion'],'runtimeArguments':result['runtimeArguments'],'originalMemoryAndFiles':True,'privateProcessesStopped':True,'productRotationAcceptance':'not-run'}))
    except BaseException:
        docker.record(status='failed-preserved',runtimeDiagnostics=docker.capture_runtime_errors());raise
if __name__=='__main__':main()

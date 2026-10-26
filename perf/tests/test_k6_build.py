"""Execute the real build script with isolated process stubs; no Docker/build workload."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[1]
class PinnedBuildScriptTests(unittest.TestCase):
    def test_xk6_gets_one_pinned_version_source_and_the_pinned_extension(self):
        bash=Path(r'C:/Program Files/Git/bin/bash.exe') if os.name=='nt' else Path('/bin/bash')
        if not bash.is_file():self.skipTest('Bash unavailable for build-script contract')
        with tempfile.TemporaryDirectory() as directory:
            folder=Path(directory);shutil.copyfile(ROOT/'k6/build.sh',folder/'build.sh');shutil.copyfile(ROOT/'k6/versions.env',folder/'versions.env')
            (folder/'stubs').mkdir();(folder/'toolchain/bin').mkdir(parents=True)
            scripts={
                'stubs/docker':r'''#!/usr/bin/env bash
set -eu
while (($#)); do
 case "$1" in
  -e) export "$2"; shift 2;;
  --mount) shift 2;;
  golang@sha256:*) shift; break;;
  *) shift;;
 esac
done
exec "$@"
''',
                'stubs/go':r'''#!/usr/bin/env bash
set -eu
if [[ "$1" == install ]]; then exit 0; fi
if [[ "$1" == env && "$2" == GOPATH ]]; then printf '%s/toolchain\n' "$CENTAERIS_TEST_TOOLS_ROOT"; exit 0; fi
exit 42
''',
                'toolchain/bin/xk6':r'''#!/usr/bin/env bash
set -eu
if [[ -v K6_VERSION ]]; then printf '%s\n' 'version specified both flag and argument' >&2; exit 9; fi
printf '%s\n' "$@" > "$CENTAERIS_TEST_TOOLS_ROOT/observed-args"
'''}
            for name,source in scripts.items():
                target=folder/name;target.write_text(source,encoding='utf-8',newline='\n');target.chmod(0o700)
            posix=subprocess.check_output([str(bash),'-c','pwd'],cwd=folder,text=True).strip()
            environment={**os.environ,'CENTAERIS_TEST_TOOLS_ROOT':posix}
            result=subprocess.run([str(bash),'-c','export PATH="$CENTAERIS_TEST_TOOLS_ROOT/stubs:$PATH"; bash build.sh'],cwd=folder,env=environment,capture_output=True,text=True,timeout=15)
            self.assertEqual(result.returncode,0,result.stderr)
            arguments=(folder/'observed-args').read_text().splitlines()
            pins=dict(line.split('=',1) for line in (ROOT/'k6/versions.env').read_text().splitlines() if '=' in line and not line.startswith('#'))
            self.assertEqual(arguments[:2],['build',pins['K6_VERSION']])
            self.assertEqual(arguments[arguments.index('--with')+1],pins['XK6_SSE_MODULE']+'@'+pins['XK6_SSE_VERSION'])

if __name__=='__main__':unittest.main()

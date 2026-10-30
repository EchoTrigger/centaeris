import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('retained_fixture_build',Path(__file__).with_name('build.py'))
build=importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)

class BuildManifestTests(unittest.TestCase):
    def test_manifest_records_current_runtime_helper_and_matching_host_supervisor(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            source=root/'source';source.mkdir()
            target=root/'target';binaries=target/'debug';binaries.mkdir(parents=True)
            for name in ('runtime_server','execution_agent','environment_host_supervisor'):
                (binaries/name).write_bytes(name.encode())
            output=root/'manifest.json'
            argv=['build','--source',str(source),'--target-dir',str(target),'--output',str(output)]
            with patch.object(sys,'argv',argv),patch.object(sys,'platform','linux'),patch.object(build,'command',side_effect=['a'*40,'b'*40,'','a'*40,'','rustc test','cargo test']),patch.object(build.subprocess,'run') as run,patch('builtins.print'):
                build.main()
            manifest=json.loads(output.read_text())
            self.assertEqual(manifest['binarySha256'],{name:hashlib.sha256(name.encode()).hexdigest() for name in ('runtime_server','execution_agent')})
            self.assertEqual(manifest['hostSupervisorSha256'],hashlib.sha256(b'environment_host_supervisor').hexdigest())
            self.assertEqual(run.call_count,1)
            self.assertIn('--bins',run.call_args.args[0])

    def test_rotation_build_stages_static_musl_helper_without_overwriting_gnu_artifact(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/'source';source.mkdir()
            target=root/'target';gnu=target/'debug';gnu.mkdir(parents=True)
            for name in ('runtime_server','execution_agent','environment_host_supervisor'):
                (gnu/name).write_bytes(('gnu-'+name).encode())
            static=target/'x86_64-unknown-linux-musl/release';static.mkdir(parents=True)
            (static/'execution_agent').write_bytes(b'static-musl-execution-agent')
            artifacts=root/'bins';output=root/'manifest.json'
            argv=['build','--source',str(source),'--target-dir',str(target),'--output',str(output),
                  '--runsc-rotation','--artifact-directory',str(artifacts)]
            with patch.object(sys,'argv',argv),patch.object(sys,'platform','linux'),patch.object(build,'command',side_effect=['a'*40,'b'*40,'','ELF executable without interpreter','a'*40,'','rustc test','cargo test']) as commands,patch.object(build.subprocess,'run') as run,patch('builtins.print'):
                build.main()
            self.assertEqual(run.call_count,2)
            helper=run.call_args_list[1]
            self.assertEqual(helper.args[0],['cargo','rustc','--locked','--release','--target','x86_64-unknown-linux-musl','--target-dir',str(target.resolve()),'-p','hosted_execution','--bin','execution_agent','--','-C','relocation-model=static'])
            self.assertEqual(helper.kwargs['env']['RUSTFLAGS'],'-Ctarget-feature=+crt-static')
            self.assertTrue(any(call.args[0]==['readelf','-l',str((static/'execution_agent').resolve())] for call in commands.call_args_list))
            self.assertEqual((artifacts/'execution_agent').read_bytes(),b'static-musl-execution-agent')
            self.assertEqual((gnu/'execution_agent').read_bytes(),b'gnu-execution_agent')
            self.assertEqual((artifacts/'runtime_server').read_bytes(),b'gnu-runtime_server')
            manifest=json.loads(output.read_text())
            self.assertEqual(manifest['binarySha256']['execution_agent'],hashlib.sha256(b'static-musl-execution-agent').hexdigest())
            self.assertEqual(manifest['executionHelperTarget'],'x86_64-unknown-linux-musl')

    def test_rotation_rejects_dynamic_helper_before_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/'source';source.mkdir()
            artifacts=root/'bins'
            argv=['build','--source',str(source),'--target-dir',str(root/'target'),'--output',str(root/'manifest.json'),
                  '--runsc-rotation','--artifact-directory',str(artifacts)]
            with patch.object(sys,'argv',argv),patch.object(sys,'platform','linux'),patch.object(build,'command',side_effect=['a'*40,'b'*40,'','  INTERP /lib/ld-musl-x86_64.so.1']),patch.object(build.subprocess,'run'),patch('builtins.print'):
                with self.assertRaisesRegex(SystemExit,'static helper'):
                    build.main()
            self.assertFalse(artifacts.exists())
            self.assertFalse((root/'manifest.json').exists())

if __name__=='__main__':unittest.main()

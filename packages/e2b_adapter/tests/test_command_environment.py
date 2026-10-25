"""Execute the actual adapter shell selection against a real local Bash child."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from centaeris_e2b_adapter.bridge import _start_command_stream


@unittest.skipUnless(os.name == "posix" and Path("/bin/bash").exists(), "requires native Bash")
class CommandEnvironmentTests(unittest.TestCase):
    def test_actual_start_preserves_template_and_request_environment_for_plugin_cli(self):
        template_path = "/opt/centaeris/venv/bin:/usr/local/bin:/usr/bin:/bin"
        plugin_path = "/opt/centaeris/plugins/demo/bin"
        request_path = "/custom/request/bin:/usr/local/bin:/usr/bin:/bin"
        template_env = {**os.environ, "PATH": template_path,
                        "CENTAERIS_TEMPLATE_VALUE": "template literal $HOME ; value"}
        template_env.pop("BASH_ENV", None)
        with tempfile.TemporaryDirectory() as workspace:
            for request_env in ({}, {"PATH": request_path, "CENTAERIS_REQUEST_VALUE": "request literal ' ; value"}):
                with self.subTest(request_env=request_env):
                    payload = {"command": "export PATH=" + shlex.quote(plugin_path) + ':"$PATH"; exec '
                        + shlex.quote(sys.executable) + " -c " + shlex.quote(
                            "import json,os;print(json.dumps({'path':os.environ['PATH'],'template':os.environ.get('CENTAERIS_TEMPLATE_VALUE'),'request':os.environ.get('CENTAERIS_REQUEST_VALUE'),'cwd':os.getcwd()}))"),
                        "cwd": workspace, "envs": request_env, "user": "agent", "timeoutSeconds": 5}
                    def execute_actual_process(url, connection, version, user, process, timeout):
                        self.assertEqual((url, connection, version, user), ("loopback-unused", "connection-unused", "0.3.0", "agent"))
                        completed = subprocess.run([process.cmd, *process.args], cwd=process.cwd,
                            env={**template_env, **(process.envs or {})}, timeout=timeout,
                            capture_output=True, check=True)
                        return json.loads(completed.stdout)
                    sandbox = SimpleNamespace(envd_api_url="loopback-unused", connection_config="connection-unused")
                    with patch("centaeris_e2b_adapter.bridge._open_process_stream", side_effect=execute_actual_process):
                        result = _start_command_stream(sandbox, "0.3.0", payload)
                    self.assertEqual(result["path"], plugin_path + ":" + request_env.get("PATH", template_path))
                    self.assertEqual(result["template"], template_env["CENTAERIS_TEMPLATE_VALUE"])
                    self.assertEqual(result["request"], request_env.get("CENTAERIS_REQUEST_VALUE"))
                    self.assertEqual(result["cwd"], workspace)

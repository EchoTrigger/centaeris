"""Explicit Linux acceptance against an operator-created disposable Docker daemon."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import shutil
import struct
import subprocess
import tarfile
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--docker-socket", required=True)
    parser.add_argument("--daemon-id", required=True)
    parser.add_argument("--runc-root", required=True)
    parser.add_argument("--supervisor", required=True)
    parser.add_argument("--helper")
    parser.add_argument("--workload-user", default="0:0")
    parser.add_argument("--workspace-root", default="/workspace")
    parser.add_argument("--evidence", required=True)
    args = parser.parse_args()
    workspace = args.workspace_root
    if os.geteuid() != 0 or args.docker_socket == "/var/run/docker.sock":
        raise RuntimeError("explicit disposable daemon and Linux root required")
    docker_prefix = ["/usr/bin/docker", "--host", "unix://" + args.docker_socket]

    def docker(*command, check=True, data=None):
        return subprocess.run(docker_prefix + list(command), input=data, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, check=check)

    observed_id = docker("info", "--format", "{{.ID}}").stdout.decode().strip()
    if observed_id != args.daemon_id:
        raise RuntimeError("disposable daemon identity mismatch")
    suffix = uuid.uuid4().hex
    instance = "instance_" + hashlib.sha256(suffix.encode()).hexdigest()
    profile = "sha256:" + hashlib.sha256(b"native-host-acceptance-profile").hexdigest()
    image = container = service = None
    checks = []
    with tempfile.TemporaryDirectory(prefix="centaeris-native-", dir="/run") as directory:
        root = Path(directory)
        os.chmod(root, 0o700)
        config_path = root / "config.json"
        control_socket = root / "socket" / "control.sock"
        config = dict(schema="environment.host.v1", socketPath=str(control_socket),
                      controlRoot=str(root / "control"), outputRoot=str(root / "output"),
                      runcPath="/usr/bin/runc", runcRoot=args.runc_root,
                      dockerPath="/usr/bin/docker", dockerSocket=args.docker_socket,
                      controllerUid=0, helperPath=None)
        if args.helper:
            helper_path = root / "execution_agent"
            shutil.copyfile(args.helper, helper_path)
            subprocess.run(["/usr/bin/strip", "--strip-debug", str(helper_path)], check=True)
            os.chmod(helper_path, 0o500)
            config.update(helperPath=str(helper_path))
        config_path.write_text(json.dumps(config))
        os.chmod(config_path, 0o600)
        service_log = open(root / "service.log", "wb")

        def start_service():
            nonlocal service
            service = subprocess.Popen([args.supervisor, str(config_path)], stdout=service_log,
                                       stderr=subprocess.STDOUT)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if service.poll() is not None:
                    raise RuntimeError((root / "service.log").read_text())
                try:
                    with socket.socket(socket.AF_UNIX) as probe:
                        probe.connect(str(control_socket))
                    return
                except (FileNotFoundError, ConnectionRefusedError):
                    time.sleep(.02)
            raise RuntimeError("supervisor did not listen")

        def rpc(request, allow_error=False):
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(30)
                stream.connect(str(control_socket))
                stream.sendall(json.dumps(dict(schema="environment.host.v1", request=request),
                                          separators=(",", ":")).encode() + b"\n")
                response = json.loads(stream.makefile("rb").readline(1024 * 1024 + 1))
            if response["status"] != "success":
                if allow_error:
                    return response
                raise RuntimeError(response["error"])
            return response["result"]

        owner = dict(kind="agentRun", runId="shared")
        fence = dict(environmentInstanceId=instance, controllerOwner=owner,
                     controllerEpoch=1, controllerPurpose="execute")

        def task(operation, **fields):
            request = dict(operation=operation, **fields)
            if operation != "takeover":
                request["fence"] = fence.copy()
            result = rpc(dict(operation="task", envelope=dict(schema="environment.task.v1",
                                                              request=request)))["response"]
            if result["status"] != "success":
                raise RuntimeError(result["error"])
            return result["result"]

        def query(task_id):
            return task("query", taskId=task_id, cursor=0, maxBytes=65536)

        def helper(kind, operation, body=b"", expected_digest=None, allow_error=False):
            header = dict(fence=fence.copy(), operationId=f"{fence['controllerEpoch']}:{operation}",
                          scope=dict(kind="environment"), kind=kind, inputLength=len(body),
                          inputDigest=expected_digest or "sha256:" + hashlib.sha256(body).hexdigest())
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(80)
                stream.connect(str(control_socket))
                stream.sendall(json.dumps(dict(schema="environment.host.v1", request=dict(operation="helper", helper=header)), separators=(",", ":")).encode() + b"\n" + body)
                reader = stream.makefile("rb")
                response = json.loads(reader.readline(1024 * 1024 + 1))
                if response["status"] != "success":
                    if allow_error:
                        return response
                    raise RuntimeError(response["error"])
                result = response["result"]
                assert result["kind"] == "task", result
                receipt = result["response"]
                assert receipt["status"] == "success", receipt
                task_id = receipt["result"]["task"]["taskId"]
            wait_for(task_id, lambda item: item["state"] == "exited" and item["outputComplete"] and item["cleanupComplete"])
            if operation == "write-large":
                altered_header = dict(header, inputDigest="sha256:" + "0" * 64)
                rejected = rpc(dict(operation="helperOutput", helper=altered_header, taskId=task_id), allow_error=True)
                assert rejected["error"] == "host_helper_output_identity_mismatch"
                if "helper_output_is_bound_to_the_original_typed_input" not in checks:
                    checks.append("helper_output_is_bound_to_the_original_typed_input")
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(80)
                stream.connect(str(control_socket))
                stream.sendall(json.dumps(dict(schema="environment.host.v1", request=dict(operation="helperOutput", helper=header, taskId=task_id)), separators=(",", ":")).encode() + b"\n")
                reader = stream.makefile("rb")
                response = json.loads(reader.readline(1024 * 1024 + 1))
                assert response["status"] == "success", response
                result = response["result"]
                assert result["kind"] == "helperFinished", result
                stdout = reader.read(result["stdoutLength"])
                stderr = reader.read(result["stderrLength"])
                assert len(stdout) == result["stdoutLength"] and len(stderr) == result["stderrLength"]
                assert result["task"]["exitCode"] == 0, stderr.decode(errors="replace")
                return result, stdout

        def filesystem_frame(operation, content=b""):
            policy = dict(filesystem=dict(workspaceRoot=workspace, readOnlyRoots=[workspace], writableRoots=[workspace],
                                          deniedReadPaths=[], deniedWritePaths=[], tmpRoot=None), network=dict(mode="disabled"))
            header = dict(request=dict(operationId=None, cwd=workspace, policy=policy, modelPath="payload.bin", operation=operation),
                          contentLength=len(content), contentDigest="sha256:" + hashlib.sha256(content).hexdigest())
            encoded = json.dumps(header, separators=(",", ":")).encode()
            return struct.pack(">I", len(encoded)) + encoded + content

        def decode_filesystem(frame):
            length = struct.unpack(">I", frame[:4])[0]
            header = json.loads(frame[4:4 + length])
            content = frame[4 + length:]
            assert header["contentLength"] == len(content)
            assert header["contentDigest"] == "sha256:" + hashlib.sha256(content).hexdigest()
            assert header["result"]["error"] is None, header
            return header, content

        def wait_for(task_id, predicate):
            deadline = time.monotonic() + 8
            last = None
            while time.monotonic() < deadline:
                last = query(task_id)
                if predicate(last["task"]):
                    return last
                time.sleep(.03)
            raise RuntimeError("task observation timed out: " + json.dumps(last))

        try:
            tar_data = io.BytesIO()
            with tarfile.open(fileobj=tar_data, mode="w") as archive:
                for name in ["bin", "workspace"]:
                    item = tarfile.TarInfo(name)
                    item.type = tarfile.DIRTYPE
                    item.mode = 0o755
                    archive.addfile(item)
                archive.add("/usr/bin/busybox", arcname="bin/busybox", recursive=False)
                shell=tarfile.TarInfo("bin/sh");shell.type=tarfile.SYMTYPE;shell.linkname="busybox"
                archive.addfile(shell)
                marker=tarfile.TarInfo("workspace/from-image");marker.size=len(b"preserved image data");marker.mode=0o644
                archive.addfile(marker, io.BytesIO(b"preserved image data"))
            image = docker("import", "-", "centaeris-native-acceptance:" + suffix,
                           data=tar_data.getvalue()).stdout.decode().strip()
            container = docker("create", "--name", "centaeris-native-" + suffix,
                               "--runtime", "runc", "--network", "none", "--user", args.workload_user,
                               "--workdir", "/", "--security-opt", "no-new-privileges",
                               "--env", "CENTAERIS_WORKSPACE_ROOT="+workspace, "--env", "CENTAERIS_WORKSPACE_USER="+args.workload_user,
                               "--memory", "128m", "--pids-limit", "128", "--cpus", "1",
                               "--label", "centaeris.managed=true",
                               "--label", "centaeris.instance_id=" + instance,
                               "--label", "centaeris.environment_id=acceptance",
                               "--label", "centaeris.environment_profile_digest=" + profile,
                               image, "/bin/busybox", "sleep", "3600").stdout.decode().strip()
            docker("start", container)
            start_service()
            target = dict(environmentInstanceId=instance, containerId=container,
                          environmentId="acceptance", profileDigest=profile, imageDigest=image)
            bundle = Path(json.loads(subprocess.check_output(["/usr/bin/runc", "--root", args.runc_root, "state", container]))["bundle"])
            original_mode = bundle.stat().st_mode & 0o777
            try:
                os.chmod(bundle, 0o755)
                assert rpc(dict(operation="register", target=target), allow_error=True)["error"] == "host_bundle_untrusted"
            finally:
                os.chmod(bundle, original_mode)
            checks += ["runtime_bundle_must_remain_private"]
            assert rpc(dict(operation="register", target=target))["target"] == target
            task("takeover", **fence)
            if args.helper:
                helper("prepareWorkspace", "prepare-workspace")
                assert docker("exec", container, "/bin/busybox", "cat", "/workspace/from-image").stdout == b"preserved image data"
                assert docker("exec", container, "/bin/busybox", "stat", "-c", "%a", "/workspace").stdout.strip() == b"755"
                checks += ["prepare_workspace_preserves_image_content_and_existing_permissions"]
                _, inventory = helper("inputInventory", "inventory")
                assert json.loads(inventory)["inputs"] == []
                payload = bytes(range(251)) * 45843 + b"large-end"
                write_frame = filesystem_frame(dict(type="writeFile", content=[], observedFileHash=None, createOnly=True), payload)
                rejected = helper("fileSystem", "bad-digest", write_frame, "sha256:" + "0" * 64, allow_error=True)
                assert rejected["error"] == "host_helper_input_digest_mismatch"
                first, written = helper("fileSystem", "write-large", write_frame)
                decode_filesystem(written)
                repeated, replay = helper("fileSystem", "write-large", write_frame)
                assert repeated["task"]["taskId"] == first["task"]["taskId"] and replay == written
                _, read = helper("fileSystem", "read-large", filesystem_frame(dict(type="readFile", maxBytes=len(payload))))
                _, returned = decode_filesystem(read)
                assert returned == payload
                checks += ["static_sealed_helper_executes", "helper_rejects_digest_before_exec", "helper_large_binary_write_read", "helper_receipt_replays_without_rerun"]
            bare=task("start",operationId="1:bare-sh",scope=dict(kind="run",runId="shared"),program="sh",args=["-c","true"],env=dict(PATH="/bin"),cwd=workspace,timeoutMs=3000)
            bare_task=wait_for(bare["task"]["taskId"],lambda item:item["state"]=="exited" and item["outputComplete"])
            assert bare_task["task"]["exitCode"]==0
            absolute=task("start",operationId="1:absolute-sh",scope=dict(kind="run",runId="shared"),program="/bin/sh",args=["-c","true"],cwd=workspace,timeoutMs=3000)
            absolute_task=wait_for(absolute["task"]["taskId"],lambda item:item["state"]=="exited" and item["outputComplete"])
            assert absolute_task["task"]["exitCode"]==0
            checks += ["bare_and_absolute_programs_execute_under_the_same_profile"]
            scope = dict(kind="environment")
            script = ("test ! -e '" + str(control_socket) + "' || exit 91; "
                      "test ! -e '" + str(root / "control") + "' || exit 92; "
                      "if echo 0 >/sys/fs/cgroup/cgroup.procs 2>/dev/null; then exit 93; fi; "
                      "test \"$CENTAERIS_TEST_ENV\" = 'native value' || exit 94; "
                      "echo started >>"+workspace+"/starts; "
                      "(/bin/busybox sleep 60; echo background) & echo ready; exit 255")
            start_fields = dict(operationId="1:root-background", scope=scope, program="/bin/busybox",
                                args=["sh", "-c", script], env=dict(CENTAERIS_TEST_ENV="native value"), cwd=workspace, timeoutMs=0)
            started = task("start", **start_fields)
            task_id = started["task"]["taskId"]
            task("input", taskId=task_id, operationId="1:close", data=[], close=True)
            observed = wait_for(task_id, lambda item: item["state"] == "exited")
            assert observed["task"]["exitCode"] == 255
            assert not observed["task"]["cleanupComplete"]
            assert not observed["task"]["outputComplete"]
            assert task("start", **start_fields)["task"]["taskId"] == task_id
            checks += ["root_control_unreachable", "root_cgroup_readonly", "leader_exit_255",
                       "leader_exit_preserves_background_domain", "inherited_stdout_remains_open",
                       "duplicate_start_keeps_original_task"]
            checks += ["native_environment_works_without_image_env_utility"]
            task("endRun", runId="shared", operationId="1:end")
            assert not query(task_id)["task"]["cleanupComplete"]
            fence["controllerOwner"] = dict(kind="maintenanceOperation", operationId="shared")
            fence["controllerEpoch"] = 2
            task("takeover", **fence)
            maintenance = task("start", operationId="2:maintenance", scope=dict(kind="operation", operationId="shared"),
                               program="/bin/busybox", args=["sleep", "60"], cwd=workspace, timeoutMs=0)
            maintenance_id = maintenance["task"]["taskId"]
            task("endOperation", ownerOperationId="shared", operationId="2:end")
            wait_for(maintenance_id, lambda item: item["cleanupComplete"])
            assert not query(task_id)["task"]["cleanupComplete"]
            checks += ["end_run_preserves_environment_scope", "maintenance_owner_is_independent",
                       "end_operation_stops_only_its_scope"]
            service.terminate()
            service.wait(timeout=5)
            start_service()
            assert not query(task_id)["task"]["cleanupComplete"]
            task("terminate", taskId=task_id, operationId="2:stop-environment")
            wait_for(task_id, lambda item: item["cleanupComplete"])
            checks += ["supervisor_restart_retains_cleanup_obligation", "restart_can_stop_old_domain"]
            result = dict(checks=checks, daemonId=observed_id, instanceId=instance,
                          workloadUser=args.workload_user, workspaceRoot=workspace,
                          runcVersion=subprocess.check_output(["/usr/bin/runc", "--version"]).decode(),
                          supervisorSha256=hashlib.sha256(Path(args.supervisor).read_bytes()).hexdigest())
            if args.helper:
                result["helperSha256"] = "sha256:" + hashlib.sha256(helper_path.read_bytes()).hexdigest()
            Path(args.evidence).write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps(dict(passed=len(checks), evidence=args.evidence)))
        except Exception:
            print((root / "service.log").read_text(errors="replace"))
            if container:
                diagnostic = subprocess.run(["/usr/bin/runc", "--root", args.runc_root, "state", container],
                                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                if diagnostic.returncode == 0:
                    bundle = Path(json.loads(diagnostic.stdout)["bundle"])
                    for path in [bundle / "config.json", bundle, *bundle.parents]:
                        metadata = path.lstat()
                        print(json.dumps(dict(path=str(path), uid=metadata.st_uid,
                                              mode=oct(metadata.st_mode), symlink=path.is_symlink())))
            raise
        finally:
            if container:
                docker("rm", "-f", container, check=False)
                if docker("inspect", "--type", "container", container, check=False).returncode == 0:
                    raise RuntimeError("owned acceptance container cleanup unconfirmed")
            if service and service.poll() is None:
                service.terminate()
                service.wait(timeout=5)
            service_log.close()
            if image:
                docker("image", "rm", image, check=False)


if __name__ == "__main__":
    main()

"""Compose contract regressions using rendered configuration and synthetic values."""
import json
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
RETIRED = {
    "KNOWLEDGE_PROCESSOR_IMAGE", "RUNTIME_URL", "API_INTERNAL_URL", "REDIS_URL",
    "POSTGRES_HOST", "POSTGRES_PORT", "STORAGE_ROOT", "PLUGIN_CATALOG_ROOT",
}
REQUIRED_TEST_SECRETS = {
    "DJANGO_SECRET_KEY", "INTERNAL_API_TOKEN", "AGENT_RUN_AUTHORIZATION_SIGNING_KEY",
    "CREDENTIAL_ENCRYPTION_KEY", "POSTGRES_PASSWORD", "BOOTSTRAP_SUPERADMIN_PASSWORD",
}
RESIDENT_LIMIT_DEFAULTS = {
    "RESIDENT_GLOBAL_COUNT": "16",
    "RESIDENT_GLOBAL_MEMORY_BYTES": "34359738368",
    "RESIDENT_GLOBAL_CPU_MILLI": "16000",
    "RESIDENT_GLOBAL_PIDS": "8192",
    "RESIDENT_GLOBAL_WORKSPACE_BYTES": "34359738368",
    "RESIDENT_TENANT_COUNT": "4",
    "RESIDENT_TENANT_MEMORY_BYTES": "8589934592",
    "RESIDENT_TENANT_CPU_MILLI": "4000",
    "RESIDENT_TENANT_PIDS": "2048",
    "RESIDENT_TENANT_WORKSPACE_BYTES": "8589934592",
}
RESIDENT_HEADROOM_VARIABLES = (
    "RESIDENT_HOST_MEMORY_HEADROOM_BYTES", "RESIDENT_HOST_CPU_HEADROOM_MILLI",
)


def compose_config(**overrides):
    values = {}
    for line in (ROOT / ".env.example").read_text().splitlines():
        key, separator, value = line.partition("=")
        if separator and not key.startswith("#"):
            values[key] = value or ("synthetic-test-only" if key in REQUIRED_TEST_SECRETS else "")
    values["CENTAERIS_SOURCE_REVISION"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    values.update(overrides)
    env = {k: v for k, v in os.environ.items()
           if k not in values and k not in RETIRED and k not in RESIDENT_HEADROOM_VARIABLES}
    with tempfile.TemporaryDirectory(prefix="centaeris-compose-contract-") as temp:
        path = Path(temp) / "synthetic.env"
        path.write_text("\n".join(f"{k}={v}" for k, v in values.items() if v is not None), encoding="utf-8")
        result = subprocess.run(
            ["docker", "compose", "--env-file", str(path), "-f", str(ROOT / "docker-compose.yml"),
             "config", "--format", "json"],
            cwd=ROOT, env=env, capture_output=True, text=True, check=True,
        )
    return json.loads(result.stdout)


class DeploymentContractTests(unittest.TestCase):
    def test_optional_web_origins_preserve_empty_default_and_explicit_override(self):
        for origins in (None, "http://localhost:3100,https://workspace.example.invalid"):
            overrides = {} if origins is None else {"WEB_EXTRA_ORIGINS": origins}
            services = compose_config(**overrides)["services"]
            for service in ("api", "api-init"):
                with self.subTest(origins=origins, service=service):
                    self.assertEqual(services[service]["environment"]["WEB_EXTRA_ORIGINS"], origins or "")

    def test_images_build_from_workspace_without_adjacent_source_contexts(self):
        for name, service in compose_config()["services"].items():
            if "build" in service:
                with self.subTest(service=name):
                    self.assertEqual(Path(service["build"]["context"]).resolve(), ROOT)
                    self.assertFalse(service["build"].get("additional_contexts"))

    def test_all_image_labels_use_the_monorepo_revision(self):
        pin = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        services = compose_config()["services"]
        for name, service in services.items():
            build = service.get("build")
            if build is not None:
                with self.subTest(service=name):
                    self.assertEqual(build["labels"]["io.centaeris.source.revision"], pin)

    def test_long_running_foundation_services_restart_after_engine_recovery(self):
        services = compose_config()["services"]
        for service in ("postgres", "redis", "api"):
            with self.subTest(service=service):
                self.assertEqual(services[service].get("restart"), "unless-stopped")
        for service in ("api-init", "document-processor", "workspace-general"):
            with self.subTest(service=service):
                self.assertEqual(services[service].get("restart"), "no")

    def test_platform_mcp_accepts_runtime_internal_host_without_wildcards(self):
        services = compose_config()["services"]
        host = urlsplit(services["runtime"]["environment"]["API_INTERNAL_URL"]).hostname
        allowed = services["api"]["environment"].get("PLATFORM_MCP_ALLOWED_HOSTS", "localhost,127.0.0.1").split(",")
        self.assertIn(host, allowed)
        self.assertEqual(set(allowed), {host, "localhost", "127.0.0.1"})
        self.assertFalse(any("*" in value for value in allowed))

    def test_docker_release_gate_accepts_current_processor_ownership(self):
        source = (ROOT / "scripts/workspace/docker-release-gate.sh").read_text(encoding="utf-8")
        validator = source.split("python3 -c '\n", 1)[1].split("\n'\n", 1)[0]
        config = compose_config()
        env = {**os.environ, "WORKSPACE_ROOT": str(ROOT)}
        def validate(value):
            return subprocess.run([os.sys.executable, "-c", validator],
                                  input=json.dumps(value), env=env, capture_output=True, text=True)
        accepted = validate(config)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        del config["services"]["material-worker"]["depends_on"]["document-processor"]
        self.assertNotEqual(validate(config).returncode, 0)

    def test_docker_release_gate_starts_and_checks_material_worker(self):
        source = (ROOT / "scripts/workspace/docker-release-gate.sh").read_text(encoding="utf-8")
        start = next(line for line in source.splitlines() if line.startswith('"${compose[@]}" up -d --wait '))
        checked = next(line for line in source.splitlines() if line.startswith("for service in postgres "))
        self.assertIn("material-worker", start.split())
        self.assertIn("material-worker", checked.split())

    def test_runtime_image_does_not_bundle_docker_cli(self):
        dockerfile = (ROOT / "packages/runtime_server/Dockerfile").read_text(encoding="utf-8")
        self.assertNotIn("docker-cli", dockerfile)
        self.assertNotIn("/usr/local/bin/docker", dockerfile)

    def test_runtime_compose_starts_without_a_cli_preflight(self):
        runtime = compose_config()["services"]["runtime"]
        self.assertEqual(runtime["entrypoint"], ["runtime_server"])
        self.assertFalse(runtime.get("command"))

    def test_sandbox_and_material_processor_default_to_runsc(self):
        for values in ({}, {"OCI_RUNTIME": None}):
            services = compose_config(**values)["services"]
            for service in ("runtime", "material-worker"):
                with self.subTest(values=values, service=service):
                    self.assertEqual(services[service]["environment"]["OCI_RUNTIME"], "runsc")

    def test_explicit_runc_override_reaches_both_execution_services(self):
        services = compose_config(OCI_RUNTIME="runc")["services"]
        for service in ("runtime", "material-worker"):
            with self.subTest(service=service):
                self.assertEqual(services[service]["environment"]["OCI_RUNTIME"], "runc")

    def test_compose_does_not_replace_an_explicit_invalid_runtime(self):
        for value in ("", "RUNSC", "unknown", "runc,runsc"):
            services = compose_config(OCI_RUNTIME=value)["services"]
            for service in ("runtime", "material-worker"):
                with self.subTest(value=value, service=service):
                    self.assertEqual(services[service]["environment"]["OCI_RUNTIME"], value)

    def test_default_sandbox_resources_reach_authorization_service(self):
        services = compose_config()["services"]
        for service in ("api", "api-init"):
            with self.subTest(service=service):
                environment = services[service]["environment"]
                self.assertEqual(environment["SANDBOX_CPU_MILLI"], "4000")
                self.assertEqual(environment["SANDBOX_MEMORY_BYTES"], "8589934592")

    def test_worker_slot_configuration_reaches_container(self):
        self.assertEqual(compose_config()["services"]["worker"]["environment"]["WORKER_SLOT_COUNT"], "8")
        self.assertEqual(compose_config(WORKER_SLOT_COUNT="4")["services"]["worker"]["environment"]["WORKER_SLOT_COUNT"], "4")

    def test_runtime_postgres_connection_budgets_reach_runtime(self):
        environment = compose_config()["services"]["runtime"]["environment"]
        self.assertEqual(environment["RUNTIME_POSTGRES_POOL_SIZE"], "10")
        self.assertEqual(environment["RUNTIME_POSTGRES_CONTROL_POOL_SIZE"], "2")
        self.assertEqual(environment["RUNTIME_POSTGRES_LISTENER_LIMIT"], "8")
        self.assertEqual(environment["RUNTIME_POSTGRES_CHECKOUT_TIMEOUT_MS"], "5000")
        self.assertEqual(environment["RUNTIME_POSTGRES_CONNECT_TIMEOUT_MS"], "3000")

    def test_resident_limits_keep_finite_defaults_with_example_or_unset_inputs(self):
        for overrides in ({}, dict.fromkeys(RESIDENT_LIMIT_DEFAULTS)):
            environment = compose_config(**overrides)["services"]["runtime"]["environment"]
            for name, expected in RESIDENT_LIMIT_DEFAULTS.items():
                with self.subTest(unset=bool(overrides), variable=name):
                    self.assertEqual(environment.get(name), expected)

    def test_explicit_resident_limits_reach_runtime(self):
        configured = {name: str(101 + index) for index, name in enumerate(RESIDENT_LIMIT_DEFAULTS)}
        environment = compose_config(**configured)["services"]["runtime"]["environment"]
        for name, expected in configured.items():
            with self.subTest(variable=name):
                self.assertEqual(environment.get(name), expected)

    def test_resident_host_headroom_overrides_reach_runtime(self):
        configured = {
            "RESIDENT_HOST_MEMORY_HEADROOM_BYTES": "1073741824",
            "RESIDENT_HOST_CPU_HEADROOM_MILLI": "250",
        }
        environment = compose_config(**configured)["services"]["runtime"]["environment"]
        for name, expected in configured.items():
            with self.subTest(variable=name):
                self.assertEqual(environment.get(name), expected)

    def test_unset_resident_headroom_preserves_runtime_dynamic_defaults(self):
        environment = compose_config(**dict.fromkeys(RESIDENT_HEADROOM_VARIABLES))["services"]["runtime"]["environment"]
        for name in RESIDENT_HEADROOM_VARIABLES:
            with self.subTest(variable=name):
                # Compose renders unresolved pass-through values as null; this
                # leaves the variable unset in the container, unlike "".
                self.assertIsNone(environment.get(name))

    def test_invalid_resident_values_reach_runtime_without_compose_fallback(self):
        variables = (*RESIDENT_LIMIT_DEFAULTS, *RESIDENT_HEADROOM_VARIABLES)
        for value in ("", "0", "-1", "not-an-integer"):
            environment = compose_config(**dict.fromkeys(variables, value))["services"]["runtime"]["environment"]
            for name in variables:
                with self.subTest(variable=name, value=value):
                    self.assertIn(name, environment)
                    self.assertEqual(environment[name], value)

    def test_runtime_ordinary_pool_default_and_overrides_render_in_compose(self):
        for value, expected in ((None, "10"), ("6", "6"), ("10", "10")):
            with self.subTest(configured=value):
                environment = compose_config(RUNTIME_POSTGRES_POOL_SIZE=value)["services"]["runtime"]["environment"]
                self.assertEqual(environment["RUNTIME_POSTGRES_POOL_SIZE"], expected)
                self.assertEqual(environment["RUNTIME_POSTGRES_CONTROL_POOL_SIZE"], "2")

    def test_django_pool_budget_only_reaches_asgi_api(self):
        services = compose_config()["services"]
        self.assertEqual(services["api"]["environment"]["API_POSTGRES_POOL_MAX_SIZE"], "10")
        self.assertEqual(services["api"]["environment"]["POSTGRES_APPLICATION_NAME"], "centaeris-api")
        self.assertEqual(
            compose_config(API_POSTGRES_POOL_MAX_SIZE="0")["services"]["api"]["environment"]["API_POSTGRES_POOL_MAX_SIZE"],
            "0",
        )
        for service in ("api-init", "material-worker", "gc", "mail-sender"):
            with self.subTest(service=service):
                self.assertNotIn("API_POSTGRES_POOL_MAX_SIZE", services[service]["environment"])
                self.assertNotEqual(services[service]["environment"]["POSTGRES_APPLICATION_NAME"], "centaeris-api")
        self.assertNotIn("POSTGRES_HOST", services["worker"]["environment"])
        self.assertNotIn("DATABASE_URL", services["worker"]["environment"])

    def test_api_pool_default_and_explicit_overrides_render_in_compose(self):
        for value, expected in ((None, "10"), ("", "10"), ("0", "0"), ("6", "6"), ("10", "10")):
            with self.subTest(configured=value):
                rendered = compose_config(API_POSTGRES_POOL_MAX_SIZE=value)
                self.assertEqual(rendered["services"]["api"]["environment"]["API_POSTGRES_POOL_MAX_SIZE"], expected)

    def test_django_pool_settings_follow_the_api_deployment_budget(self):
        probe = (
            "import os; from api.settings import DATABASES; "
            "db = DATABASES['default']; "
            "size = int(os.environ.get('API_POSTGRES_POOL_MAX_SIZE', '0')); "
            "assert db['CONN_MAX_AGE'] == 0; "
            "assert (db['OPTIONS'].get('pool') == "
            "({'min_size': 0, 'max_size': size, 'timeout': 5} if size else None))"
        )
        for service, override in (("api", None), ("api", "0"), ("api", "6"), ("api-init", None)):
            with self.subTest(service=service, pool_size=override):
                environment = {
                    **os.environ,
                    **compose_config()["services"][service]["environment"],
                }
                if service != "api":
                    environment.pop("API_POSTGRES_POOL_MAX_SIZE", None)
                elif override is not None:
                    environment["API_POSTGRES_POOL_MAX_SIZE"] = override
                environment["CREDENTIAL_ENCRYPTION_KEY"] = (
                    "z5wA0vTzQGNG2LkVbNqnd3CPnGds4M8Xqy9lXgkqfZI="
                )
                environment["PYTHONPATH"] = str(ROOT / "packages" / "api")
                result = subprocess.run(
                    [os.sys.executable, "-c", probe],
                    cwd=ROOT, env=environment, capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_processor_build_extra_follows_exact_device(self):
        command = [os.sys.executable, str(ROOT / "packages/document_processor/processor_build_extra.py")]
        for device, extra in (("cpu", "cpu"), ("gpu:0", "gpu")):
            result = subprocess.run([*command, device], capture_output=True, text=True, check=True)
            self.assertEqual(result.stdout.strip(), extra)
        for device in ("gpu", "gpu:1", "CPU", ""):
            self.assertNotEqual(subprocess.run([*command, device], capture_output=True).returncode, 0)

    def test_image_gate_rejects_existing_wrong_image_and_device(self):
        spec = importlib.util.spec_from_file_location("image_gate", ROOT / "scripts/workspace/verify-deployment-images.py")
        gate = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(gate)
        config = compose_config()
        services = config["services"]
        processor_ref = services["document-processor"]["image"]
        general_ref = services["workspace-general"]["image"]
        images = {
            processor_ref: {"Id": "sha256:processor", "Config": {"Env": ["CENTAERIS_PROCESSOR_DEVICE=cpu"]}},
            general_ref: {"Id": "sha256:general"},
            "old-but-present:tag": {"Id": "sha256:old"},
        }
        gate.verify_images(config, images.__getitem__)
        services["material-worker"]["environment"]["MATERIAL_PROCESSOR_IMAGE"] = "old-but-present:tag"
        with self.assertRaisesRegex(ValueError, "different image"):
            gate.verify_images(config, images.__getitem__)
        services["material-worker"]["environment"]["MATERIAL_PROCESSOR_IMAGE"] = processor_ref
        services["material-worker"]["environment"]["MATERIAL_PROCESSOR_DEVICE"] = "gpu:0"
        with self.assertRaisesRegex(ValueError, "built device differs"):
            gate.verify_images(config, images.__getitem__)

    def test_processor_build_and_material_worker_have_one_identity(self):
        for device in ("cpu", "gpu:0"):
            with self.subTest(device=device):
                config = compose_config(KNOWLEDGE_PROCESSOR_DEVICE=device,
                                        KNOWLEDGE_PROCESSOR_IMAGE="stale-image:old")
                processor = config["services"]["document-processor"]
                runtime = config["services"]["material-worker"]["environment"]
                self.assertEqual(processor["image"], runtime["MATERIAL_PROCESSOR_IMAGE"])
                self.assertNotEqual(runtime["MATERIAL_PROCESSOR_IMAGE"], "stale-image:old")
                self.assertEqual(processor["build"]["args"]["PROCESSOR_DEVICE"], device)
                self.assertEqual(runtime["MATERIAL_PROCESSOR_DEVICE"], device)

    def test_runtime_port_change_reaches_api_and_worker(self):
        services = compose_config(RUNTIME_PORT="9100")["services"]
        runtime_url = f"http://runtime:{services['runtime']['environment']['RUNTIME_PORT']}"
        self.assertEqual(services["api"]["environment"]["RUNTIME_URL"], runtime_url)
        self.assertEqual(services["worker"]["environment"]["RUNTIME_INTERNAL_URL"], runtime_url)

    def test_compose_paths_remain_on_shared_named_volumes(self):
        services = compose_config(STORAGE_ROOT="/wrong/storage", PLUGIN_CATALOG_ROOT="/wrong/plugins")["services"]
        for name, variable, volume in [
            ("api", "STORAGE_ROOT", "storage-data"),
            ("gc", "STORAGE_ROOT", "storage-data"),
            ("api", "PLUGIN_CATALOG_ROOT", "plugin-data"),
            ("api-init", "PLUGIN_CATALOG_ROOT", "plugin-data"),
        ]:
            with self.subTest(service=name, variable=variable):
                service = services[name]
                mount = next(v for v in service["volumes"] if v["source"] == volume)
                self.assertEqual(service["environment"][variable], mount["target"])
        runtime_plugin = next(v for v in services["runtime"]["volumes"] if v["source"] == "plugin-data")
        self.assertTrue(runtime_plugin["read_only"])
        self.assertEqual(runtime_plugin["target"], services["api"]["environment"]["PLUGIN_CATALOG_ROOT"])

    def test_bundled_service_addresses_cannot_split(self):
        services = compose_config(API_INTERNAL_URL="http://wrong:1", REDIS_URL="redis://wrong:1",
                                  POSTGRES_HOST="wrong", POSTGRES_PORT="9999")["services"]
        for name in ("runtime", "worker"):
            self.assertEqual(services[name]["environment"]["API_INTERNAL_URL"], "http://api:8000")
        for name in ("api", "runtime"):
            self.assertEqual(services[name]["environment"]["REDIS_URL"], "redis://redis:6379/0")
        self.assertEqual(services["api"]["environment"]["POSTGRES_HOST"], "postgres")
        self.assertEqual(services["api"]["environment"]["POSTGRES_PORT"], "5432")
        self.assertIn("@postgres:5432/", services["runtime"]["environment"]["DATABASE_URL"])

    def test_api_drops_capabilities_and_blocks_privilege_gain(self):
        api = compose_config()["services"]["api"]
        self.assertEqual(api.get("cap_drop"), ["ALL"])
        self.assertIn("no-new-privileges:true", api.get("security_opt", []))

    def test_only_execution_services_mount_docker_socket(self):
        services = compose_config()["services"]
        holders = [name for name, service in services.items()
                   if any(v.get("source") == "/var/run/docker.sock" for v in service.get("volumes", []))]
        self.assertEqual(set(holders), {"runtime", "material-worker"})


if __name__ == "__main__":
    unittest.main()

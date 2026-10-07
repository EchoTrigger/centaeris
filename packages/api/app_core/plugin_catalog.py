import hashlib
import json
import unicodedata
from contextlib import contextmanager
from pathlib import Path

from django.conf import settings
from django.db import connection

from .credentials import validate_lower_kebab


PLUGIN_ACTIVATION_SCHEMA = "plugin_activation_snapshot_v1"
ACTIVATION_FIELDS = {"schema", "digest", "packages"}
PACKAGE_FIELDS = {
    "name",
    "version",
    "packageDigest",
    "skills",
    "cli",
    "mcpServers",
    "hooks",
}
RESOURCE_FIELDS = {"path", "digest"}
MANIFEST_FIELDS = {"name", "version", "paths", "interface"}
INTERFACE_FIELDS = {"displayName", "shortDescription", "capabilities"}
PLUGIN_LIFECYCLE_LOCK_ID = 0x43656E7461657269
MAX_PLUGIN_METADATA_BYTES = 4 * 1024 * 1024


def _read_plugin_metadata(path: Path, limit: int) -> bytes:
    with path.open("rb") as source:
        content = source.read(limit + 1)
    if len(content) > limit:
        raise ValueError("plugin metadata exceeds byte budget")
    return content


@contextmanager
def plugin_lifecycle_lock():
    with connection.cursor() as cursor:
        cursor.execute("SELECT pg_advisory_lock(%s)", [PLUGIN_LIFECYCLE_LOCK_ID])
    try:
        yield
    finally:
        with connection.cursor() as cursor:
            cursor.execute("SELECT pg_advisory_unlock(%s)", [PLUGIN_LIFECYCLE_LOCK_ID])


def load_plugin_catalog(*, require_packages: bool = True) -> dict:
    with plugin_lifecycle_lock():
        return load_plugin_catalog_at(
            Path(settings.PLUGIN_CATALOG_ROOT), require_packages=require_packages
        )


def load_plugin_catalog_at(root: Path, *, require_packages: bool = True) -> dict:
    path = root / "catalog.snapshot.json"
    try:
        payload = json.loads(_read_plugin_metadata(path, MAX_PLUGIN_METADATA_BYTES).decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"plugin_catalog_unavailable:{path}") from error
    validate_plugin_activation(payload)
    for package in payload["packages"]:
        if require_packages and not (root / package["name"]).is_dir():
            raise ValueError(f"plugin_package_missing:{package['name']}")
    return payload


def load_plugin_interfaces(catalog: dict, root: Path | None = None) -> dict[str, dict]:
    catalog_root = Path(settings.PLUGIN_CATALOG_ROOT) if root is None else root
    interfaces = {}
    for package in catalog["packages"]:
        path = (
            catalog_root
            / package["name"]
            / ".centaeris-plugin"
            / "plugin.json"
        )
        try:
            manifest = json.loads(_read_plugin_metadata(path, MAX_PLUGIN_METADATA_BYTES).decode("utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError(f"plugin_manifest_unavailable:{package['name']}") from error
        if (
            not isinstance(manifest, dict)
            or set(manifest) - MANIFEST_FIELDS
            or manifest.get("name") != package["name"]
            or manifest.get("version") != package["version"]
        ):
            raise ValueError(f"plugin_manifest_mismatch:{package['name']}")
        raw_interface = manifest.get("interface")
        interface = {} if raw_interface is None else raw_interface
        if (
            not isinstance(interface, dict)
            or set(interface) - INTERFACE_FIELDS
            or not isinstance(interface.get("displayName", package["name"]), str)
            or not isinstance(interface.get("shortDescription", ""), str)
            or not isinstance(interface.get("capabilities", []), list)
            or any(
                not isinstance(capability, str)
                for capability in interface.get("capabilities", [])
            )
        ):
            raise ValueError(f"plugin_interface_invalid:{package['name']}")
        interfaces[package["name"]] = {
            "displayName": interface.get("displayName") or package["name"],
            "shortDescription": interface.get("shortDescription") or "",
            "capabilities": interface.get("capabilities", []),
        }
    return interfaces


def _verified_mcp_documents(package: dict) -> list[tuple[dict, dict]]:
    """Read management-only credential identities, not executable tool contracts."""
    root = Path(settings.PLUGIN_CATALOG_ROOT).resolve(strict=True)
    package_root = root / package["name"]
    documents = []
    remaining_bytes = MAX_PLUGIN_METADATA_BYTES
    for resource in package["mcpServers"]:
        _require_resource_path(resource["path"])
        path = package_root
        for part in ("", *resource["path"].split("/")):
            path = path / part
            if path.is_symlink() or not path.resolve(strict=True).is_relative_to(package_root):
                raise ValueError("plugin credential resource escaped package")
        content = _read_plugin_metadata(path, remaining_bytes)
        remaining_bytes -= len(content)
        digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
        for data in (resource["path"].encode("utf-8"), content):
            digest.update(len(data).to_bytes(8, "big"))
            digest.update(data)
        if f"sha256:{digest.hexdigest()}" != resource["digest"]:
            raise ValueError("plugin credential resource digest mismatch")
        payload = json.loads(content.decode("utf-8"))
        _require_exact_fields(payload, {"schema", "servers"}, "MCP credential document")
        if payload["schema"] != "mcp_servers_v1" or not isinstance(payload["servers"], list):
            raise ValueError("MCP credential document schema mismatch")
        for server in payload["servers"]:
            if not isinstance(server, dict) or set(server) - {
                "id", "transport", "modelContractDigest", "lifecycle",
                "startupTimeoutMs", "toolTimeoutMs", "tools",
            }:
                raise ValueError("MCP credential server fields mismatch")
            transport = server.get("transport")
            if not isinstance(transport, dict):
                raise ValueError("MCP credential transport missing")
            if transport.get("type") == "stdio":
                _require_exact_fields(transport, {"type", "program", "args"}, "MCP credential transport")
                continue
            if transport.get("type") != "streamableHttp" or set(transport) not in (
                {"type", "url"}, {"type", "url", "bearerCredentialRef"},
            ):
                raise ValueError("MCP credential transport fields mismatch")
            if "bearerCredentialRef" in transport:
                validate_lower_kebab("MCP bearer credential ref", transport["bearerCredentialRef"])
        documents.append((resource, payload))
    return documents


def load_plugin_bearer_credential_refs(package: dict) -> list[str]:
    return sorted({server["transport"]["bearerCredentialRef"]
        for _, document in _verified_mcp_documents(package) for server in document["servers"]
        if "bearerCredentialRef" in server["transport"]})


def load_plugin_connector_resources(package: dict) -> list[dict]:
    """Management identities from integrity-checked declarations; Core owns tool semantics."""
    resources = []
    identities = set()
    for resource, document in _verified_mcp_documents(package):
        for server in document["servers"]:
            server_id = validate_lower_kebab("MCP server identity", server["id"])
            if server_id in identities:
                raise ValueError("duplicate connector server identity")
            identities.add(server_id)
            resources.append({"serverId": server_id, "resourcePath": resource["path"],
                "resourceDigest": resource["digest"], "credentialRef": server["transport"].get("bearerCredentialRef")})
    return resources


def plugin_activation_for_workspace(workspace) -> dict:
    catalog = load_plugin_catalog()
    enabled = set(
        workspace.pluginEnablements.values_list("pluginName", flat=True)
    )
    packages_by_name = {package["name"]: package for package in catalog["packages"]}
    missing = sorted(enabled - packages_by_name.keys())
    if missing:
        raise ValueError(f"enabled_plugin_not_in_release_catalog:{missing[0]}")
    packages = [
        package for package in catalog["packages"] if package["name"] in enabled
    ]
    activation = {
        "schema": PLUGIN_ACTIVATION_SCHEMA,
        "digest": activation_digest(packages),
        "packages": packages,
    }
    validate_plugin_activation(activation)
    return activation


def validate_plugin_names(names: list[str]) -> None:
    if not isinstance(names, list) or any(not isinstance(name, str) for name in names):
        raise ValueError("plugin names must be a list of identities")
    for name in names:
        validate_lower_kebab("plugin name", name)
    if names != sorted(set(names)):
        raise ValueError("plugin names must be sorted and unique")


def selected_plugin_activation(workspace, names: list[str]) -> dict:
    validate_plugin_names(names)
    if not names:
        return {"schema": PLUGIN_ACTIVATION_SCHEMA, "digest": activation_digest([]), "packages": []}
    available = plugin_activation_for_workspace(workspace)
    packages = [package for package in available["packages"] if package["name"] in names]
    if len(packages) != len(names):
        raise ValueError("agent_definition_plugin_not_enabled")
    return {"schema": PLUGIN_ACTIVATION_SCHEMA, "digest": activation_digest(packages), "packages": packages}


def require_current_plugin_activation(workspace, activation: dict) -> None:
    validate_plugin_activation(activation)
    names = [package["name"] for package in activation["packages"]]
    if selected_plugin_activation(workspace, names) != activation:
        raise ValueError("agent_definition_plugin_snapshot_unavailable")


def activation_digest(packages: list[dict]) -> str:
    encoded = json.dumps(
        {"schema": PLUGIN_ACTIVATION_SCHEMA, "packages": packages},
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def validate_plugin_activation(payload: dict) -> None:
    _require_exact_fields(payload, ACTIVATION_FIELDS, "plugin activation")
    if payload["schema"] != PLUGIN_ACTIVATION_SCHEMA:
        raise ValueError("plugin_activation_schema_mismatch")
    _require_sha256("plugin activation digest", payload["digest"])
    packages = payload["packages"]
    if not isinstance(packages, list):
        raise ValueError("plugin activation packages must be a list")
    if packages != sorted(
        packages,
        key=lambda package: (package.get("name", ""), package.get("packageDigest", "")),
    ):
        raise ValueError("plugin activation packages must be sorted")
    names = set()
    cli_names = set()
    for package in packages:
        _require_exact_fields(package, PACKAGE_FIELDS, "activated plugin package")
        name = package["name"]
        if not _is_lower_kebab(name) or name in names:
            raise ValueError(f"invalid or duplicate activated plugin name:{name}")
        names.add(name)
        _require_version(package["version"])
        _require_sha256("plugin package digest", package["packageDigest"])
        for kind in ("skills", "cli", "mcpServers", "hooks"):
            resources = package[kind]
            if not isinstance(resources, list):
                raise ValueError(f"plugin {kind} must be a list")
            paths = []
            for resource in resources:
                _require_exact_fields(resource, RESOURCE_FIELDS, "plugin resource")
                _require_resource_path(resource["path"])
                _require_sha256("plugin resource digest", resource["digest"])
                paths.append(resource["path"])
            if paths != sorted(set(paths)):
                raise ValueError(f"plugin {kind} resources must be sorted and unique")
        for resource in package["cli"]:
            cli_name = resource["path"].rsplit("/", 1)[-1]
            if cli_name in cli_names:
                raise ValueError(f"duplicate plugin CLI identity:{cli_name}")
            cli_names.add(cli_name)
    if activation_digest(packages) != payload["digest"]:
        raise ValueError("plugin activation snapshot digest mismatch")


def _require_exact_fields(payload, expected: set[str], label: str) -> None:
    if not isinstance(payload, dict) or set(payload) != expected:
        raise ValueError(f"{label} fields mismatch")


def _require_sha256(label: str, value) -> None:
    if (
        not isinstance(value, str)
        or len(value) != 71
        or not value.startswith("sha256:")
        or any(character not in "0123456789abcdef" for character in value[7:])
    ):
        raise ValueError(f"{label} must be lowercase sha256")


def _is_lower_kebab(value) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value) <= 64
        and not value.startswith("-")
        and not value.endswith("-")
        and "--" not in value
        and all(
            character.isascii()
            and (character.islower() or character.isdigit() or character == "-")
            for character in value
        )
    )


def _require_version(value) -> None:
    if not isinstance(value, str):
        raise ValueError("plugin version must be major.minor.patch")
    parts = value.split(".")
    if len(parts) != 3 or any(
        not part.isascii()
        or not part.isdigit()
        or (len(part) > 1 and part.startswith("0"))
        for part in parts
    ):
        raise ValueError("plugin version must be major.minor.patch")


def _require_resource_path(value) -> None:
    if (
        not isinstance(value, str)
        or not value
        or value.startswith("/")
        or "\\" in value
        or ":" in value
        or any(part in {"", ".", ".."} for part in value.split("/"))
        or any(unicodedata.category(character).startswith("C") for character in value)
        or unicodedata.normalize("NFC", value) != value
    ):
        raise ValueError("plugin resource path must be canonical relative POSIX")

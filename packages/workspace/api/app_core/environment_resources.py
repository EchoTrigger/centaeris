"""Deployment defaults and the existing environment resource transport contract."""
from django.conf import settings


RESOURCE_FIELDS = frozenset({"cpuMilli", "memoryBytes", "pidsLimit", "workspaceBytes", "storageBytes"})
MAX_RESOURCE_COUNT = 2**32 - 1
MAX_RESOURCE_BYTES = 2**64 - 1
MAX_SETTINGS_BYTES = 2**53 - 1


def validate_environment_resources(value):
    if not isinstance(value, dict) or set(value) != RESOURCE_FIELDS:
        raise ValueError("environment profile resources fields mismatch")
    for field, number in value.items():
        maximum = MAX_RESOURCE_COUNT if field in {"cpuMilli", "pidsLimit"} else MAX_RESOURCE_BYTES
        if type(number) is not int or number <= 0:
            raise ValueError("environment profile resources must be positive integers")
        if number > maximum:
            raise ValueError(f"environment profile resources.{field} exceeds transport range")


def default_environment_resources():
    resources = {"cpuMilli": settings.SANDBOX_CPU_MILLI,
        "memoryBytes": settings.SANDBOX_MEMORY_BYTES, "pidsLimit": settings.SANDBOX_PIDS_LIMIT,
        "workspaceBytes": settings.SANDBOX_WORKSPACE_BYTES, "storageBytes": settings.SANDBOX_WORKSPACE_BYTES}
    validate_environment_resources(resources)
    return resources

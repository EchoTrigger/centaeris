"""Ordinary SDK clients for an already-running exact perRun sandbox.

This pinned lower-level SDK seam only observes provider information. It does not
resume an instance, extend its timeout or recover lost command output.
"""
from dataclasses import dataclass, field

from .control import read_existing_per_run


@dataclass(frozen=True)
class AttachedSandbox:
    sandbox_id: str
    envd_api_url: str = field(repr=False)
    connection_config: object = field(repr=False)
    files: object = field(repr=False)
    commands: object = field(repr=False)


def attach_existing(config, payload):
    from packaging.version import Version
    from e2b.api.client_sync import get_envd_api
    from e2b.sandbox_sync.filesystem.filesystem import Filesystem
    from e2b.sandbox_sync.commands.command import Commands

    context = read_existing_per_run(config, payload)
    version = Version(context.detail.envd_version)
    envd_api = get_envd_api(context.connection, context.envd_url)
    sandbox = AttachedSandbox(context.detail.sandbox_id, context.envd_url, context.connection,
        Filesystem(context.envd_url, version, context.connection, envd_api),
        Commands(context.envd_url, context.connection, version, envd_api))
    return sandbox, context.detail
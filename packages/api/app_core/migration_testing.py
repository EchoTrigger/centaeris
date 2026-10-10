"""Independent old-schema databases for forward migration contract tests."""
from contextlib import contextmanager
from threading import get_ident
from uuid import uuid4

from django.db import DEFAULT_DB_ALIAS, connections, router
from django.db.migrations.executor import MigrationExecutor
from django.db.migrations.state import StateApps


class _MigrationFixtureRouter:
    def __init__(self, alias, route_current_models):
        self.alias = alias
        self.route_current_models = route_current_models
        self.thread = get_ident()

    def db_for_read(self, model, **hints):
        if get_ident() == self.thread and (
            self.route_current_models or isinstance(model._meta.apps, StateApps)
        ):
            return self.alias

    db_for_write = db_for_read


@contextmanager
def _fixture_routing(alias, route_current_models):
    previous = router.routers
    router.routers = [_MigrationFixtureRouter(alias, route_current_models), *previous]
    try:
        yield
    finally:
        router.routers = previous


@contextmanager
def isolated_migration_database(targets, *, route_current_models=False):
    """Never migrate or replace the shared test runner's default database.

    The connection is registered only in this thread, not in DATABASES. Explicit
    ORM aliases and historical data-migration models use the isolated database.
    Browser-login fixtures can also route their real current Client/SessionStore
    models there. Routing and cleanup unwind on migration or assertion failure.
    """
    source = connections[DEFAULT_DB_ALIAS]
    if source.in_atomic_block:
        raise RuntimeError("migration_fixture_requires_transaction_test_case")
    alias = "test_migration_" + uuid4().hex
    isolated = source.copy(alias=alias)
    isolated.settings_dict.update(CONN_MAX_AGE=0, AUTOCOMMIT=True)
    isolated.settings_dict["OPTIONS"].pop("pool", None)
    created = False
    connections[alias] = isolated
    try:
        if source.vendor == "postgresql":
            if not str(source.settings_dict["NAME"]).startswith("test_"):
                raise RuntimeError("migration_fixture_requires_test_database")
            isolated.settings_dict["NAME"] = alias
            with source._nodb_cursor() as cursor:
                cursor.execute(f"CREATE DATABASE {source.ops.quote_name(alias)} TEMPLATE template0")
            created = True
        elif source.vendor == "sqlite":
            isolated.settings_dict["NAME"] = ":memory:"
        else:
            raise RuntimeError("migration_fixture_backend_unsupported")
        with _fixture_routing(alias, route_current_models):
            executor = MigrationExecutor(isolated)
            target_apps = {app for app, _migration in targets}
            # Old fixtures downgraded only the target app while contrib schemas
            # stayed current. Preserve that baseline for real auth/session code.
            dependencies = [node for node in executor.loader.graph.leaf_nodes()
                            if node[0] not in target_apps]
            executor.migrate([*targets, *dependencies])
            yield isolated
    finally:
        try:
            isolated.close()
        finally:
            del connections[alias]
            if created:
                with source._nodb_cursor() as cursor:
                    cursor.execute(f"DROP DATABASE {source.ops.quote_name(alias)}")

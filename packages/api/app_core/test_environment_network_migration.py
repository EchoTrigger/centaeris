"""Network policy history and database enforcement use disposable PostgreSQL."""

import json
from pathlib import Path
from types import SimpleNamespace

from django.db import IntegrityError, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_authorization_fixtures import historical_profile_digest as benchmark_profile_digest
from .migration_testing import isolated_migration_database
from .migration_authorization_fixtures import historical_digest as authorization_digest, historical_signature as authorization_signature
from .migration_authorization_fixtures import historical_profile as profile


class EnvironmentNetworkMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0020_benchmark_execution_profile")]
    current = [("app_core", "0021_environment_network")]

    def fixture(self, database, targets):
        apps = MigrationExecutor(database).loader.project_state(targets).apps
        manager = lambda name: apps.get_model("app_core", name).objects.using(database.alias)
        user = apps.get_model("auth", "User").objects.using(database.alias).create(username="network-migration")
        workspace = manager("Workspace").create(name="Network", createdBy_id=user.pk)
        member = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
        agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Network")
        config = manager("ModelConfig").create(displayName="Network")
        return SimpleNamespace(apps=apps, manager=manager, user=user, workspace=workspace,
                               member=member, agent=agent, config=config)

    def session(self, database, fixture, **fields):
        with transaction.atomic(using=database.alias):
            session = fixture.manager("Session").create(workspace_id=fixture.workspace.pk,
                owner_id=fixture.user.pk, agent_id=fixture.agent.pk, **fields)
            environment = fixture.manager("SessionEnvironment").create(session_id=session.pk)
            fixture.manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
        environment.refresh_from_db(using=database.alias)
        return session, environment

    def bind(self, database, fixture, environment, *, network="publicInternet"):
        value = profile() | {"network": network}
        fields = {"benchmark_profile": value, "benchmark_profile_digest": benchmark_profile_digest(value),
            "benchmark_profile_operation_id": "bind-" + environment.pk,
            "benchmark_profile_bound_at": timezone.now()}
        if "network" in {field.name for field in environment._meta.fields}:
            fields["network"] = network
        fixture.manager("SessionEnvironment").filter(pk=environment.pk).update(**fields)
        environment.refresh_from_db(using=database.alias)

    def run_and_grant(self, fixture, session, environment, version, *, corrupt_network=False):
        root = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/historical_agent_run_authorization"
        name = "benchmark.json" if version == 3 and environment.benchmark_profile else "ordinary.json" if version == 3 else "valid.json"
        payload = json.loads((root / f"v{version}" / name).read_text(encoding="utf-8"))
        run = fixture.manager("AgentRun").create(workspace_id=fixture.workspace.pk, user_id=fixture.user.pk,
            session_id=session.pk, environment_id=environment.pk, membership_ref=fixture.member.pk,
            modelConfig_id=fixture.config.pk, prompt="retained network", thinkingMode="high", status="queued")
        payload.update(id="authorization-" + run.pk, workspaceId=fixture.workspace.pk,
            userId=str(fixture.user.pk), sessionId=session.pk, agentId=fixture.agent.pk,
            agentRunId=run.pk, modelConfigRef=fixture.config.pk)
        if version == 3:
            payload["network"] = environment.network
        digest, signature = authorization_digest(payload), authorization_signature(payload, "test-key")
        if corrupt_network:
            # A raw ledger insert can bypass application validation. Even this
            # malformed common network must not authorize benchmark setup.
            payload["network"] = "disabled"
        grant = fixture.manager("AgentRunAuthorization").create(id=payload["id"], agent_run_id=run.pk,
            payload=payload, digest=digest, signature=signature)
        return run, grant

    def complete_setup(self, database, environment, run):
        with database.cursor() as cursor:
            cursor.execute("""UPDATE app_core_sessionenvironment SET benchmark_setup_operation_id=%s,
                benchmark_setup_run_id=%s, benchmark_setup_instance_id=%s, benchmark_setup_container_id=%s,
                benchmark_setup_completed_at=%s WHERE id=%s""",
                ["setup-complete", run.pk, "instance_" + "a" * 64, "b" * 64, timezone.now(), environment.pk])

    def test_forward_backfills_benchmark_network_without_rewriting_v1_v2_history(self):
        with isolated_migration_database(self.old) as database:
            fixture = self.fixture(database, self.old)
            now = timezone.now()
            ordinary, disabled = self.session(database, fixture, status="deleted", deletedAt=now, purgedAt=now)
            self.run_and_grant(fixture, ordinary, disabled, 1)
            benchmark, internet = self.session(database, fixture)
            self.bind(database, fixture, internet)
            run, _ = self.run_and_grant(fixture, benchmark, internet, 2)
            self.complete_setup(database, internet, run)
            original = {name: list(fixture.manager(name).order_by("pk").values())
                for name in ("Workspace", "Session", "SessionEnvironment", "AgentRun", "AgentRunAuthorization")}

            def check_upgraded():
                state = MigrationExecutor(database).loader.project_state(self.current).apps
                environments = state.get_model("app_core", "SessionEnvironment").objects.using(database.alias)
                self.assertEqual(environments.get(pk=disabled.pk).network, "disabled")
                self.assertEqual(environments.get(pk=internet.pk).network, "publicInternet")
                workspace = state.get_model("app_core", "Workspace").objects.using(database.alias).get(pk=fixture.workspace.pk)
                self.assertEqual(workspace.execution_network, "disabled")
                for name, rows in original.items():
                    self.assertEqual(list(fixture.manager(name).order_by("pk").values()), rows)
                for grant in fixture.manager("AgentRunAuthorization").all():
                    self.assertEqual(authorization_digest(grant.payload), grant.digest)
                    self.assertEqual(authorization_signature(grant.payload, "test-key"), grant.signature)

            MigrationExecutor(database).migrate(self.current)
            check_upgraded()
            # A v2 benchmark's network remains authoritative in its unchanged
            # profile, so this lossless rollback and reconstruction are allowed.
            MigrationExecutor(database).migrate(self.old)
            MigrationExecutor(database).migrate(self.current)
            check_upgraded()

    def test_public_ordinary_policy_and_v3_history_each_block_destructive_reversal(self):
        for fact in ("workspace", "environment", "v3_grant"):
            with self.subTest(fact=fact), isolated_migration_database(self.current) as database:
                fixture = self.fixture(database, self.current)
                if fact in {"workspace", "environment"}:
                    fixture.manager("Workspace").filter(pk=fixture.workspace.pk).update(execution_network="publicInternet")
                session, environment = self.session(database, fixture)
                if fact == "environment":
                    fixture.manager("Workspace").filter(pk=fixture.workspace.pk).update(execution_network="disabled")
                if fact == "v3_grant":
                    self.run_and_grant(fixture, session, environment, 3)
                with self.assertRaisesRegex(RuntimeError, "environment_network_history_requires_forward_migration"):
                    MigrationExecutor(database).migrate(self.old)
                environment.refresh_from_db(using=database.alias)
                changed = "disabled" if environment.network == "publicInternet" else "publicInternet"
                with self.assertRaisesRegex(IntegrityError, "environment_network_immutable"), transaction.atomic(using=database.alias):
                    fixture.manager("SessionEnvironment").filter(pk=environment.pk).update(network=changed)

    def test_raw_and_bulk_writes_cannot_forge_or_mutate_the_workspace_snapshot(self):
        with isolated_migration_database(self.current) as database:
            fixture = self.fixture(database, self.current)
            fixture.manager("Workspace").filter(pk=fixture.workspace.pk).update(execution_network="publicInternet")
            session, environment = self.session(database, fixture)
            Environment = fixture.apps.get_model("app_core", "SessionEnvironment")
            bulk = Environment(session_id=session.pk, network="disabled")
            fixture.manager("SessionEnvironment").bulk_create([bulk])
            bulk.refresh_from_db(using=database.alias)
            self.assertEqual(bulk.network, "publicInternet")

            # Clone through SQL with a forged network; the INSERT trigger must
            # still snapshot the Workspace policy instead of trusting the row.
            columns, projections, params = [], [], []
            for field in Environment._meta.local_fields:
                columns.append(database.ops.quote_name(field.column))
                if field.name in {"id", "network"}:
                    projections.append("%s")
                    params.append("raw-network-environment" if field.name == "id" else "disabled")
                else:
                    projections.append(database.ops.quote_name(field.column))
            params.append(environment.pk)
            with database.cursor() as cursor:
                cursor.execute(f'INSERT INTO app_core_sessionenvironment ({",".join(columns)}) '
                    f'SELECT {",".join(projections)} FROM app_core_sessionenvironment WHERE id=%s', params)
            self.assertEqual(fixture.manager("SessionEnvironment").get(pk="raw-network-environment").network, "publicInternet")

            for method in ("queryset", "bulk", "raw"):
                with self.subTest(method=method), self.assertRaisesRegex(IntegrityError, "environment_network_immutable"), transaction.atomic(using=database.alias):
                    if method == "queryset":
                        fixture.manager("SessionEnvironment").filter(pk=environment.pk).update(network="disabled")
                    elif method == "bulk":
                        bulk.network = "disabled"
                        fixture.manager("SessionEnvironment").bulk_update([bulk], ["network"])
                    else:
                        with database.cursor() as cursor:
                            cursor.execute("UPDATE app_core_sessionenvironment SET network=%s WHERE id=%s", ["disabled", environment.pk])
            with self.assertRaises(IntegrityError), transaction.atomic(using=database.alias):
                fixture.manager("Workspace").filter(pk=fixture.workspace.pk).update(execution_network="host")
            fixture.manager("Workspace").filter(pk=fixture.workspace.pk).update(execution_network="disabled")
            _, later = self.session(database, fixture)
            self.assertEqual(later.network, "disabled")
            environment.refresh_from_db(using=database.alias)
            self.assertEqual(environment.network, "publicInternet")

    def test_only_unused_matching_benchmark_binding_can_change_the_snapshot(self):
        with isolated_migration_database(self.current) as database:
            fixture = self.fixture(database, self.current)
            session, environment = self.session(database, fixture)
            value = profile()
            fields = {"benchmark_profile": value, "benchmark_profile_digest": benchmark_profile_digest(value),
                "benchmark_profile_operation_id": "bind-unused", "benchmark_profile_bound_at": timezone.now()}
            with self.assertRaisesRegex(IntegrityError, "environment_network_profile_mismatch"), transaction.atomic(using=database.alias):
                fixture.manager("SessionEnvironment").filter(pk=environment.pk).update(**fields)
            fixture.manager("SessionEnvironment").filter(pk=environment.pk).update(network="publicInternet", **fields)
            environment.refresh_from_db(using=database.alias)
            self.assertEqual(environment.network, "publicInternet")
            with self.assertRaises(IntegrityError), transaction.atomic(using=database.alias):
                self.bind(database, fixture, environment, network="disabled")
            used, retained = self.session(database, fixture)
            self.run_and_grant(fixture, used, retained, 1)
            with self.assertRaisesRegex(IntegrityError, "benchmark_profile_requires_unused_session"), transaction.atomic(using=database.alias):
                self.bind(database, fixture, retained)
            retained.refresh_from_db(using=database.alias)
            self.assertEqual(retained.network, "disabled")
            self.assertIsNone(retained.benchmark_profile)

    def test_raw_v3_setup_accepts_matching_common_network_and_rejects_mismatch(self):
        with isolated_migration_database(self.current) as database:
            fixture = self.fixture(database, self.current)
            for corrupt in (False, True):
                with self.subTest(corrupt_network=corrupt):
                    session, environment = self.session(database, fixture)
                    self.bind(database, fixture, environment)
                    run, _ = self.run_and_grant(fixture, session, environment, 3, corrupt_network=corrupt)
                    if corrupt:
                        with self.assertRaisesRegex(IntegrityError, "benchmark_setup_authorization_invalid"), transaction.atomic(using=database.alias):
                            self.complete_setup(database, environment, run)
                    else:
                        self.complete_setup(database, environment, run)
                    environment.refresh_from_db(using=database.alias)
                    self.assertEqual(environment.benchmark_setup_operation_id, "" if corrupt else "setup-complete")

    def test_invalid_historical_benchmark_network_aborts_upgrade_without_partial_columns(self):
        with isolated_migration_database(self.old) as database:
            fixture = self.fixture(database, self.old)
            _, environment = self.session(database, fixture)
            value = profile() | {"network": "host"}
            fixture.manager("SessionEnvironment").filter(pk=environment.pk).update(benchmark_profile=value,
                benchmark_profile_digest="sha256:" + "a" * 64, benchmark_profile_operation_id="invalid-history",
                benchmark_profile_bound_at=timezone.now())
            with self.assertRaisesRegex(RuntimeError, "historical_environment_network_invalid"):
                MigrationExecutor(database).migrate(self.current)
            environment.refresh_from_db(using=database.alias)
            self.assertEqual(environment.benchmark_profile, value)
            with database.cursor() as cursor:
                fields = {field.name for field in database.introspection.get_table_description(cursor, "app_core_sessionenvironment")}
            self.assertNotIn("network", fields)

    def test_insert_race_returns_the_database_network_snapshot_to_the_caller(self):
        from .models import Session, SessionEnvironment

        with isolated_migration_database(self.current, route_current_models=True) as database:
            fixture = self.fixture(database, self.current)
            writer = database.copy(alias=database.alias + "_policy_writer")
            changed = False
            insertion = f'INSERT INTO {database.ops.quote_name(SessionEnvironment._meta.db_table)}'

            def change_policy_before_environment_insert(execute, sql, params, many, context):
                nonlocal changed
                if not changed and sql.startswith(insertion):
                    changed = True
                    with writer.cursor() as cursor:
                        cursor.execute("UPDATE app_core_workspace SET execution_network=%s WHERE id=%s",
                                       ["publicInternet", fixture.workspace.pk])
                return execute(sql, params, many, context)

            try:
                with database.execute_wrapper(change_policy_before_environment_insert):
                    session = Session.objects.using(database.alias).create(
                        workspace_id=fixture.workspace.pk, owner_id=fixture.user.pk, agent_id=fixture.agent.pk)
            finally:
                writer.close()
            self.assertTrue(changed, "the fixture must commit the policy change between read and insert")
            stored = SessionEnvironment.objects.using(database.alias).get(pk=session.current_environment_id)
            self.assertEqual(stored.network, "publicInternet")
            self.assertEqual(session.current_environment.network, stored.network)

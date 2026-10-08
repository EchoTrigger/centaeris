"""Run focused Runtime PostgreSQL regressions in a fresh disposable database."""
import os
from pathlib import Path
import subprocess
import uuid
from urllib.parse import quote


def database_settings():
    return {key: os.environ.get('TEST_POSTGRES_' + env_key, default)
            for key, env_key, default in (
                ('host', 'HOST', 'localhost'), ('port', 'PORT', '55432'),
                ('dbname', 'DB', 'centaeris'), ('user', 'USER', 'centaeris'),
                ('password', 'PASSWORD', 'centaeris'))}


def discovered_tests(output):
    names = [line.removesuffix(': test') for line in output.splitlines() if line.endswith(': test')]
    if not names or len(names) != len(set(names)):
        raise RuntimeError('outbox test discovery is empty or contains duplicates')
    return names


def main():
    import psycopg
    from psycopg import sql

    root = Path(__file__).resolve().parents[2]
    settings = database_settings()
    database = 'test_outbox_' + uuid.uuid4().hex
    host = settings['host']
    if ':' in host and not host.startswith('['):
        host = '[' + host + ']'
    url = f"postgresql://{quote(settings['user'], safe='')}:{quote(settings['password'], safe='')}@{host}:{settings['port']}/{database}"
    env = {**os.environ, 'CENTAERIS_ALLOW_POSTGRES_TEST_RESET': '1',
           'CENTAERIS_TEST_POSTGRES_URL': url, 'INTERNAL_API_TOKEN': uuid.uuid4().hex,
           'CARGO_BUILD_JOBS': '1', 'EXECUTION_GLOBAL_LIMIT': '8',
           'EXECUTION_TENANT_LIMIT': '4', 'CENTAERIS_CAPTURE_BOUNDARY_DATABASE': url,
           'CENTAERIS_PROJECTION_BOUNDARY_DATABASE': url}
    commands = [['cargo', 'test', '--locked', '-p', 'runtime_server',
                 'transcript_capture::tests::transcript_capture_second_run_requires_committed_session_sequence',
                 '--', '--exact'],
                ['cargo', 'test', '--locked', '-p', 'runtime_server',
                 'postgres_store::transcript::tests::postgres_transcript_head_recovery_read_interleaves_with_next_commit',
                 '--', '--exact'],
                ['cargo', 'test', '--locked', '-p', 'runtime_server', 'postgres_outbox', '--'],
                ['cargo', 'test', '--locked', '-p', 'runtime_server', 'postgres_wait_handoff', '--'],
                ['cargo', 'test', '--locked', '-p', 'runtime_server', 'postgres_current_schema', '--']]
    exact_tests = [
        'snapshot_cas_tests::postgres_snapshot_cas_rejects_stale_writers_and_preserves_concurrent_results',
        'postgres_transcript_concurrent_duplicate_commit_is_idempotent',
        'postgres_transcript_producer_serves_versioned_page_patch_and_deletes_derived_state',
        'postgres_runtime_store_persists_core_state_and_claims_jobs_once',
        'postgres_same_worker_reclaim_fences_old_writes_and_result_transaction',
        'postgres_worker_cancel_checks_claim_but_user_cancel_targets_job',
        'postgres_hosted_worker_reclaim_changes_claim_identity',
        'postgres_runtime_job_yield_requeues_same_job_without_queued_outbox_and_fences_old_owner',
        'postgres_subagent_projection_uses_core_durable_job_binding',
        'hosted_execution_capacity_is_shared_across_replicas_and_released_on_yield',
        'postgres_runtime_store_validates_waiter_owner_index_on_reopen',
        'postgres_runtime_store_clones_reuse_one_short_lived_connection',
        'postgres_runtime_store_pool_can_drop_inside_tokio_runtime',
        'postgres_runtime_store_pool_checkout_is_bounded',
        'postgres_runtime_store_discards_failed_and_panicked_leases_without_replay',
        'postgres_runtime_store_replaces_closed_idle_connection_before_operation',
        'postgres_control_and_listener_connections_do_not_consume_the_ordinary_pool',
        'postgres_runtime_job_wait_is_notified_and_closes_lost_wakeups',
        'postgres_session_terminal_append_fences_reclaimed_lease_owner',
        'postgres_fenced_rewrite_tombstones_completed_dispatch_request',
        'postgres_pre_admission_cancellation_is_durable_fenced_and_history_free',
        'recovery_orchestration_releases_capacity_before_followup_store_work',
        'postgres_new_user_turn_after_unpaired_tool_call_fails_admission',
        'postgres_new_user_turn_admission_commits_closure_and_run_atomically',
        'postgres_transcript_generation_rotation_switches_pointer_without_deleting_old_rows',
        'postgres_transcript_old_block_encoding_generation_is_invalidated_and_reinitialized',
    ]
    commands.extend([
        ['cargo', 'test', '--locked', '-p', 'runtime_server',
         f'postgres_store::integration_tests::{name}', '--', '--exact']
        for name in exact_tests
    ])
    names = []
    for command in commands:
        discovered = subprocess.run([*command, '--ignored', '--list'], cwd=root, env=env,
                                    capture_output=True, text=True, check=True)
        names.extend(discovered_tests(discovered.stdout))
    if len(names) != len(set(names)):
        raise RuntimeError('Runtime PostgreSQL test discovery contains duplicates')
    with psycopg.connect(**settings, autocommit=True) as admin:
        admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(database)))
        try:
            for command in commands:
                subprocess.run([*command, '--ignored', '--test-threads=1'], cwd=root, env=env, check=True)
            print(f'Runtime PostgreSQL gate: {len(names)} discovered tests passed')
        finally:
            admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(database)))


if __name__ == '__main__':
    main()

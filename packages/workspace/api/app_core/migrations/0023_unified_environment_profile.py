"""Reconstruct environment snapshots; signed grants and physical facts stay intact."""
import copy
import hashlib
import json
from pathlib import PurePosixPath
import re
import unicodedata

from django.db import migrations, models


# Freeze this migration's released input contract independently of current validators.
PROFILE_SCHEMA = 'runtime.environment_profile.v1'
PROFILE_FIELDS = {'schema', 'imageDigest', 'workspaceRoot', 'user', 'network', 'resources'}
RESOURCE_FIELDS = {'cpuMilli', 'memoryBytes', 'pidsLimit', 'dataTmpfsBytes', 'storageBytes'}


def validate_historical_profile(value):
    if not isinstance(value, dict) or set(value) != PROFILE_FIELDS or value['schema'] != PROFILE_SCHEMA:
        raise ValueError('environment_profile_schema_invalid')
    image = value['imageDigest']
    if not isinstance(image, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', image):
        raise ValueError('environment_profile_image_invalid')
    root = value['workspaceRoot']
    if (not isinstance(root, str) or not root.startswith('/') or root.startswith('//')
            or len(root.encode('utf-8')) > 4096 or any(unicodedata.category(c) == 'Cc' for c in root)
            or '\\' in root or unicodedata.normalize('NFC', root) != root
            or '..' in root.split('/') or str(PurePosixPath(root)) != root
            or root == '/run/centaeris' or root.startswith('/run/centaeris/')):
        raise ValueError('environment_profile_workspace_root_invalid')
    user = value['user']
    if not isinstance(user, str) or not re.fullmatch(r'(0|[1-9][0-9]*):(0|[1-9][0-9]*)', user):
        raise ValueError('environment_profile_user_invalid')
    if any(int(part) >= 2**32 - 1 for part in user.split(':')):
        raise ValueError('environment_profile_user_invalid')
    if value['network'] not in ('publicInternet', 'disabled'):
        raise ValueError('environment_profile_network_invalid')
    resources = value['resources']
    if not isinstance(resources, dict) or set(resources) != RESOURCE_FIELDS:
        raise ValueError('environment profile resources fields mismatch')
    for field, number in resources.items():
        maximum = 2**32 - 1 if field in ('cpuMilli', 'pidsLimit') else 2**64 - 1
        if type(number) is not int or number <= 0:
            raise ValueError('environment profile resources must be positive integers')
        if number > maximum:
            raise ValueError(f'environment profile resources.{field} exceeds transport range')



def remove_old_guards(apps, schema_editor):
    if schema_editor.connection.vendor == 'postgresql':
        schema_editor.execute('''
            DROP TRIGGER environment_network_policy ON app_core_sessionenvironment;
            DROP FUNCTION protect_environment_network();
            DROP TRIGGER benchmark_environment_profile ON app_core_sessionenvironment;
            DROP FUNCTION protect_benchmark_environment_profile();
            DROP TRIGGER benchmark_session_binding ON app_core_session;
            DROP FUNCTION protect_benchmark_session_binding();
            DROP TRIGGER benchmark_single_run ON app_core_agentrun;
            DROP FUNCTION protect_benchmark_single_run();
        ''', params=None)


def reconstruct_profiles(apps, schema_editor):
    environments = apps.get_model('app_core', 'SessionEnvironment').objects.using(schema_editor.connection.alias)
    for environment in environments.exclude(environment_profile=None).iterator():
        original = environment.environment_profile
        # This known source schema contains all six authoritative configuration fields.
        # Ordinary historical grants do not record user/root/storage and cannot supply them.
        if (not isinstance(original, dict) or original.get('schema') != 'runtime.benchmark_execution_profile.v1'):
            raise RuntimeError('historical_environment_profile_unsupported')
        old_digest = 'sha256:' + hashlib.sha256(json.dumps(original, ensure_ascii=False,
            sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        if old_digest != environment.environment_profile_digest:
            raise RuntimeError('historical_environment_profile_digest_invalid')
        current = copy.deepcopy(original)
        current['schema'] = 'runtime.environment_profile.v1'
        try:
            validate_historical_profile(current)
            digest = 'sha256:' + hashlib.sha256(json.dumps(current, ensure_ascii=False,
                sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        except (ValueError, TypeError, OverflowError):
            raise RuntimeError('historical_environment_profile_unsupported') from None
        environments.filter(pk=environment.pk).update(environment_profile=current,
            environment_profile_digest=digest,
            historical_profile={'profile': original, 'profileDigest': old_digest})


GUARDS = '''
CREATE FUNCTION protect_environment_profile() RETURNS trigger AS $$
BEGIN
    IF TG_OP='INSERT' THEN
        SELECT w.execution_network INTO NEW.network FROM app_core_workspace w
        JOIN app_core_session s ON s.workspace_id=w.id WHERE s.id=NEW.session_id;
        IF NEW.environment_profile IS NOT NULL OR NEW.historical_profile IS NOT NULL
           OR NEW.benchmark_setup_operation_id<>'' THEN
            RAISE EXCEPTION 'environment_profile_requires_binding' USING ERRCODE='23514';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP='DELETE' THEN
        IF OLD.environment_profile IS NOT NULL THEN
            RAISE EXCEPTION 'environment_profile_binding_immutable' USING ERRCODE='23514';
        END IF;
        RETURN OLD;
    END IF;
    IF NEW.historical_profile IS DISTINCT FROM OLD.historical_profile THEN
        RAISE EXCEPTION 'environment_profile_history_immutable' USING ERRCODE='23514';
    END IF;
    IF OLD.environment_profile IS NOT NULL AND
       ROW(NEW.environment_profile,NEW.environment_profile_digest,NEW.environment_profile_operation_id,NEW.environment_profile_bound_at)
       IS DISTINCT FROM ROW(OLD.environment_profile,OLD.environment_profile_digest,OLD.environment_profile_operation_id,OLD.environment_profile_bound_at) THEN
        RAISE EXCEPTION 'environment_profile_binding_immutable' USING ERRCODE='23514';
    END IF;
    IF OLD.environment_profile IS NULL AND NEW.environment_profile IS NOT NULL THEN
        PERFORM 1 FROM app_core_session WHERE id=NEW.session_id FOR UPDATE;
        IF EXISTS(SELECT 1 FROM app_core_agentrunauthorization a
                  JOIN app_core_agentrun r ON r.id=a.agent_run_id WHERE r.environment_id=NEW.id) THEN
            RAISE EXCEPTION 'environment_profile_requires_unused_session' USING ERRCODE='23514';
        END IF;
    END IF;
    IF NEW.network IS DISTINCT FROM OLD.network AND NOT
       (OLD.environment_profile IS NULL AND NEW.environment_profile IS NOT NULL) THEN
        RAISE EXCEPTION 'environment_network_immutable' USING ERRCODE='23514';
    END IF;
    IF NEW.environment_profile IS NOT NULL AND NEW.network IS DISTINCT FROM NEW.environment_profile->>'network' THEN
        RAISE EXCEPTION 'environment_network_profile_mismatch' USING ERRCODE='23514';
    END IF;
    IF ROW(NEW.benchmark_setup_operation_id,NEW.benchmark_setup_run_id,NEW.benchmark_setup_instance_id,
           NEW.benchmark_setup_container_id,NEW.benchmark_setup_completed_at)
       IS DISTINCT FROM ROW(OLD.benchmark_setup_operation_id,OLD.benchmark_setup_run_id,OLD.benchmark_setup_instance_id,
           OLD.benchmark_setup_container_id,OLD.benchmark_setup_completed_at) THEN
        RAISE EXCEPTION 'historical_benchmark_setup_immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER environment_profile_binding BEFORE INSERT OR UPDATE OR DELETE ON app_core_sessionenvironment
FOR EACH ROW EXECUTE FUNCTION protect_environment_profile();
'''


def install_guards(apps, schema_editor):
    if schema_editor.connection.vendor == 'postgresql':
        schema_editor.execute(GUARDS, params=None)


def reject_reverse(apps, schema_editor):
    raise RuntimeError('environment_profile_history_requires_forward_migration')


class Migration(migrations.Migration):
    dependencies = [('app_core', '0022_environment_maintenance_authority')]
    operations = [
        migrations.RunPython(remove_old_guards, migrations.RunPython.noop),
        migrations.RemoveConstraint('sessionenvironment', 'environment_benchmark_profile_complete'),
        migrations.RemoveConstraint('sessionenvironment', 'environment_benchmark_setup_complete'),
        *[migrations.RenameField('sessionenvironment', 'benchmark_profile' + suffix, 'environment_profile' + suffix)
          for suffix in ('', '_digest', '_operation_id', '_bound_at')],
        migrations.AddField('sessionenvironment', 'historical_profile', models.JSONField(
            null=True, default=None, blank=True, editable=False)),
        migrations.RunPython(reconstruct_profiles, reject_reverse),
        migrations.AddConstraint('sessionenvironment', models.CheckConstraint(
            condition=(models.Q(environment_profile__isnull=True, environment_profile_digest='',
                environment_profile_operation_id='', environment_profile_bound_at__isnull=True)
                | (models.Q(environment_profile__isnull=False, environment_profile_bound_at__isnull=False)
                    & ~models.Q(environment_profile_digest='') & ~models.Q(environment_profile_operation_id=''))),
            name='environment_profile_complete')),
        migrations.AddConstraint('sessionenvironment', models.CheckConstraint(
            condition=(models.Q(benchmark_setup_operation_id='', benchmark_setup_run__isnull=True,
                benchmark_setup_instance_id='', benchmark_setup_container_id='', benchmark_setup_completed_at__isnull=True)
                | (models.Q(environment_profile__isnull=False, benchmark_setup_run__isnull=False,
                            benchmark_setup_completed_at__isnull=False)
                   & ~models.Q(benchmark_setup_operation_id='') & ~models.Q(benchmark_setup_instance_id='')
                   & ~models.Q(benchmark_setup_container_id=''))), name='environment_benchmark_setup_complete')),
        migrations.RunPython(install_guards, migrations.RunPython.noop),
    ]

"""Record a canonical payload fact without changing historical signatures."""
import hashlib
import json

from django.db import migrations, models


def record_digest(apps, schema_editor):
    grants = apps.get_model('app_core', 'EnvironmentMaintenanceGrant').objects.using(schema_editor.connection.alias)
    for grant in grants.iterator():
        encoded = json.dumps(grant.payload, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()
        grants.filter(pk=grant.pk).update(digest='sha256:' + hashlib.sha256(encoded).hexdigest())


class Migration(migrations.Migration):
    dependencies = [('app_core', '0024_environment_provision_authority')]
    operations = [
        migrations.AddField('environmentmaintenancegrant', 'digest', models.CharField(max_length=71, default='', editable=False)),
        migrations.RunPython(record_digest, migrations.RunPython.noop),
        migrations.AddConstraint('environmentmaintenancegrant', models.CheckConstraint(
            condition=models.Q(digest__regex=r'^sha256:[0-9a-f]{64}$'), name='maintenance_grant_digest_valid')),
    ]

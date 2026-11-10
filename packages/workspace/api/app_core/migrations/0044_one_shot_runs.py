import app_core.models
import django.db.models.deletion
from django.conf import settings
from django.db import migrations, models


class Migration(migrations.Migration):

    dependencies = [
        ('app_core', '0043_session_archive'),
        migrations.swappable_dependency(settings.AUTH_USER_MODEL),
    ]

    operations = [
        migrations.CreateModel(
            name='OneShotCredential',
            fields=[
                ('id', models.CharField(default=app_core.models.new_one_shot_credential_id, max_length=64, primary_key=True, serialize=False)),
                ('membership_ref', models.CharField(max_length=64)),
                ('name', models.CharField(max_length=120)),
                ('pluginNames', models.JSONField(default=list)),
                ('sourceObjectRefs', models.JSONField(default=list)),
                ('secretDigest', models.CharField(max_length=64)),
                ('revokedAt', models.DateTimeField(blank=True, null=True)),
                ('createdAt', models.DateTimeField(auto_now_add=True)),
            ],
        ),
        migrations.CreateModel(
            name='OneShotOperation',
            fields=[
                ('id', models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name='ID')),
                ('operationId', models.CharField(max_length=128)),
                ('requestDigest', models.CharField(max_length=64)),
                ('agentRunId', models.CharField(max_length=64, unique=True)),
                ('sessionId', models.CharField(max_length=64, unique=True)),
                ('acceptedAt', models.DateTimeField(auto_now_add=True)),
                ('deadlineAt', models.DateTimeField()),
                ('expiresAt', models.DateTimeField()),
                ('runStatus', models.CharField(default='queued', max_length=16)),
                ('cleanupStatus', models.CharField(default='pending', max_length=16)),
                ('reasonCode', models.CharField(blank=True, default='', max_length=160)),
                ('deliveryLeaseUntil', models.DateTimeField(blank=True, null=True)),
                ('deliveryClosedAt', models.DateTimeField(blank=True, null=True)),
            ],
        ),
        migrations.AddField(
            model_name='session',
            name='purpose',
            field=models.CharField(default='interactive', max_length=16),
        ),
        migrations.AddConstraint(
            model_name='session',
            constraint=models.CheckConstraint(condition=models.Q(('purpose__in', ('interactive', 'oneShot'))), name='session_purpose_valid'),
        ),
        migrations.AddField(
            model_name='oneshotcredential',
            name='modelConfig',
            field=models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, to='app_core.modelconfig'),
        ),
        migrations.AddField(
            model_name='oneshotcredential',
            name='user',
            field=models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, to=settings.AUTH_USER_MODEL),
        ),
        migrations.AddField(
            model_name='oneshotcredential',
            name='workspace',
            field=models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, to='app_core.workspace'),
        ),
        migrations.AddField(
            model_name='oneshotoperation',
            name='credential',
            field=models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, to='app_core.oneshotcredential'),
        ),
        migrations.AddIndex(
            model_name='oneshotoperation',
            index=models.Index(fields=['cleanupStatus', 'acceptedAt'], name='one_shot_cleanup_due'),
        ),
        migrations.AddConstraint(
            model_name='oneshotoperation',
            constraint=models.UniqueConstraint(fields=('credential', 'operationId'), name='one_shot_operation_unique'),
        ),
        migrations.AddConstraint(
            model_name='oneshotoperation',
            constraint=models.CheckConstraint(condition=models.Q(('runStatus__in', ('queued', 'running', 'completed', 'failed', 'cancelled'))), name='one_shot_run_status_valid'),
        ),
        migrations.AddConstraint(
            model_name='oneshotoperation',
            constraint=models.CheckConstraint(condition=models.Q(('cleanupStatus__in', ('pending', 'cleaning', 'completed', 'failed'))), name='one_shot_cleanup_status_valid'),
        ),
    ]

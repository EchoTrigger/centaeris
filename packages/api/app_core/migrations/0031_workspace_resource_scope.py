"""Keep released unowned records platform shared; never infer a tenant or promote users.

Forward only: removing workspace scope could expose local encrypted credentials
and private packages as shared resources. Roll forward after a failed deployment.
"""

import django.db.models.deletion
from django.conf import settings
from django.db import migrations, models


class Migration(migrations.Migration):

    dependencies = [
        ('app_core', '0030_session_manual_unread'),
        migrations.swappable_dependency(settings.AUTH_USER_MODEL),
    ]

    operations = [
        migrations.CreateModel(
            name='WorkspacePluginInstallation',
            fields=[
                ('id', models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name='ID')),
                ('plugin_name', models.CharField(max_length=64)),
                ('package', models.JSONField()),
                ('updated_at', models.DateTimeField(auto_now=True)),
            ],
        ),
        migrations.RemoveConstraint(
            model_name='mcpbearercredential',
            name='mcp_bearer_credential_plugin_ref_unique',
        ),
        migrations.RemoveConstraint(
            model_name='modelprovider',
            name='model_provider_active_template_unique',
        ),
        migrations.AddField(
            model_name='mcpbearercredential',
            name='workspace',
            field=models.ForeignKey(blank=True, null=True, on_delete=django.db.models.deletion.PROTECT, related_name='mcp_credentials', to='app_core.workspace'),
        ),
        migrations.AddField(
            model_name='modelprovider',
            name='workspace',
            field=models.ForeignKey(blank=True, null=True, on_delete=django.db.models.deletion.PROTECT, related_name='model_providers', to='app_core.workspace'),
        ),
        migrations.AddConstraint(
            model_name='mcpbearercredential',
            constraint=models.UniqueConstraint(condition=models.Q(('workspace__isnull', True)), fields=('plugin_name', 'credential_ref'), name='mcp_bearer_credential_plugin_ref_unique'),
        ),
        migrations.AddConstraint(
            model_name='mcpbearercredential',
            constraint=models.UniqueConstraint(condition=models.Q(('workspace__isnull', False)), fields=('workspace', 'plugin_name', 'credential_ref'), name='workspace_mcp_credential_unique'),
        ),
        migrations.AddConstraint(
            model_name='modelprovider',
            constraint=models.UniqueConstraint(condition=models.Q(('archivedAt__isnull', True), ('template_id__isnull', False), ('workspace__isnull', True)), fields=('template_id',), name='model_provider_active_template_unique'),
        ),
        migrations.AddConstraint(
            model_name='modelprovider',
            constraint=models.UniqueConstraint(condition=models.Q(('archivedAt__isnull', True), ('template_id__isnull', False), ('workspace__isnull', False)), fields=('workspace', 'template_id'), name='workspace_provider_template_unique'),
        ),
        migrations.AddField(
            model_name='workspaceplugininstallation',
            name='updated_by',
            field=models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, to=settings.AUTH_USER_MODEL),
        ),
        migrations.AddField(
            model_name='workspaceplugininstallation',
            name='workspace',
            field=models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name='plugin_installations', to='app_core.workspace'),
        ),
        migrations.AddConstraint(
            model_name='workspaceplugininstallation',
            constraint=models.UniqueConstraint(fields=('workspace', 'plugin_name'), name='workspace_plugin_installation_unique'),
        ),
        migrations.RunPython(migrations.RunPython.noop),
    ]

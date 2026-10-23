# Forward-only snapshot publication responsibilities; historical files are not backfilled.

import django.db.models.deletion
from django.db import migrations, models


PUBLICATION_DDL = r"""
CREATE FUNCTION app_core_protect_snapshot_publication() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'snapshot publication responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.state<>'pending' OR NEW.confirmed_at IS NOT NULL
   OR NEW.request_digest !~ '^sha256:[0-9a-f]{64}$' OR NEW.purpose_key !~ '^[0-9a-f]{64}$'
   OR NEW.generation<1 OR NEW.encoding<>'frameV1'
   OR (NEW.purpose='sessionCommit' AND NEW.checkpoint_id<>'')
   OR (NEW.purpose='executionCheckpoint' AND NEW.checkpoint_id='')
  THEN RAISE EXCEPTION 'snapshot publication initial identity rejected'; END IF;
  IF NOT EXISTS(SELECT 1 FROM app_core_agentrun r JOIN app_core_agentrunauthorization a ON a.agent_run_id=r.id
    WHERE r.id=NEW.agent_run_id AND r.session_id=NEW.session_id AND a.id=NEW.authorization_id)
  THEN RAISE EXCEPTION 'snapshot publication owner mismatch'; END IF;
 ELSE
  IF ROW(NEW.id,NEW.session_id,NEW.agent_run_id,NEW.authorization_id,NEW.purpose,NEW.purpose_key,NEW.checkpoint_id,
    NEW.storage_key,NEW.request_digest,NEW.generation,NEW.encoding,NEW.snapshot_sha256,NEW.snapshot_size_bytes,
    NEW.expanded_size_bytes,NEW.file_count,NEW.created_at)
   IS DISTINCT FROM ROW(OLD.id,OLD.session_id,OLD.agent_run_id,OLD.authorization_id,OLD.purpose,OLD.purpose_key,OLD.checkpoint_id,
    OLD.storage_key,OLD.request_digest,OLD.generation,OLD.encoding,OLD.snapshot_sha256,OLD.snapshot_size_bytes,
    OLD.expanded_size_bytes,OLD.file_count,OLD.created_at)
   OR OLD.state<>'pending' OR OLD.confirmed_at IS NOT NULL OR NEW.confirmed_at IS NULL
   OR NOT ((NEW.state='staged' AND NEW.purpose='executionCheckpoint') OR (NEW.state='committed' AND NEW.purpose='sessionCommit'))
  THEN RAISE EXCEPTION 'snapshot publication immutable identity or confirmation rejected'; END IF;
  IF NEW.state='committed' AND NOT EXISTS(SELECT 1 FROM app_core_session s WHERE s.id=NEW.session_id
    AND s."workspaceGeneration"=NEW.generation AND s."workspaceStorageKey"=NEW.storage_key
    AND s."workspaceSnapshotSha256"=NEW.snapshot_sha256 AND s."workspaceSnapshotSizeBytes"=NEW.snapshot_size_bytes
    AND s."workspaceSnapshotEncoding"=NEW.encoding AND s."workspaceExpandedSizeBytes"=NEW.expanded_size_bytes
    AND s."workspaceFileCount"=NEW.file_count AND s."workspaceLastAdvancedAgentRun_id"=NEW.agent_run_id)
  THEN RAISE EXCEPTION 'snapshot publication committed head missing'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER snapshot_publication_facts BEFORE INSERT OR UPDATE OR DELETE ON app_core_workspacesnapshotpublication
 FOR EACH ROW EXECUTE FUNCTION app_core_protect_snapshot_publication();
"""


class Migration(migrations.Migration):

    dependencies = [
        ('app_core', '0026_external_identity_login'),
    ]

    operations = [
        migrations.CreateModel(
            name='WorkspaceSnapshotPublication',
            fields=[
                ('id', models.CharField(max_length=96, primary_key=True, serialize=False)),
                ('purpose', models.CharField(max_length=32)),
                ('purpose_key', models.CharField(max_length=64)),
                ('checkpoint_id', models.TextField(blank=True, default='')),
                ('storage_key', models.CharField(blank=True, db_index=True, default='', max_length=1000)),
                ('request_digest', models.CharField(max_length=71)),
                ('generation', models.PositiveBigIntegerField()),
                ('encoding', models.CharField(max_length=16)),
                ('snapshot_sha256', models.CharField(blank=True, default='', max_length=71)),
                ('snapshot_size_bytes', models.PositiveBigIntegerField()),
                ('expanded_size_bytes', models.PositiveBigIntegerField()),
                ('file_count', models.PositiveIntegerField()),
                ('state', models.CharField(default='pending', max_length=16)),
                ('created_at', models.DateTimeField(auto_now_add=True)),
                ('confirmed_at', models.DateTimeField(default=None, null=True)),
                ('agent_run', models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name='snapshot_publications', to='app_core.agentrun')),
                ('authorization', models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name='snapshot_publications', to='app_core.agentrunauthorization')),
                ('session', models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name='snapshot_publications', to='app_core.session')),
            ],
            options={
                'constraints': [models.UniqueConstraint(fields=('agent_run', 'purpose', 'purpose_key'), name='unique_snapshot_publication_purpose'), models.CheckConstraint(condition=models.Q(('purpose__in', ['sessionCommit', 'executionCheckpoint'])), name='snapshot_publication_purpose_valid'), models.CheckConstraint(condition=models.Q(('encoding', 'frameV1')), name='snapshot_publication_encoding_valid'), models.CheckConstraint(condition=models.Q(models.Q(('confirmed_at__isnull', True), ('state', 'pending')), models.Q(('confirmed_at__isnull', False), ('state__in', ['staged', 'committed'])), _connector='OR'), name='snapshot_publication_confirmation_valid')],
            },
        ),
        migrations.RunSQL(PUBLICATION_DDL),
    ]

from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0031_workspace_resource_scope")]

    operations = [
        migrations.AddField(model_name="agent", name="avatar_image",
            field=models.BinaryField(blank=True, editable=False, null=True)),
        migrations.AddField(model_name="agent", name="avatar_digest",
            field=models.CharField(blank=True, default="", editable=False, max_length=64)),
        migrations.AddConstraint(model_name="agent", constraint=models.CheckConstraint(
            condition=(models.Q(avatar_image__isnull=True, avatar_digest="")
                | (models.Q(avatar_image__isnull=False) & ~models.Q(avatar_digest=""))),
            name="agent_avatar_image_consistent")),
    ]

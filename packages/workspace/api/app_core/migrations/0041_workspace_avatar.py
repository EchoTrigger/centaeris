from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0040_question_responses")]

    operations = [
        migrations.AddField(model_name="workspace", name="avatar_image",
                            field=models.BinaryField(blank=True, null=True)),
        migrations.AddField(model_name="workspace", name="avatar_digest",
                            field=models.CharField(blank=True, default="", max_length=64)),
        migrations.AddConstraint(model_name="workspace", constraint=models.CheckConstraint(
            condition=(models.Q(avatar_image__isnull=True, avatar_digest="")
                       | models.Q(avatar_image__isnull=False, avatar_digest__regex=r"^[0-9a-f]{64}$")),
            name="workspace_avatar_image_consistent")),
    ]

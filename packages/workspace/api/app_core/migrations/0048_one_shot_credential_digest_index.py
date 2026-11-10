from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0047_one_shot_credential_copy_and_delete")]

    operations = [
        migrations.AlterField(
            model_name="oneshotcredential",
            name="secretDigest",
            field=models.CharField(max_length=64, unique=True),
        ),
    ]

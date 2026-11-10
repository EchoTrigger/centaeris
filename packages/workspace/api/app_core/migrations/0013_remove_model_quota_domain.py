from django.db import migrations


class Migration(migrations.Migration):
    dependencies = [("app_core", "0012_work_return_audit")]

    operations = [
        migrations.RemoveField(model_name="providercredential", name="quotaDomain"),
        migrations.DeleteModel(name="ModelQuotaDomain"),
    ]

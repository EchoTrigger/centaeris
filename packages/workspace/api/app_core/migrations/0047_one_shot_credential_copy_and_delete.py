from django.db import migrations, models
import django.db.models.deletion


def remove_revoked_credentials(apps, schema_editor):
    alias = schema_editor.connection.alias
    credential = apps.get_model("app_core", "OneShotCredential")
    operation = apps.get_model("app_core", "OneShotOperation")
    revoked = credential.objects.using(alias).filter(revokedAt__isnull=False)
    operation.objects.using(alias).filter(credential__in=revoked, cleanupStatus="completed").delete()
    revoked.delete()
    # Flush PostgreSQL FK triggers before altering the credential table below.
    schema_editor.execute("SET CONSTRAINTS ALL IMMEDIATE")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0046_workspace_owned_resources")]

    operations = [
        migrations.AddField(
            model_name="oneshotcredential",
            name="encryptedSecret",
            field=models.TextField(blank=True, default=""),
        ),
        migrations.AlterField(
            model_name="oneshotoperation",
            name="credential",
            field=models.ForeignKey(null=True, on_delete=django.db.models.deletion.SET_NULL, to="app_core.oneshotcredential"),
        ),
        migrations.RunPython(remove_revoked_credentials),
        migrations.RemoveField(model_name="oneshotcredential", name="revokedAt"),
    ]

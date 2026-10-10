from django.apps import AppConfig


class AppCoreConfig(AppConfig):
    default_auto_field = "django.db.models.BigAutoField"
    name = "app_core"

    def ready(self):
        from .upload_temp import initialize_upload_temp_root
        initialize_upload_temp_root()

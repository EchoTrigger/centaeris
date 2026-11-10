from .test_settings import *  # noqa: F403


# Migration drift is a schema-generation gate and must never inspect a shared DB.
DATABASES = {"default": {"ENGINE": "django.db.backends.sqlite3", "NAME": ":memory:"}}

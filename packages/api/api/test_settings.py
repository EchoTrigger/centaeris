import os
import tempfile
from pathlib import Path


os.environ.setdefault("DJANGO_SECRET_KEY", "test-secret")
os.environ.setdefault("POSTGRES_DB", "test")
os.environ.setdefault("POSTGRES_USER", "test")
os.environ.setdefault("POSTGRES_PASSWORD", "test")
os.environ.setdefault("POSTGRES_HOST", "localhost")
os.environ.setdefault("POSTGRES_PORT", "5432")
os.environ.setdefault("RUNTIME_URL", "http://runtime:9000")
os.environ.setdefault("RUNTIME_START_TIMEOUT_SECONDS", "10")
os.environ.setdefault("WEB_ORIGIN", "http://localhost:3000")
os.environ["PASSWORD_RESET_ENABLED"] = "0"
os.environ["PASSWORD_RESET_MAIL_SENDER"] = "0"
os.environ.setdefault("REDIS_URL", "redis://localhost:6379/15")
os.environ.setdefault("STORAGE_STREAM_LANES", "4")
os.environ.setdefault("STORAGE_STREAM_CHUNK_BYTES", "65536")
os.environ.setdefault("REDIS_BROWSER_MAX_CONNECTIONS", "8")
os.environ["CREDENTIAL_ENCRYPTION_KEY"] = "z5wA0vTzQGNG2LkVbNqnd3CPnGds4M8Xqy9lXgkqfZI="
os.environ["INTERNAL_API_TOKEN"] = "test-internal-token"
os.environ["AGENT_RUN_AUTHORIZATION_SIGNING_KEY"] = "test-run-authorization-signing-key"
os.environ["SANDBOX_MEMORY_BYTES"] = str(2 * 1024 * 1024 * 1024)
os.environ["SANDBOX_CPU_MILLI"] = "2000"
os.environ["SANDBOX_PIDS_LIMIT"] = "512"
os.environ["SANDBOX_DATA_TMPFS_BYTES"] = str(4 * 1024 * 1024 * 1024)
os.environ.setdefault("STORAGE_ROOT", tempfile.mkdtemp(prefix="centaeris-workspace-agent-test-"))
os.environ["TEST_POSTGRES_DB"] = os.environ.get("TEST_POSTGRES_DB", "centaeris")
os.environ["TEST_POSTGRES_USER"] = os.environ.get("TEST_POSTGRES_USER", "centaeris")
os.environ["TEST_POSTGRES_PASSWORD"] = os.environ.get("TEST_POSTGRES_PASSWORD", "centaeris")
os.environ["TEST_POSTGRES_HOST"] = os.environ.get("TEST_POSTGRES_HOST", "localhost")
os.environ["TEST_POSTGRES_PORT"] = os.environ.get("TEST_POSTGRES_PORT", "55432")
os.environ["PLUGIN_CATALOG_ROOT"] = str(
    Path(__file__).resolve().parents[1] / "app_core" / "testdata" / "plugins"
)

from .settings import *  # noqa: F403


DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.postgresql",
        "NAME": os.environ["TEST_POSTGRES_DB"],
        "USER": os.environ["TEST_POSTGRES_USER"],
        "PASSWORD": os.environ["TEST_POSTGRES_PASSWORD"],
        "HOST": os.environ["TEST_POSTGRES_HOST"],
        "PORT": os.environ["TEST_POSTGRES_PORT"],
        "CONN_MAX_AGE": 0,
    }
}

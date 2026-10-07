import os
from pathlib import Path
from urllib.parse import urlsplit

from cryptography.fernet import Fernet


BASE_DIR = Path(__file__).resolve().parent.parent


def required_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"Missing required environment variable: {name}")
    return value


def boolean_env(name: str, default: str = "0") -> bool:
    value = os.environ.get(name, default)
    if value not in {"0", "1"}:
        raise RuntimeError(f"{name} must be 0 or 1")
    return value == "1"


def positive_capacity_env(name, default):
    value = os.environ.get(name, str(default))
    if not value.isascii() or not value.isdecimal() or not 1 <= int(value) <= 1_000_000:
        raise RuntimeError(f"{name} must be an integer between 1 and 1000000")
    return int(value)


EXECUTION_GLOBAL_QUEUE_LIMIT = positive_capacity_env("EXECUTION_GLOBAL_QUEUE_LIMIT", 128)
EXECUTION_TENANT_QUEUE_LIMIT = positive_capacity_env("EXECUTION_TENANT_QUEUE_LIMIT", 32)
EXECUTION_QUEUE_WAIT_SECONDS = positive_capacity_env("EXECUTION_QUEUE_WAIT_SECONDS", 300)
RUNTIME_CONTROL_TIMEOUT_SECONDS = positive_capacity_env("RUNTIME_HTTP_CONTROL_TIMEOUT_SECONDS", 5)


SECRET_KEY = required_env("DJANGO_SECRET_KEY")
DEBUG = os.environ.get("DJANGO_DEBUG") == "1"
ALLOWED_HOSTS = os.environ.get("DJANGO_ALLOWED_HOSTS", "localhost,127.0.0.1").split(",")

INSTALLED_APPS = [
    "corsheaders",
    "django.contrib.auth",
    "django.contrib.contenttypes",
    "django.contrib.sessions",
    "django.contrib.messages",
    "django.contrib.staticfiles",
    "app_core",
]

MIDDLEWARE = [
    "django.middleware.security.SecurityMiddleware",
    "corsheaders.middleware.CorsMiddleware",
    "django.contrib.sessions.middleware.SessionMiddleware",
    "django.middleware.common.CommonMiddleware",
    "django.middleware.csrf.CsrfViewMiddleware",
    "django.contrib.auth.middleware.AuthenticationMiddleware",
    "django.contrib.messages.middleware.MessageMiddleware",
]

ROOT_URLCONF = "api.urls"

TEMPLATES = [
    {
        "BACKEND": "django.template.backends.django.DjangoTemplates",
        "DIRS": [],
        "APP_DIRS": True,
        "OPTIONS": {
            "context_processors": [
                "django.template.context_processors.request",
                "django.contrib.auth.context_processors.auth",
                "django.contrib.messages.context_processors.messages",
            ],
        },
    },
]

ASGI_APPLICATION = "api.asgi.application"

_postgres_pool_size = os.environ.get("API_POSTGRES_POOL_MAX_SIZE", "0")
if not _postgres_pool_size.isascii() or not _postgres_pool_size.isdecimal() or not 0 <= int(_postgres_pool_size) <= 32:
    raise RuntimeError("API_POSTGRES_POOL_MAX_SIZE must be an integer between 0 and 32")

_postgres_options = {
    "application_name": os.environ.get("POSTGRES_APPLICATION_NAME", "centaeris-api"),
}
if int(_postgres_pool_size):
    _postgres_options["pool"] = {
        "min_size": 0,
        "max_size": int(_postgres_pool_size),
        "timeout": 5,
    }

DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.postgresql",
        "NAME": required_env("POSTGRES_DB"),
        "USER": required_env("POSTGRES_USER"),
        "PASSWORD": required_env("POSTGRES_PASSWORD"),
        "HOST": required_env("POSTGRES_HOST"),
        "PORT": required_env("POSTGRES_PORT"),
        "CONN_MAX_AGE": 0,
        "OPTIONS": _postgres_options,
    }
}

LANGUAGE_CODE = "en-us"
TIME_ZONE = "UTC"
USE_I18N = True
USE_TZ = True

STATIC_URL = "static/"
MEDIA_ROOT = required_env("STORAGE_ROOT")
DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"

AUTH_PASSWORD_VALIDATORS = [
    {"NAME": "django.contrib.auth.password_validation.UserAttributeSimilarityValidator"},
    {"NAME": "django.contrib.auth.password_validation.MinimumLengthValidator", "OPTIONS": {"min_length": 15}},
    {"NAME": "django.contrib.auth.password_validation.CommonPasswordValidator"},
    {"NAME": "django.contrib.auth.password_validation.NumericPasswordValidator"},
]

PASSWORD_HASHERS = ["django.contrib.auth.hashers.Argon2PasswordHasher"]
SESSION_COOKIE_HTTPONLY = True
SESSION_COOKIE_SAMESITE = "Lax"
SESSION_COOKIE_SECURE = not DEBUG
SESSION_ENGINE = "app_core.persistent_sessions"
# Cookie retention is a browser transport policy, not a server login deadline.
SESSION_COOKIE_AGE = 400 * 24 * 60 * 60
SESSION_EXPIRE_AT_BROWSER_CLOSE = False
SESSION_SAVE_EVERY_REQUEST = True
CSRF_COOKIE_HTTPONLY = False
CSRF_COOKIE_SECURE = not DEBUG
SECURE_PROXY_SSL_HEADER = ("HTTP_X_FORWARDED_PROTO", "https")
RUNTIME_URL = required_env("RUNTIME_URL")
WEB_ORIGIN = required_env("WEB_ORIGIN")
_extra_web_origins = os.environ.get("WEB_EXTRA_ORIGINS", "")
_web_origins = [WEB_ORIGIN, *(_extra_web_origins.split(",") if _extra_web_origins else [])]
for _origin in _web_origins:
    _url = urlsplit(_origin)
    if (_url.scheme not in {"http", "https"} or not _url.hostname or "*" in _origin
            or _url.username is not None or _url.password is not None
            or _url.path or _url.query or _url.fragment or _origin.strip() != _origin):
        raise RuntimeError("Web origins must be explicit http(s) origins without paths or credentials")
CSRF_TRUSTED_ORIGINS = list(dict.fromkeys(_web_origins))
CORS_ALLOWED_ORIGINS = CSRF_TRUSTED_ORIGINS.copy()
CORS_ALLOW_CREDENTIALS = True
CORS_ALLOW_HEADERS = ["Content-Type", "Last-Event-ID", "X-CSRFToken"]
CORS_ALLOW_METHODS = ["DELETE", "GET", "PATCH", "POST", "PUT", "OPTIONS"]
INTERNAL_API_TOKEN = required_env("INTERNAL_API_TOKEN")
PLATFORM_MCP_ALLOWED_HOSTS = os.environ.get("PLATFORM_MCP_ALLOWED_HOSTS", "localhost,127.0.0.1").split(",")
AGENT_RUN_AUTHORIZATION_SIGNING_KEY = required_env("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
SANDBOX_MEMORY_BYTES = int(required_env("SANDBOX_MEMORY_BYTES"))
SANDBOX_CPU_MILLI = int(required_env("SANDBOX_CPU_MILLI"))
SANDBOX_PIDS_LIMIT = int(required_env("SANDBOX_PIDS_LIMIT"))
SANDBOX_DATA_TMPFS_BYTES = int(required_env("SANDBOX_DATA_TMPFS_BYTES"))
RUNTIME_START_TIMEOUT_SECONDS = int(required_env("RUNTIME_START_TIMEOUT_SECONDS"))
MODEL_PROVIDER_TIMEOUT_SECONDS = int(os.environ.get("MODEL_PROVIDER_TIMEOUT_SECONDS", "120"))
CREDENTIAL_ENCRYPTION_KEY = required_env("CREDENTIAL_ENCRYPTION_KEY")
try:
    Fernet(CREDENTIAL_ENCRYPTION_KEY.encode("ascii"))
except (UnicodeEncodeError, ValueError) as error:
    raise RuntimeError("CREDENTIAL_ENCRYPTION_KEY must be a valid Fernet key") from error
REDIS_URL = required_env("REDIS_URL")
PLUGIN_CATALOG_ROOT = required_env("PLUGIN_CATALOG_ROOT")


def required_bounded_positive_int(name: str, *, maximum: int) -> int:
    raw_value = required_env(name)
    try:
        value = int(raw_value)
    except ValueError as error:
        raise RuntimeError(f"{name} must be a positive integer") from error
    if value <= 0:
        raise RuntimeError(f"{name} must be a positive integer")
    if value > maximum:
        raise RuntimeError(f"{name} must be at most {maximum}")
    return value


STORAGE_STREAM_LANES = required_bounded_positive_int(
    "STORAGE_STREAM_LANES",
    maximum=256,
)
STORAGE_STREAM_CHUNK_BYTES = required_bounded_positive_int(
    "STORAGE_STREAM_CHUNK_BYTES",
    maximum=1024 * 1024,
)
REDIS_BROWSER_MAX_CONNECTIONS = required_bounded_positive_int(
    "REDIS_BROWSER_MAX_CONNECTIONS",
    maximum=4096,
)
if REDIS_BROWSER_MAX_CONNECTIONS < 2:
    raise RuntimeError("REDIS_BROWSER_MAX_CONNECTIONS must be at least 2")

# Sync snapshot/cursor checks are short but can burst; reserve at most 1/8 of
# the worker budget (capped at 32) and leave the rest for long-lived SSE reads.
REDIS_BROWSER_SYNC_MAX_CONNECTIONS = max(
    1,
    min(32, REDIS_BROWSER_MAX_CONNECTIONS // 8),
)
REDIS_BROWSER_LIVE_MAX_CONNECTIONS = (
    REDIS_BROWSER_MAX_CONNECTIONS - REDIS_BROWSER_SYNC_MAX_CONNECTIONS
)

PASSWORD_RESET_ENABLED = boolean_env("PASSWORD_RESET_ENABLED")
PASSWORD_RESET_TIMEOUT = int(os.environ.get("PASSWORD_RESET_TIMEOUT_SECONDS", "86400"))
if not 60 <= PASSWORD_RESET_TIMEOUT <= 7 * 24 * 60 * 60:
    raise RuntimeError(
        "PASSWORD_RESET_TIMEOUT_SECONDS must be between 60 and 604800"
    )
if PASSWORD_RESET_ENABLED and not DEBUG and not WEB_ORIGIN.startswith("https://"):
    raise RuntimeError("WEB_ORIGIN must use https when password reset is enabled")

PASSWORD_RESET_MAIL_SENDER = boolean_env("PASSWORD_RESET_MAIL_SENDER")
EMAIL_BACKEND = "django.core.mail.backends.smtp.EmailBackend"
EMAIL_HOST = os.environ.get("EMAIL_HOST", "")
EMAIL_PORT = int(os.environ.get("EMAIL_PORT", "587"))
EMAIL_HOST_USER = os.environ.get("EMAIL_HOST_USER", "")
EMAIL_HOST_PASSWORD = os.environ.get("EMAIL_HOST_PASSWORD", "")
EMAIL_USE_TLS = boolean_env("EMAIL_USE_TLS", "1")
EMAIL_USE_SSL = boolean_env("EMAIL_USE_SSL")
EMAIL_TIMEOUT = int(os.environ.get("EMAIL_TIMEOUT_SECONDS", "15"))
DEFAULT_FROM_EMAIL = os.environ.get("DEFAULT_FROM_EMAIL", "")

if PASSWORD_RESET_ENABLED and PASSWORD_RESET_MAIL_SENDER:
    if not EMAIL_HOST or not DEFAULT_FROM_EMAIL:
        raise RuntimeError(
            "EMAIL_HOST and DEFAULT_FROM_EMAIL are required for password reset mail"
        )
    if not 1 <= EMAIL_PORT <= 65535:
        raise RuntimeError("EMAIL_PORT must be between 1 and 65535")
    if not 1 <= EMAIL_TIMEOUT <= 60:
        raise RuntimeError("EMAIL_TIMEOUT_SECONDS must be between 1 and 60")
    if EMAIL_USE_TLS == EMAIL_USE_SSL:
        raise RuntimeError("exactly one of EMAIL_USE_TLS or EMAIL_USE_SSL must be 1")
    if bool(EMAIL_HOST_USER) != bool(EMAIL_HOST_PASSWORD):
        raise RuntimeError(
            "EMAIL_HOST_USER and EMAIL_HOST_PASSWORD must be configured together"
        )

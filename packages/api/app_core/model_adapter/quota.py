"""PostgreSQL admission for actual hosted provider attempts.

Each attempt owns a session advisory lock until its response or stream closes.
PostgreSQL releases the lock if the API process or connection dies.
"""

import asyncio
import time
from contextlib import asynccontextmanager, contextmanager
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime

import psycopg
from asgiref.sync import sync_to_async
from django.db import connection
from django.db.models.functions import Greatest

from ..models import ModelConfig, ModelQuotaDomain, ProviderCredential
from .common import ModelProviderError


POLL_SECONDS = 0.05


def _database_now_ms() -> int:
    with connection.cursor() as cursor:
        cursor.execute("SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint")
        return cursor.fetchone()[0]


def _quota_domain_key(model: ModelConfig) -> int:
    credential = ProviderCredential.objects.filter(provider_id=model.provider_id).first()
    if credential is None or credential.quotaDomain_id is None:
        raise ModelProviderError("model_quota_domain_required")
    return credential.quotaDomain_id


def _domain_state(key: int) -> tuple[int, float]:
    try:
        domain = ModelQuotaDomain.objects.get(pk=key)
    except ModelQuotaDomain.DoesNotExist as error:
        raise ModelProviderError("model_quota_domain_required") from error
    if not domain.enabled:
        raise ModelProviderError("model_quota_domain_disabled")
    return domain.maxConcurrent, domain.cooldownUntilMs / 1000


def _connection_params() -> dict:
    if connection.vendor != "postgresql":
        raise ModelProviderError("model_quota_backend_unavailable")
    # This connection holds a session advisory lock across the provider attempt.
    # It must remain separate from Django's reusable ORM pool.
    return connection.get_connection_params() | {
        "connect_timeout": 5,
        "application_name": "centaeris-api-quota",
    }


def _try_lock(conn, key: int, limit: int) -> int:
    with conn.cursor() as cursor:
        for candidate in range(1, limit + 1):
            cursor.execute("SELECT pg_try_advisory_lock(%s, %s)", (-key, candidate))
            if cursor.fetchone()[0]:
                return candidate
    return 0


def _unlock(conn, key: int, slot: int) -> None:
    with conn.cursor() as cursor:
        cursor.execute("SELECT pg_advisory_unlock(%s, %s)", (-key, slot))


async def _in_thread(operation, *args, **kwargs):
    # Finish the database operation before cancellation closes its connection.
    task = asyncio.create_task(asyncio.to_thread(operation, *args, **kwargs))
    try:
        return await asyncio.shield(task)
    except asyncio.CancelledError:
        await _finish_thread_task(task)
        raise


async def _finish_thread_task(task):
    while True:
        try:
            return await asyncio.shield(task)
        except asyncio.CancelledError:
            if task.done():
                return task.result()


async def _open_connection(params: dict):
    task = asyncio.create_task(asyncio.to_thread(psycopg.connect, **params, autocommit=True))
    try:
        return await asyncio.shield(task)
    except asyncio.CancelledError:
        conn = await _finish_thread_task(task)
        await _in_thread(conn.close)
        raise


def _retry_after_delay(value: str | None) -> float | None:
    if not value:
        return None
    value = value.strip()
    if value.isascii() and value.isdigit():
        try:
            seconds = int(value)
        except ValueError:
            return None
    else:
        try:
            date = parsedate_to_datetime(value)
            if date.tzinfo is None:
                return None
            seconds = (
                date - datetime.fromtimestamp(_database_now_ms() / 1000, timezone.utc)
            ).total_seconds()
        except (TypeError, ValueError, OverflowError):
            return None
    seconds = max(seconds, 0)
    if seconds > (2**63 - 1 - _database_now_ms()) / 1000:
        return None
    return seconds


def _observe_error(key: int, error: ModelProviderError) -> None:
    if error.httpStatus not in {408, 429} and (
        error.httpStatus is None or error.httpStatus < 500
    ):
        return
    delay = _retry_after_delay(error.retryAfter)
    if delay is None:
        return
    ModelQuotaDomain.objects.filter(pk=key).update(
        cooldownUntilMs=Greatest("cooldownUntilMs", _database_now_ms() + int(delay * 1000))
    )


@contextmanager
def model_attempt(model: ModelConfig, cancel_event=None):
    key = _quota_domain_key(model)
    if cancel_event is not None and cancel_event.is_set():
        raise ModelProviderError("model_run_cancelled")
    conn = psycopg.connect(**_connection_params(), autocommit=True)
    try:
        slot = 0
        while True:
            if cancel_event is not None and cancel_event.is_set():
                raise ModelProviderError("model_run_cancelled")
            limit, cooldown_until = _domain_state(key)
            if _database_now_ms() / 1000 >= cooldown_until:
                slot = _try_lock(conn, key, limit)
            if slot:
                _, cooldown_until = _domain_state(key)
                if _database_now_ms() / 1000 >= cooldown_until and (
                    cancel_event is None or not cancel_event.is_set()
                ):
                    break
                _unlock(conn, key, slot)
                slot = 0
            time.sleep(POLL_SECONDS)
        try:
            yield
        except ModelProviderError as error:
            _observe_error(key, error)
            raise
    finally:
        conn.close()


@asynccontextmanager
async def async_model_attempt(model: ModelConfig):
    key = await sync_to_async(_quota_domain_key, thread_sensitive=True)(model)
    params = await sync_to_async(_connection_params, thread_sensitive=True)()
    conn = await _open_connection(params)
    try:
        slot = 0
        while True:
            limit, cooldown_until = await sync_to_async(_domain_state, thread_sensitive=True)(key)
            current_ms = await sync_to_async(_database_now_ms, thread_sensitive=True)()
            if current_ms / 1000 >= cooldown_until:
                slot = await _in_thread(_try_lock, conn, key, limit)
            if slot:
                _, cooldown_until = await sync_to_async(_domain_state, thread_sensitive=True)(key)
                current_ms = await sync_to_async(_database_now_ms, thread_sensitive=True)()
                if current_ms / 1000 >= cooldown_until:
                    break
                await _in_thread(_unlock, conn, key, slot)
                slot = 0
            await asyncio.sleep(POLL_SECONDS)
        try:
            yield
        except ModelProviderError as error:
            await sync_to_async(_observe_error, thread_sensitive=True)(key, error)
            raise
    finally:
        await _in_thread(conn.close)

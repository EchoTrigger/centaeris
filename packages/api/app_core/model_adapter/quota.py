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
from .common import ModelProviderError, model_database_operation


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
    if limit <= 0:
        return 0
    with conn.cursor() as cursor:
        if limit == 1:
            cursor.execute("SELECT pg_try_advisory_lock(%s, %s)", (-key, 1))
            return 1 if cursor.fetchone()[0] else 0
        # Stop recursion after the first success: LIMIT over volatile lock calls
        # could acquire additional slots before the planner limits the result.
        cursor.execute("""
            WITH RECURSIVE attempts(slot, acquired) AS (
                SELECT 0, false
                UNION ALL
                SELECT slot + 1, pg_try_advisory_lock(%s, slot + 1)
                FROM attempts
                WHERE NOT acquired AND slot < %s
            )
            SELECT COALESCE(MAX(slot) FILTER (WHERE acquired), 0)
            FROM attempts
        """, (-key, limit))
        return cursor.fetchone()[0]


def _unlock(conn, key: int, slot: int) -> None:
    with conn.cursor() as cursor:
        cursor.execute("SELECT pg_advisory_unlock(%s, %s)", (-key, slot))


def _slot_may_be_available(key: int, limit: int) -> bool:
    if limit <= 0:
        return False
    # This snapshot only avoids opening a dedicated connection when all slots
    # appear occupied. A fresh advisory-lock attempt still owns admission.
    with connection.cursor() as cursor:
        cursor.execute("""
            SELECT COUNT(DISTINCT objid) < %s
            FROM pg_locks
            WHERE locktype = 'advisory'
              AND database = (
                  SELECT oid FROM pg_database WHERE datname = current_database()
              )
              AND classid::bigint = %s
              AND objsubid = 2
              AND granted
              AND mode = 'ExclusiveLock'
              AND objid::bigint BETWEEN 1 AND %s
        """, (limit, (-key) & 0xffffffff, limit))
        return cursor.fetchone()[0]


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


@model_database_operation
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


@model_database_operation
def _attempt_configuration(model, cancel_event=None):
    key = _quota_domain_key(model)
    if cancel_event is not None and cancel_event.is_set():
        raise ModelProviderError("model_run_cancelled")
    return key, _connection_params()


@model_database_operation
def _poll_admission(key):
    limit, cooldown_until = _domain_state(key)
    available = (
        _database_now_ms() / 1000 >= cooldown_until
        and _slot_may_be_available(key, limit)
    )
    return limit, available


@model_database_operation
def _cooldown_has_ended(key):
    _, cooldown_until = _domain_state(key)
    return _database_now_ms() / 1000 >= cooldown_until


@contextmanager
def model_attempt(model: ModelConfig, cancel_event=None):
    key, params = _attempt_configuration(model, cancel_event)
    conn = None
    try:
        slot = 0
        while True:
            if cancel_event is not None and cancel_event.is_set():
                raise ModelProviderError("model_run_cancelled")
            limit, available = _poll_admission(key)
            if available:
                if cancel_event is not None and cancel_event.is_set():
                    raise ModelProviderError("model_run_cancelled")
                conn = psycopg.connect(**params, autocommit=True)
                slot = _try_lock(conn, key, limit)
            if slot:
                if _cooldown_has_ended(key) and (
                    cancel_event is None or not cancel_event.is_set()
                ):
                    break
                slot = 0
            if conn is not None:
                conn.close()
                conn = None
            time.sleep(POLL_SECONDS)
        try:
            yield
        except ModelProviderError as error:
            _observe_error(key, error)
            raise
    finally:
        if conn is not None:
            conn.close()


@asynccontextmanager
async def async_model_attempt(model: ModelConfig):
    key, params = await sync_to_async(_attempt_configuration, thread_sensitive=True)(model)
    conn = None
    try:
        slot = 0
        while True:
            limit, available = await sync_to_async(_poll_admission, thread_sensitive=True)(key)
            if available:
                conn = await _open_connection(params)
                slot = await _in_thread(_try_lock, conn, key, limit)
            if slot:
                if await sync_to_async(_cooldown_has_ended, thread_sensitive=True)(key):
                    break
                slot = 0
            if conn is not None:
                await _in_thread(conn.close)
                conn = None
            await asyncio.sleep(POLL_SECONDS)
        try:
            yield
        except ModelProviderError as error:
            await sync_to_async(_observe_error, thread_sensitive=True)(key, error)
            raise
    finally:
        if conn is not None:
            await _in_thread(conn.close)

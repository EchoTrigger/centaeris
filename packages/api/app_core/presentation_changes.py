"""Persistent scoped dirtiness for shared presentation readers.

PostgreSQL triggers append scoped dirtiness in the source transaction, including
writes from Runtime. A bounded relay advances versions and consumes the outbox
atomically. NOTIFY carries only a key and is a hint; lost wakeups remain pending.
These display versions never replace transcript water, read acknowledgements or
authorization checks. Permission checks remain independent of display invalidation.
"""
from django.db import connections, transaction

from .models import PresentationChange, PresentationChangeOutbox


CHANNEL = "workspace_presentation_changed_v1"
PROJECTED_CHANNEL = "workspace_presentation_projected_v1"


def _identity(value):
    if not isinstance(value, str) or not value or len(value) > 64:
        raise ValueError("presentation_change_identity_invalid")
    return value


def agent_change_key(agent_id, kind):
    if kind not in {"navigation", "history", "work"}:
        raise ValueError("presentation_change_kind_invalid")
    return f"agent:{_identity(agent_id)}:{kind}"


def session_change_key(session_id, kind):
    if kind not in {"state", "context"}:
        raise ValueError("presentation_change_kind_invalid")
    return f"session:{_identity(session_id)}:{kind}"


def presentation_change_keys(workspace_id, agent_id, session_id=None):
    # Agent and Session IDs are globally unique; workspace remains part of the
    # caller's authorized binding rather than a broad content invalidation key.
    _identity(workspace_id)
    keys = {kind: agent_change_key(agent_id, kind) for kind in ("navigation", "history", "work")}
    if session_id is not None:
        keys.update({kind: session_change_key(session_id, kind) for kind in ("state", "context")})
    return keys


def read_presentation_versions(workspace_id, agent_id, session_id=None, *, using="default"):
    if connections[using].vendor != "postgresql":
        raise RuntimeError("presentation_changes_require_postgresql")
    keys = presentation_change_keys(workspace_id, agent_id, session_id)
    positions = dict(PresentationChange.objects.using(using).filter(key__in=keys.values()).values_list("key", "revision"))
    return {kind: positions.get(keys.get(kind), 0)
            for kind in ("navigation", "history", "work", "state", "context")}


def drain_pending_changes(limit=256, *, using="default"):
    """Project one bounded batch without locking versions inside source writes.

    Each source transaction owns distinct outbox rows. Relays lock pending rows
    with SKIP LOCKED and update version keys in one stable order, then consume the
    same rows atomically. No source operation waits for a relay-owned version row.
    Remaining rows are retained until a later relay or explicit maintenance run.
    """
    if connections[using].vendor != "postgresql":
        raise RuntimeError("presentation_changes_require_postgresql")
    if type(limit) is not int or not 1 <= limit <= 4096:
        raise ValueError("presentation_change_batch_invalid")
    with transaction.atomic(using=using):
        with connections[using].cursor() as cursor:
            cursor.execute("SET LOCAL statement_timeout='5s'")
        pending = list(PresentationChangeOutbox.objects.using(using).select_for_update(skip_locked=True)
            .order_by("id").values_list("id", "key")[:limit])
        if not pending:
            return 0
        keys = sorted({key for _identity, key in pending})
        with connections[using].cursor() as cursor:
            cursor.execute('INSERT INTO app_core_presentationchange(key,revision) '
                'SELECT key,1 FROM unnest(%s::text[]) AS changes(key) ORDER BY key '
                'ON CONFLICT(key) DO UPDATE SET revision=app_core_presentationchange.revision+1', [keys])
        PresentationChangeOutbox.objects.using(using).filter(pk__in=[identity for identity, _key in pending]).delete()
        with connections[using].cursor() as cursor:
            # A peer may have skipped our locked outbox rows and read old versions.
            # Commit publishes visibility without making this a new drain request.
            cursor.execute('SELECT pg_notify(%s,key) FROM unnest(%s::text[]) AS changes(key)',
                [PROJECTED_CHANNEL, keys])
        return len(pending)

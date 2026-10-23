"""Owner-scoped notification projections over authoritative Session records.

A transport disconnect never changes AgentRun state. These watermarks describe
what needs the owner's attention and what the owner has actually observed.
"""
from django.db.models import F, Max, Q

from .models import Session, SessionEvent


MAX_SEQUENCE = 9_223_372_036_854_775_807
ATTENTION_EVENT_TYPES = ("agent_run_completed", "agent_run_failed", "agent_run_interrupted")


def attention_event_filter():
    # checkpoint_ref is a canonical Core record, including when it is deliberately
    # absent from the former per-run stream. It does not mean the question was answered.
    return Q(payload__type__in=ATTENTION_EVENT_TYPES) | Q(
        payload__type="checkpoint_ref", payload__payload__status="paused_question",
    )


def durable_session_high_water(session):
    return SessionEvent.objects.filter(session_id=session.pk).aggregate(value=Max("sequence"))["value"] or 0


def refresh_session_attention(session, *, through_sequence=None):
    events = SessionEvent.objects.filter(session_id=session.pk, sequence__gt=session.latestAttentionSequence)
    if through_sequence is not None:
        events = events.filter(sequence__lte=through_sequence)
    latest = events.filter(attention_event_filter()).aggregate(value=Max("sequence"))["value"]
    _apply_latest_attention(session, latest)
    return session


def refresh_session_attention_many(sessions):
    """Discover sidebar attention in one grouped query, independent of list size."""
    if not sessions:
        return
    latest = dict(SessionEvent.objects.filter(session_id__in=[session.pk for session in sessions])
        .filter(attention_event_filter()).values("session_id")
        .annotate(attention=Max("sequence")).values_list("session_id", "attention"))
    for session in sessions:
        _apply_latest_attention(session, latest.get(session.pk))


def _apply_latest_attention(session, latest):
    if latest is not None and latest > session.latestAttentionSequence:
        # A replay may rediscover the same fact. It cannot move the notification
        # backwards, touch read state, or change recency. No UPDATE occurs in the
        # ordinary no-new-attention path.
        Session.objects.filter(pk=session.pk, latestAttentionSequence__lt=latest).update(
            latestAttentionSequence=latest, attentionRevision=F("attentionRevision") + 1,
        )
        # A stale in-memory Session may rediscover a fact already projected by a
        # different writer. Return the persisted revision and owner read state.
        session.refresh_from_db(fields=["latestAttentionSequence", "readThroughSequence", "isUnread", "attentionRevision"])


def session_attention_snapshot(session):
    return {
        "attentionRevision": str(session.attentionRevision),
        "latestAttentionSequence": str(session.latestAttentionSequence),
        "readThroughSequence": str(session.readThroughSequence),
        "isManuallyUnread": session.isUnread,
        "isUnread": session.isUnread or session.latestAttentionSequence > session.readThroughSequence,
    }


def acknowledge_session_read(session, through_sequence):
    """Apply an observed cursor while the caller holds the Session row lock.

    Automatic acknowledgements never clear an explicit manual-unread marker.
    The durable upper bound prevents a client from acknowledging unseen future facts.
    """
    if type(through_sequence) is not int or not 0 <= through_sequence <= MAX_SEQUENCE:
        raise ValueError("session_read_sequence_invalid")
    if through_sequence > durable_session_high_water(session):
        raise ValueError("session_read_sequence_ahead")
    refresh_session_attention(session)
    if through_sequence > session.readThroughSequence:
        session.readThroughSequence = through_sequence
        session.attentionRevision += 1
        session.save(update_fields=["readThroughSequence", "attentionRevision"])
    return session


def set_manual_session_unread(session, unread):
    """Apply an explicit owner action while the caller holds the Session row lock."""
    refresh_session_attention(session)
    through = session.readThroughSequence if unread else max(
        session.readThroughSequence, durable_session_high_water(session),
    )
    if session.isUnread != unread or session.readThroughSequence != through:
        session.isUnread = unread
        session.readThroughSequence = through
        session.attentionRevision += 1
        session.save(update_fields=["isUnread", "readThroughSequence", "attentionRevision"])
    return session
